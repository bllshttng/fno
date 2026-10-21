//! The sideline activity ramp's source: incremental tool-call counting over
//! session transcripts. One pass per daemon tick (5s ceiling) reads only the
//! bytes appended since the last pass - a per-session fold holds the byte
//! offset at the last complete line, so a multi-megabyte transcript costs a
//! read of its tail, never a re-read of the whole file. A daemon restart
//! re-reads its transcripts once, the same trade [`RunningCost`] makes.
//!
//! Counts land on the registry row (the same channel the context readings
//! ride, stamp-gated on the server's 1s poll); the fold itself stays
//! in-memory, keyed by `harness_session_id` (law d-e952ed19).
//!
//! A failed call is read the way each transcript states it: a claude
//! `tool_result` carries its own `is_error` flag; a codex rollout has no
//! flag, so the refusal buckets that price `refusal_rate` read the output
//! text - the same read the lead check-in uses.

use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// The per-session fold: byte offset plus the cumulative counts.
#[derive(Default)]
pub(crate) struct ToolFold {
    pub(crate) offset: u64,
    pub(crate) calls: u64,
    pub(crate) errors: u64,
    /// The last pass that counted this fold: the prune's clock.
    last_seen: Option<Instant>,
    /// The transcript this fold counts, kept so a pass never walks the
    /// store for a session it already found.
    path: Option<PathBuf>,
}

impl ToolFold {
    /// Absorb the bytes appended to `path` since the last call. Only whole
    /// newline-terminated lines are consumed; a partial tail stays for the
    /// next pass. A file shorter than the remembered offset (rotated,
    /// replaced) resets the fold and recounts once.
    pub(crate) fn absorb(&mut self, path: &Path, harness: &str) {
        let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else {
            return;
        };
        if len < self.offset {
            *self = Self {
                path: self.path.take(),
                ..Self::default()
            };
        }
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        let mut reader = std::io::BufReader::new(file);
        if reader.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(n) => {
                    if line.last() != Some(&b'\n') {
                        break; // partial tail stays for the next pass
                    }
                    self.offset += n as u64;
                    if let Ok(text) = std::str::from_utf8(&line) {
                        self.row_line(text.trim_end_matches('\n'), harness);
                    }
                }
                Err(_) => break,
            }
        }
    }

    fn row_line(&mut self, line: &str, harness: &str) {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        if harness == "codex" {
            self.codex_row(&row);
        } else {
            self.claude_row(&row);
        }
    }

    /// A claude transcript row: tool_use blocks are calls; a tool_result
    /// with the row's own `is_error` flag is a failure.
    fn claude_row(&mut self, row: &serde_json::Value) {
        let Some(content) = row.pointer("/message/content").and_then(|v| v.as_array()) else {
            return;
        };
        for block in content {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("tool_use") => self.calls += 1,
                Some("tool_result") => {
                    if block.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
                        self.errors += 1;
                    }
                }
                _ => {}
            }
        }
    }

    /// A codex rollout row: call rows are calls; an output whose text
    /// matches a refusal bucket is a failure (no structural flag exists).
    fn codex_row(&mut self, row: &serde_json::Value) {
        let Some(payload) = row.get("payload") else {
            return;
        };
        match payload.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "function_call" | "custom_tool_call" | "local_shell_call" => self.calls += 1,
            "custom_tool_call_output" | "function_call_output" => {
                let text = crate::session_activity::codex_output_text(payload);
                if crate::refusal_rate::buckets_of(&text).next().is_some() {
                    self.errors += 1;
                }
            }
            _ => {}
        }
    }
}

static FOLDS: OnceLock<Mutex<HashMap<String, ToolFold>>> = OnceLock::new();

/// Scan every counted row once: absorb appends, write the cumulative pair
/// onto the registry row. Runs off the daemon loop on the blocking pool.
/// The store walk and the reads run against a snapshot; the write lock is
/// taken only to stamp the counts.
fn scan_pass(home: &crate::paths::AgentsHome) {
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return;
    };
    let rows: Vec<(String, String)> = registry
        .entries
        .iter()
        .filter(|e| e.status != crate::AgentStatus::Exited)
        .filter(|e| matches!(e.harness_name(), "claude" | "codex"))
        .filter_map(|e| Some((e.harness_session_id.clone()?, e.harness_name().to_string())))
        .collect();
    let transcripts = crate::context_run::SessionTranscripts::default();
    let mut folds = match FOLDS.get_or_init(Default::default).lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let counts = count_rows(&rows, &mut folds, &mut |sid: &str, harness: &str| {
        transcripts.find(sid, harness)
    });
    drop(folds);
    if counts.is_empty() {
        return;
    }
    let _ = crate::state::update_registry(&home.registry_json(), |registry| {
        for entry in &mut registry.entries {
            let Some(sid) = entry.harness_session_id.as_deref() else {
                continue;
            };
            if let Some((calls, errors)) = counts.get(sid) {
                entry.tool_calls = Some(*calls);
                entry.tool_errors = Some(*errors);
            }
        }
    });
}

/// Absorb each `(session, harness)` row into its fold and return the counts
/// by session. A fold keeps its resolved transcript path, so `find` runs only
/// for a new session or one whose file is gone.
fn count_rows(
    rows: &[(String, String)],
    folds: &mut HashMap<String, ToolFold>,
    find: &mut dyn FnMut(&str, &str) -> Option<PathBuf>,
) -> HashMap<String, (u64, u64)> {
    let now = Instant::now();
    let mut counts = HashMap::new();
    for (sid, harness) in rows {
        let fold = folds.entry(sid.clone()).or_default();
        let path = match fold.path.clone().filter(|p| p.exists()) {
            Some(path) => path,
            None => {
                let Some(found) = find(sid, harness) else {
                    continue;
                };
                // Another file under the same session: its offset means
                // nothing there, so the fold recounts it.
                if fold.path.as_ref() != Some(&found) {
                    *fold = ToolFold::default();
                    fold.path = Some(found.clone());
                }
                found
            }
        };
        fold.absorb(&path, harness);
        fold.last_seen = Some(now);
        counts.insert(sid.clone(), (fold.calls, fold.errors));
    }
    // A fold whose row stopped counting (exited, reaped, transcript gone)
    // prunes after a day, the same retention the cost folds keep.
    folds.retain(|sid, fold| {
        counts.contains_key(sid)
            || fold
                .last_seen
                .map_or(true, |t| now.duration_since(t) < FOLD_TTL)
    });
    counts
}

/// One day: the fold retention the cost folds keep.
const FOLD_TTL: std::time::Duration = std::time::Duration::from_secs(86_400);

/// The daemon tick's tool-scan arm: throttled to [`SCAN_CADENCE`], one
/// in-flight pass behind `in_flight`, run off the loop. Same shape as
/// `liveness_sweep::maybe_sweep`.
pub(crate) fn maybe_scan(
    last: &mut Instant,
    in_flight: &std::sync::Arc<AtomicBool>,
    home: crate::paths::AgentsHome,
) {
    if last.elapsed() < SCAN_CADENCE || in_flight.swap(true, Ordering::SeqCst) {
        return;
    }
    *last = Instant::now();
    let flag = std::sync::Arc::clone(in_flight);
    tokio::task::spawn_blocking(move || {
        scan_pass(&home);
        flag.store(false, Ordering::SeqCst);
    });
}

/// The counting ceiling the user set: at most one parse pass per 5s.
const SCAN_CADENCE: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;

    const CALL: &str = r#"{"message":{"content":[{"type":"tool_use"}]}}"#;

    fn rows() -> Vec<(String, String)> {
        vec![("s1".to_string(), "claude".to_string())]
    }

    /// AC4: a fold whose cached path still exists is counted without a
    /// store lookup.
    #[test]
    fn a_cached_path_skips_the_store_lookup() {
        let dir = std::env::temp_dir().join(format!("tool-scan-hit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s1.jsonl");
        std::fs::write(&path, format!("{CALL}\n")).unwrap();
        let mut folds = HashMap::new();
        let mut finds = 0;
        let mut find = |_: &str, _: &str| {
            finds += 1;
            Some(path.clone())
        };
        count_rows(&rows(), &mut folds, &mut find);
        let counts = count_rows(&rows(), &mut folds, &mut find);
        assert_eq!(finds, 1, "the second pass reuses the path");
        assert_eq!(counts["s1"], (1, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AC4: a cached path that was removed resolves again, and the counts
    /// come from the new file.
    #[test]
    fn a_removed_path_resolves_again_and_recounts() {
        let dir = std::env::temp_dir().join(format!("tool-scan-gone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old.jsonl");
        let new = dir.join("new.jsonl");
        std::fs::write(&old, format!("{CALL}\n{CALL}\n")).unwrap();
        let mut folds = HashMap::new();
        let mut finds = 0;
        let mut find = |_: &str, _: &str| {
            finds += 1;
            Some(if finds == 1 { old.clone() } else { new.clone() })
        };
        assert_eq!(count_rows(&rows(), &mut folds, &mut find)["s1"], (2, 0));
        std::fs::remove_file(&old).unwrap();
        std::fs::write(&new, format!("{CALL}\n")).unwrap();
        assert_eq!(count_rows(&rows(), &mut folds, &mut find)["s1"], (1, 0));
        assert_eq!(finds, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
