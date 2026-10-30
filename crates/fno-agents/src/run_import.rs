//! The ledger history imports once: the daemon migrate pass reads the
//! machine's `ledger.json` and every space-file ledger, and emits each row
//! no `run_recorded` carrier covers as a durable `run_imported` event, then
//! one `run_history_imported` marker with the count and the earliest run
//! start. The file keeps being written until its retirement slice; this pass
//! only copies history into the store, it never moves or unlinks the file.
//!
//! Gates: a root whose store holds no carrier row yet refuses (the import
//! must not run ahead of the deployed writer, or rows after the import would
//! have no event); a root whose store already holds the marker is silent.
//! A carrier that lands between the coverage read and the emits is harmless
//! at read time: the run-row fold drops an imported row whose key a carrier
//! row covers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::event_store::{append_envelope, query_events, EventQuery};
use crate::state_layout::Status;

/// Import this machine's ledger history into the store, once. The return
/// feeds the migrate receipt: one row naming the gate that refused, the
/// pending count on a dry run, or `merged` after the pass.
pub fn ledger_history_import(root: &Path, stamp: &str, apply: bool) -> Vec<(String, Status)> {
    let journal = root.join("events.jsonl");
    let marker = EventQuery::of_types(&["run_history_imported"]);
    if query_events(&journal, &marker)
        .map(|rows| !rows.is_empty())
        .unwrap_or(false)
    {
        return Vec::new();
    }
    let carrier = EventQuery::of_types(&["run_recorded"]);
    let carrier_keys: HashSet<String> = query_events(&journal, &carrier)
        .unwrap_or_default()
        .iter()
        .filter_map(|row| run_row(&row.line))
        .filter_map(|run| crate::event_store::runs::run_key(&run))
        .collect();
    if carrier_keys.is_empty() {
        return vec![(
            "run-history".to_string(),
            Status::Refused(
                "no run_recorded carrier row in the store yet; the import waits for the deployed writer"
                    .to_string(),
            ),
        )];
    }
    let mut seen = carrier_keys;
    let mut imported: Vec<Value> = Vec::new();
    let mut refused: Vec<(String, Status)> = Vec::new();
    for file in ledger_files(root) {
        match read_entries(&file) {
            Ok(entries) => {
                for row in entries {
                    let Some(key) = crate::event_store::runs::run_key(&row) else {
                        continue;
                    };
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.insert(key);
                    imported.push(row);
                }
            }
            Err(e) => refused.push((
                file.strip_prefix(root)
                    .unwrap_or(&file)
                    .display()
                    .to_string(),
                Status::Refused(e),
            )),
        }
    }
    if imported.is_empty() {
        return refused;
    }
    if !apply {
        refused.push((
            "run-history".to_string(),
            Status::Pending(format!("import {} rows", imported.len())),
        ));
        return refused;
    }
    for row in &imported {
        append_run_event(&journal, "run_imported", json_run(row));
    }
    append_run_event(
        &journal,
        "run_history_imported",
        serde_json::json!({
            "count": imported.len(),
            "earliest_started": earliest_started(&imported, stamp),
            "imported_at": stamp,
        }),
    );
    refused.push(("run-history".to_string(), Status::Merged));
    refused
}

/// The ledger files this machine holds: the global ledger, then every
/// space-file ledger sorted for a stable emit order.
fn ledger_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![root.join("ledger.json")];
    let spaces = root.join("spaces");
    let mut space_files: Vec<PathBuf> = std::fs::read_dir(&spaces)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join("ledger.json"))
        .filter(|path| path.is_file())
        .collect();
    space_files.sort();
    files.extend(space_files);
    files.retain(|path| path.is_file());
    files
}

/// The `entries` rows of one ledger file; a file without that shape refuses
/// by name instead of silently dropping its rows.
fn read_entries(file: &Path) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let data: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {e}", file.display()))?;
    Ok(data
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// The `data.run` object of one stored envelope line.
fn run_row(line: &str) -> Option<Value> {
    let parsed: Value = serde_json::from_str(line).ok()?;
    parsed
        .pointer("/data/run")
        .filter(|v| v.is_object())
        .cloned()
}

fn json_run(row: &Value) -> Value {
    serde_json::json!({ "run": row })
}

/// One durable `migration` envelope into the global journal. Best-effort,
/// like every store append on the migrate pass: a failed emit retries on the
/// next pass, because the marker only lands after every row's own emit.
fn append_run_event(journal: &Path, kind: &str, data: Value) {
    let event = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        "type": kind,
        "source": "migration",
        "data": data,
    });
    let line = serde_json::to_string(&event).unwrap_or_default();
    if line.is_empty() {
        eprintln!("{kind}: serialize failed; the import retries on the next pass");
        return;
    }
    if let Err(e) = append_envelope(journal, &line, None) {
        eprintln!("{kind}: emit to {} failed: {e}", journal.display());
    }
}

/// The earliest run start the import covers: `started` where a row carries
/// one, else `completed` (the reconcile-backstop rows carry only that). Both
/// are UTC, so the first 19 characters order them.
fn earliest_started(rows: &[Value], fallback: &str) -> String {
    rows.iter()
        .filter_map(|row| {
            ["started", "completed"]
                .iter()
                .find_map(|field| row.get(*field).and_then(Value::as_str))
        })
        .map(|ts| ts.get(..19).unwrap_or(ts).to_string())
        .min()
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// One carrier row for `session`, appended through the judge so the
    /// fixture matches what the deployed finalize writes.
    fn append_carrier(journal: &Path, session: &str, started: &str) {
        let event = json!({
            "ts": "2026-10-30T12:00:00Z",
            "type": "run_recorded",
            "source": "target",
            "data": {"run": {
                "type": "execution",
                "fno_id": session,
                "session_id": session,
                "started": started,
                "cost_usd": 0.5,
            }},
        });
        append_envelope(journal, &event.to_string(), None).unwrap();
    }

    fn write_ledger(path: &Path, entries: Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, json!({"entries": entries}).to_string()).unwrap();
    }

    fn stored(journal: &Path, kind: &str) -> Vec<Value> {
        query_events(journal, &EventQuery::of_types(&[kind]))
            .unwrap()
            .iter()
            .filter_map(|row| serde_json::from_str::<Value>(&row.line).ok())
            .collect()
    }

    #[test]
    fn refuses_until_a_carrier_row_exists() {
        let root = TempDir::new().unwrap();
        let out = ledger_history_import(root.path(), "2026-10-30T20:30:00Z", true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "run-history");
        let Status::Refused(reason) = &out[0].1 else {
            panic!("expected a refusal, got {:?}", out[0].1);
        };
        assert!(reason.contains("carrier"), "reason: {reason}");
        // The silent twin: a root that already holds the marker reports
        // nothing, imported or not.
        append_carrier(
            &root.path().join("events.jsonl"),
            "sess-a",
            "2026-10-01T10:00:00Z",
        );
        append_run_event(
            &root.path().join("events.jsonl"),
            "run_history_imported",
            json!({"count": 0, "earliest_started": "2026-10-01T10:00:00Z", "imported_at": "2026-10-30T20:30:00Z"}),
        );
        write_ledger(
            &root.path().join("ledger.json"),
            json!([
                {"session_id": "sess-b", "started": "2026-10-05T10:00:00Z"},
            ]),
        );
        assert!(ledger_history_import(root.path(), "2026-10-30T21:00:00Z", true).is_empty());
        assert_eq!(
            stored(&root.path().join("events.jsonl"), "run_imported").len(),
            0
        );
    }

    #[test]
    fn imports_uncovered_rows_once() {
        let root = TempDir::new().unwrap();
        let journal = root.path().join("events.jsonl");
        append_carrier(&journal, "sess-a", "2026-10-04T10:00:00Z");
        write_ledger(
            &root.path().join("ledger.json"),
            json!([
                // Covered by the carrier: never imported.
                {"fno_id": "sess-a", "session_id": "sess-a", "started": "2026-10-04T10:00:00Z"},
                // A plain uncovered row: imported under session_id.
                {"session_id": "sess-b", "started": "2026-10-05T10:00:00Z"},
                // A backstop row: keyed by node and PR, no cost.
                {"graph_node_id": "x-node", "pr_number": 7, "completed": "2026-10-06T11:00:00+00:00", "cost_usd": null},
            ]),
        );
        let out = ledger_history_import(root.path(), "2026-10-30T20:30:00Z", true);
        assert_eq!(out, vec![("run-history".to_string(), Status::Merged)]);
        let imported = stored(&journal, "run_imported");
        assert_eq!(imported.len(), 2);
        assert_eq!(imported[0]["run"]["session_id"], json!("sess-b"));
        assert_eq!(imported[1]["run"]["pr_number"], json!(7));
        let markers = stored(&journal, "run_history_imported");
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0]["data"]["count"], json!(2));
        assert_eq!(
            markers[0]["data"]["earliest_started"],
            json!("2026-10-05T10:00:00Z")
        );
        // The fold answers the carrier plus the two imported rows.
        let runs = crate::event_store::runs::query_runs(&journal, &Default::default()).unwrap();
        assert_eq!(runs.len(), 3);
        // The second pass emits nothing.
        assert!(ledger_history_import(root.path(), "2026-10-30T21:00:00Z", true).is_empty());
        assert_eq!(stored(&journal, "run_imported").len(), 2);
    }

    #[test]
    fn reads_space_files_and_refuses_a_bad_file_by_name() {
        let root = TempDir::new().unwrap();
        let journal = root.path().join("events.jsonl");
        append_carrier(&journal, "sess-a", "2026-10-01T10:00:00Z");
        write_ledger(
            &root.path().join("ledger.json"),
            json!([{"fno_id": "sess-a", "session_id": "sess-a"}]),
        );
        write_ledger(
            &root
                .path()
                .join("spaces")
                .join("-Users-bb16-code-one")
                .join("ledger.json"),
            json!([{"session_id": "sess-b", "started": "2026-10-05T10:00:00Z"}]),
        );
        std::fs::create_dir_all(root.path().join("spaces").join("-Users-bb16-code-two")).unwrap();
        std::fs::write(
            root.path()
                .join("spaces")
                .join("-Users-bb16-code-two")
                .join("ledger.json"),
            "not json",
        )
        .unwrap();
        let out = ledger_history_import(root.path(), "2026-10-30T20:30:00Z", true);
        // The bad file refuses by its root-relative name; the good rows land.
        assert!(out.iter().any(
            |(name, status)| name == "spaces/-Users-bb16-code-two/ledger.json"
                && matches!(status, Status::Refused(_))
        ));
        assert!(out.iter().any(|(name, _)| name == "run-history"));
        assert_eq!(stored(&journal, "run_imported").len(), 1);
        assert_eq!(
            stored(&journal, "run_imported")[0]["run"]["session_id"],
            json!("sess-b")
        );
    }
}
