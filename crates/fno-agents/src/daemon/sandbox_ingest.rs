//! The sandbox-denial ingester: codex transcripts hold runtime permission
//! refusals that no event reader could see, so every cause count for them
//! was a hand scan. One pass per interval: walk the codex rollout store,
//! read only the tool-output payloads (a doc or message quoting a denial
//! is prose, not a denial - substring counting overstated them for weeks),
//! and emit one `sandbox_denial` row per (session, class, path) with a hit
//! count. The claude half of the shape rides another node; the `harness`
//! field is the seam.
//!
//! Evidence discipline: the runtime refusals carry their errno
//! (`Operation not permitted (os error 1)`, `Read-only file system
//! (os error 30)`, SQLite's own "readonly database" sentence). Matching
//! the errno-bearing shape keeps quoted prose out: a doc line inside a
//! message payload is not a denial.

use super::*;
use crate::codex_store::codex_sessions;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How long between passes. Denials are forensics, not freshness: the
/// stalls report reads weekly windows, so discovery lag of an hour costs
/// nothing and a quiet machine pays a bounded walk.
pub(crate) const SANDBOX_INGEST_INTERVAL_SECS: i64 = 3_600;

/// Newest transcripts only: a rollout whose mtime leaves this window is
/// older than the report windows the stalls command reads by default, and its
/// denials (if any) were already ingested when it was fresh.
const SCAN_WINDOW_DAYS: u64 = 8;

/// Newest-N cap per pass: the walk is bounded even on a fleet-sized store.
const MAX_FILES_PER_PASS: usize = 200;

/// Byte cap per file per pass. Rollouts are append-only and the pass reads
/// from the remembered offset, so the cap bounds a first contact with a
/// huge historical transcript; the rest waits for the next pass.
const MAX_SCAN_BYTES_PER_FILE: u64 = 4 * 1024 * 1024;

/// The per-pass offset memory: transcript path -> next byte to read. A
/// file whose offset memory was reset by a truncation rescans from zero.
type IngestOffsets = HashMap<String, u64>;

/// One aggregated refusal shape inside one transcript.
struct DenialBucket {
    class: &'static str,
    rule: &'static str,
    path: String,
}

/// (rule, the marker literal). `readonly_database` keys on SQLite's own
/// sentence; the other two key on the errno-suffixed runtime shape, so
/// quoted prose without an errno never counts.
const DENIAL_RULES: &[(&str, &str)] = &[
    ("readonly_database", "readonly database"),
    ("operation_not_permitted", "Operation not permitted"),
    ("read_only_file_system", "Read-only file system"),
];

/// What a (rule, path) refusal is. Matching the hand analysis's buckets:
/// canonical git writes, claim locks, the read-only backlog store, codex
/// home config, and blocked commands with no path at all.
fn denial_class(rule: &str, path: &str) -> &'static str {
    if rule == "readonly_database" {
        return "readonly_db";
    }
    if path.contains("/.git/") || path.ends_with("/.git") {
        return "git_write";
    }
    if path.ends_with(".lock") {
        return "claim_lock";
    }
    if path.contains("/.codex/") {
        return "home_config";
    }
    if path.is_empty() {
        return "daemon_io";
    }
    "other"
}

/// One pass. Returns the rows emitted so the caller's event row carries a
/// real count, never a fabricated zero.
pub(crate) fn sandbox_ingest(home: &AgentsHome, emitter: &EventEmitter, now: i64) -> usize {
    let stamp = home.root().join("sandbox-ingest.stamp");
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    if now.saturating_sub(last) < SANDBOX_INGEST_INTERVAL_SECS {
        return 0;
    }
    // A dispatch pause suspends the pass without consuming its cadence: no
    // scan, no stamp write, one positive skip row so intentional silence
    // cannot read as a dead arm (the sibling sweeps' shape).
    let pause = crate::loops_pause::dispatch_pause();
    if pause.is_paused() {
        let _ = emitter.emit(
            "sandbox_ingest",
            &json!({
                "outcome": "skipped",
                "reason": pause.skip_reason(),
            }),
        );
        return 0;
    }
    let state_path = home.root().join("sandbox-ingest-state.json");
    let mut offsets: IngestOffsets = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut rows_emitted = 0usize;
    let mut files_scanned = 0usize;
    let mut denials = 0usize;
    let files = codex_sessions(None, SCAN_WINDOW_DAYS);
    for file in files.into_iter().take(MAX_FILES_PER_PASS) {
        let key = file.path.display().to_string();
        let mut offset = offsets.get(&key).copied().unwrap_or(0);
        if offset > file.size {
            offset = 0;
        }
        let (buckets, scanned) = scan_transcript(&file.path, offset);
        let new_offset = offset + scanned;
        files_scanned += 1;
        for (bucket, hits) in buckets {
            let _ = emitter.emit(
                "sandbox_denial",
                &json!({
                    "harness": "codex",
                    "session_id": file.session_id.clone(),
                    "class": bucket.class,
                    "rule": bucket.rule,
                    "path": bucket.path,
                    "hits": hits,
                    "transcript": key.clone(),
                }),
            );
            rows_emitted += 1;
            denials += hits;
        }
        offsets.insert(key, new_offset);
    }

    let _ = emitter.emit(
        "sandbox_ingest",
        &json!({
            "outcome": "ok",
            "files_scanned": files_scanned,
            "denials": denials,
            "rows": rows_emitted,
        }),
    );
    if let Ok(body) = serde_json::to_string(&offsets) {
        let _ = std::fs::write(&state_path, body);
    }
    let _ = std::fs::write(&stamp, now.to_string());
    rows_emitted
}

/// Scan one transcript from `offset`, aggregating refusals into
/// (class, rule, path) buckets, one `sandbox_denial` row each. Returns the
/// buckets plus the byte count actually scanned: the offset only advances
/// over complete lines, and a short read advances by what it read, never
/// by what it was offered.
fn scan_transcript(path: &Path, offset: u64) -> (Vec<(DenialBucket, usize)>, u64) {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (Vec::new(), 0),
    };
    let end = file.metadata().map(|m| m.len()).unwrap_or(0);
    if offset >= end {
        return (Vec::new(), 0);
    }
    let read_len = (end - offset).min(MAX_SCAN_BYTES_PER_FILE);
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return (Vec::new(), 0);
    }
    let mut buf = vec![0u8; read_len as usize];
    let mut scanned = 0usize;
    if file.read_exact(&mut buf).is_ok() {
        scanned = read_len as usize;
    }
    // Slice to the last complete line: a tail cut mid-line is re-read by
    // the next pass from the offset this pass records.
    let line_end = buf[..scanned]
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    scanned = line_end;
    let text = String::from_utf8_lossy(&buf[..scanned]);

    let mut order: Vec<String> = Vec::new();
    let mut buckets: HashMap<String, (DenialBucket, usize)> = HashMap::new();
    for line in text.lines() {
        for (rule, path) in output_denial_markers(line) {
            let key = format!("{}|{}", rule, path);
            let entry = buckets.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                (
                    DenialBucket {
                        class: denial_class(rule, &path),
                        rule,
                        path,
                    },
                    0,
                )
            });
            entry.1 += 1;
        }
    }
    let scanned64 = scanned as u64;
    (
        order
            .into_iter()
            .map(|k| buckets.remove(&k).expect("inserted above"))
            .collect(),
        scanned64,
    )
}

/// The denial markers in one rollout line, each as (rule, refused path).
/// Only tool-output payload shapes are read: a doc or message quoting the
/// marker is prose, and counting prose is how the hand analysis overstated
/// its numbers into admitted upper bounds.
fn output_denial_markers(line: &str) -> Vec<(&'static str, String)> {
    let envelope: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let payload = match envelope.get("payload") {
        Some(p) => p,
        None => return Vec::new(),
    };
    let output = match envelope.get("type").and_then(|t| t.as_str()) {
        Some("response_item") => {
            let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match ptype {
                "function_call_output" | "custom_tool_call_output" => {
                    payload.get("output").and_then(|o| o.as_str())
                }
                _ => None,
            }
        }
        Some("event_msg") => {
            let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match ptype {
                "item_completed" => payload
                    .get("item")
                    .and_then(|i| i.get("output"))
                    .and_then(|o| o.as_str()),
                _ => None,
            }
        }
        _ => None,
    };
    let Some(output) = output else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &(rule, marker) in DENIAL_RULES.iter() {
        let mut from = 0usize;
        while let Some(at) = output[from..].find(marker) {
            let hit = from + at;
            from = hit + marker.len();
            out.push((rule, refused_path_around(output, hit).unwrap_or_default()));
        }
    }
    out
}

/// The refused path around a marker hit: the path-shaped token ending at
/// the marker's lead, after a colon, a backtick, or "cannot write ". A hit
/// with no path on either side stays empty and the row still counts.
fn refused_path_around(output: &str, hit: usize) -> Option<String> {
    let head = &output[..hit];
    let trimmed = head.trim_end_matches([' ', ':', '`', '"']);
    let token: String = trimmed
        .chars()
        .rev()
        .take_while(|c| is_path_char(*c))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    token.contains('/').then_some(token)
}

fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_with_output(output: &str) -> String {
        json!({
            "timestamp": "2026-10-06T05:29:59Z",
            "type": "response_item",
            "payload": {"type": "function_call_output", "output": output},
        })
        .to_string()
    }

    #[test]
    fn sandbox_denial_markers_classes_and_offsets_follow_their_contracts() {
        // Marker table: (output, want rule, want path). Only tool-output
        // payload shapes count; a message payload quoting the marker is
        // prose, the false-positive shape the hand analysis counted.
        for (output, want_rule, want_path) in [
            (
                "error: cannot write /Users/me/repo/.git/index.lock: Operation not permitted (os error 1)",
                "operation_not_permitted",
                "/Users/me/repo/.git/index.lock",
            ),
            ("ps failed to run: Operation not permitted (os error 1)", "operation_not_permitted", ""),
        ] {
            let line = line_with_output(output);
            let markers = output_denial_markers(&line);
            assert_eq!(markers.len(), 1, "output {output:?}");
            assert_eq!(markers[0].0, want_rule);
            assert_eq!(markers[0].1, want_path);
        }
        let prose = json!({
            "timestamp": "2026-10-06T05:29:59Z",
            "type": "response_item",
            "payload": {"type": "message", "content": "Operation not permitted is a real errno"},
        })
        .to_string();
        assert!(output_denial_markers(&prose).is_empty());
        // Class buckets: (rule, path -> class), matching the hand
        // analysis's buckets.
        for (rule, path, want) in [
            (
                "operation_not_permitted",
                "/repo/.git/FETCH_HEAD",
                "git_write",
            ),
            (
                "operation_not_permitted",
                "/space/worktrees/w1/claim-a1b2.lock",
                "claim_lock",
            ),
            (
                "operation_not_permitted",
                "/Users/me/.codex/agents/w.toml",
                "home_config",
            ),
            ("operation_not_permitted", "", "daemon_io"),
            ("operation_not_permitted", "/srv/data.csv", "other"),
        ] {
            assert_eq!(denial_class(rule, path), want, "path {path:?}");
        }
        let readonly = line_with_output(
            "sqlite error: attempt to write a readonly database at /Users/me/.fno/graph.db",
        );
        let markers = output_denial_markers(&readonly);
        assert_eq!(markers[0].0, "readonly_database");
        assert_eq!(denial_class(markers[0].0, &markers[0].1), "readonly_db");
        // Offset contract: the pass advances only over complete lines, so
        // a partial tail is re-read by the next pass and advances nothing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("rollout-2026-10-06T00-00-00-0f0e1d2c-3b4a-4958-8675-3092f4c1b2a3.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{{\"partial",
                line_with_output("ps failed to run: Operation not permitted (os error 1)")
            ),
        )
        .unwrap();
        let (buckets, scanned) = scan_transcript(&path, 0);
        assert_eq!(buckets.len(), 1);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(scanned as usize, text.rfind('\n').unwrap() + 1);
        let (again, rescanned) = scan_transcript(&path, scanned);
        assert!(again.is_empty());
        assert_eq!(rescanned, 0);
    }
}
