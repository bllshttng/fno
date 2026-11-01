//! Typed corrections become golden drafts. A daily daemon pass reads new
//! `agent_corrected` rows after a watermark and writes one draft per new case
//! to `<state>/evals/golden-drafts/`. A person reviews a draft into
//! `evals/bank`. A draft never reaches the bank by itself, because a bad
//! expected answer would hold merges with nobody having read it.
//!
//! The folder's dot files are the pass's memory: `.watermark` holds the last
//! event seq, `.seen` one hash per case, and `.expired` one hash per expired
//! draft. `.seen` outlives a draft, so an expired case never comes back.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::event_store::{query_events, EventQuery};

const DAILY: Duration = Duration::from_secs(86_400);
const EXPIRE_AFTER: Duration = Duration::from_secs(30 * 86_400);

/// The correction id and quote of a person's typed correction. An interrupt
/// carries no words to draft from, and a scrubbed quote has lost its case.
pub(crate) fn typed_correction(row: &Value) -> Option<(&str, &str)> {
    let text = |key: &str| row.get(key).and_then(Value::as_str);
    if text("corrected_by")? != "user"
        || text("kind")? == "interrupt"
        || text("quote_status")? != "kept"
    {
        return None;
    }
    let quote = text("quote")?;
    (!quote.trim().is_empty()).then_some((text("correction_id")?, quote))
}

fn case_hash(quote: &str) -> String {
    let flat = quote
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    blake3::hash(flat.as_bytes()).to_hex()[..12].to_string()
}

fn draft_body(hash: &str, id: &str, quote: &str, row: &Value) -> String {
    let draft = json!({
        "id": format!("golden-draft-{hash}"),
        "draft_status": "needs_review",
        "tier": "capability",
        "prompt": "",
        "grade": [],
        "tags": ["golden"],
        "golden": {
            "source": {"kind": "correction", "ref": id},
            "quote": quote,
            "expected": "",
            "rule": "",
            "failure_type": "",
            "outcome": "failed",
        },
        "context": {
            "session": row.get("session_id"),
            "harness": row.get("harness"),
            "kind": row.get("kind"),
            "ts_turn": row.get("ts_turn"),
            "node": row.pointer("/node/value"),
            "head_sha": row.pointer("/head_sha/value"),
        },
    });
    serde_yaml_ng::to_string(&draft).unwrap_or_default()
}

fn lines_of(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn expire(dir: &Path, now: SystemTime) -> Result<(), String> {
    for path in std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
    {
        if path.extension().map_or(true, |e| e != "yaml") {
            continue;
        }
        let old = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= EXPIRE_AFTER);
        if old {
            let hash = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            append_line(&dir.join(".expired"), &hash)?;
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// One pass. A store read that fails moves nothing, so the next pass retries.
/// A draft lands before its `.seen` line, so a crash rewrites the same file
/// and never loses a case.
pub(crate) fn pass(journal: &Path, dir: &Path, now: SystemTime) -> Result<Value, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mark = dir.join(".watermark");
    let after = std::fs::read_to_string(&mark)
        .ok()
        .and_then(|t| t.trim().parse::<i64>().ok());
    let query = EventQuery {
        types: vec!["agent_corrected".to_string()],
        after_seq: Some(after.unwrap_or(0)),
        ..Default::default()
    };
    let rows = query_events(journal, &query)?;
    let seen_path = dir.join(".seen");
    let mut seen = lines_of(&seen_path);
    let mut new = 0u64;
    let mut last = after;
    for event in &rows {
        last = last.max(Some(event.seq));
        let Ok(line) = serde_json::from_str::<Value>(&event.line) else {
            continue;
        };
        let data = &line["data"];
        let Some((id, quote)) = typed_correction(data) else {
            continue;
        };
        let hash = case_hash(quote);
        if !seen.insert(hash.clone()) {
            continue;
        }
        let path = dir.join(format!("{hash}.yaml"));
        std::fs::write(&path, draft_body(&hash, id, quote, data))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        append_line(&seen_path, &hash)?;
        new += 1;
    }
    if let Some(seq) = last {
        std::fs::write(&mark, seq.to_string()).map_err(|e| e.to_string())?;
    }
    expire(dir, now)?;
    let open = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "yaml"))
        .count();
    Ok(json!({
        "new": new,
        "open": open,
        "expired": lines_of(&dir.join(".expired")).len(),
    }))
}

fn drafts_dir(state: &Path) -> PathBuf {
    state.join("evals").join("golden-drafts")
}

/// `fno-agents evals drafts`: run the pass now and print its counts.
pub fn run(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("usage: fno-agents evals drafts");
        return 2;
    }
    let state = crate::evals_candidates::state_root();
    match pass(
        &state.join("events.jsonl"),
        &drafts_dir(&state),
        SystemTime::now(),
    ) {
        Ok(receipt) => {
            println!("{receipt}");
            0
        }
        Err(e) => {
            eprintln!("evals drafts: {e}");
            1
        }
    }
}

#[derive(Default)]
pub struct Arm {
    last: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

/// The fleet tick's entry: once a day, one pass in flight, off the core loop.
/// A daemon restart runs one extra pass, which the watermark makes harmless.
pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    {
        let mut last = arm.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < DAILY) || arm.in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let Some(state) = home.root().parent().map(Path::to_path_buf) else {
            return;
        };
        let journal = state.join("events.jsonl");
        if let Err(e) = pass(&journal, &drafts_dir(&state), SystemTime::now()) {
            eprintln!("golden drafts: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn correction(id: &str, by: &str, kind: &str, quote: &str) -> Value {
        json!({
            "ts": "2026-11-01T00:00:00Z", "type": "agent_corrected",
            "source": "daemon", "session_id": "s1",
            "data": {"correction_id": id, "corrected_by": by, "kind": kind,
                     "quote": quote, "quote_status": "kept", "session_id": "s1"},
        })
    }

    #[test]
    fn a_new_correction_drafts_once_and_an_expired_case_never_returns() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("events.jsonl");
        let dir = tmp.path().join("drafts");
        for (n, row) in [
            correction("corr:1", "user", "correct", "no, use the worktree"),
            correction("corr:2", "user", "correct", "No,  use the WORKTREE"),
            correction(
                "corr:3",
                "user",
                "interrupt",
                "[Request interrupted by user]",
            ),
            correction("corr:4", "machine", "correct", "a gate said no"),
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("corr:{n}");
            crate::event_store::append_envelope(&journal, &row.to_string(), Some(&id)).unwrap();
        }
        let now = SystemTime::now();

        let first = pass(&journal, &dir, now).unwrap();
        assert_eq!(first, json!({"new": 1, "open": 1, "expired": 0}));
        let hash = case_hash("no, use the worktree");
        let body = std::fs::read_to_string(dir.join(format!("{hash}.yaml"))).unwrap();
        assert!(
            body.contains("corr:1") && body.contains("needs_review"),
            "{body}"
        );

        let second = pass(&journal, &dir, now).unwrap();
        assert_eq!(second, json!({"new": 0, "open": 1, "expired": 0}));

        let later = now + EXPIRE_AFTER + Duration::from_secs(60);
        let expired = pass(&journal, &dir, later).unwrap();
        assert_eq!(expired, json!({"new": 0, "open": 0, "expired": 1}));

        std::fs::remove_file(dir.join(".watermark")).unwrap();
        let replay = pass(&journal, &dir, later).unwrap();
        assert_eq!(
            replay,
            json!({"new": 0, "open": 0, "expired": 1}),
            "a lost watermark redrafts nothing"
        );
    }
}
