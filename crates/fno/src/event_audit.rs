use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

fn belongs(row: &Value, session: &str) -> bool {
    let data = row
        .get("data")
        .filter(|v| v.is_object())
        .or_else(|| row.get("payload"))
        .unwrap_or(&Value::Null);
    let sid = row
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .or_else(|| data.get("session_id").and_then(Value::as_str));
    let holder = data
        .get("holder")
        .and_then(Value::as_str)
        .and_then(|v| v.strip_prefix("target-session:"));
    sid == Some(session) || holder == Some(session)
}

fn read_events(journal: &std::path::Path) -> Result<Vec<Value>, String> {
    let executable =
        std::env::current_exe().map_err(|e| format!("native event reader unavailable: {e}"))?;
    let mut command = Command::new(executable);
    command
        .args(["doctor", "event", "rows", "--events"])
        .arg(journal)
        .arg("--legacy-fallback");
    let output = crate::config_doctor::bounded_output(
        &mut command,
        None,
        std::time::Duration::from_secs(30),
    )
    .map_err(|e| format!("native event reader unavailable: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let lines: Vec<String> = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("invalid native event reader output: {e}"))?;
    Ok(lines
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

fn audit(rows: Vec<Value>, session: &str, strict: bool) -> Value {
    let rows: Vec<Value> = rows
        .into_iter()
        .filter(|row| belongs(row, session))
        .collect();
    let mut init = BTreeSet::new();
    let mut gates = BTreeSet::new();
    if strict {
        for row in &rows {
            let data = row
                .get("payload")
                .filter(|v| v.as_object().is_some_and(|v| !v.is_empty()))
                .or_else(|| row.get("data"))
                .unwrap_or(&Value::Null);
            let Some(phase) = data
                .get("phase")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
            else {
                continue;
            };
            match row.get("type").and_then(Value::as_str) {
                Some("phase_init") => {
                    init.insert(phase.to_string());
                }
                Some("gate_written") => {
                    gates.insert(phase.to_string());
                }
                _ => {}
            }
        }
    }
    let gaps: Vec<String> = init
        .difference(&gates)
        .map(|phase| format!("{phase}: gate_written missing"))
        .collect();
    if gaps.is_empty() {
        json!({"ok": true, "events": rows})
    } else {
        json!({"ok": false, "events": rows, "gaps": gaps})
    }
}

pub(super) fn run(args: &[OsString]) -> i32 {
    let mut session = None;
    let mut journal: Option<PathBuf> = None;
    let mut strict = false;
    let normalized = super::split_options(args, &["--events", "--session-id"]);
    let mut iter = normalized.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--session-id") => match iter.next() {
                Some(value) => session = Some(value.to_string_lossy().into_owned()),
                None => {
                    eprintln!("error: --session-id requires a value");
                    return 2;
                }
            },
            Some("--events") => match iter.next() {
                Some(value) => journal = Some(PathBuf::from(value)),
                None => {
                    eprintln!("error: --events requires a path");
                    return 2;
                }
            },
            Some("--strict") => strict = true,
            Some("--help") => {
                println!("Usage: fno-py doctor event audit [OPTIONS]\n\n  Audit events for a session. Use --strict to check for gaps.\n\nOptions:\n  --session-id <str>  session ID to audit  [required]\n  --strict            check for required event sequences\n  --events <path>     path to events.jsonl\n  --help              Show this message and exit.");
                return 0;
            }
            _ => {
                eprintln!("error: unknown audit option {}", arg.to_string_lossy());
                return 2;
            }
        }
    }
    let Some(session) = session else {
        eprintln!("error: --session-id is required");
        return 2;
    };
    let explicit = journal.is_some();
    let journal = journal.unwrap_or_else(crate::config_cli::history::project_journal);
    if !explicit {
        if let Err(error) = super::hermetic::check(&journal) {
            eprintln!("error: {error}");
            return 1;
        }
    }
    let rows = match read_events(&journal) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let result = audit(rows, &session, strict);
    println!("{}", crate::doctor::python_json(&result));
    if result["ok"] == false {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_audit_filters_sessions_and_reports_sorted_missing_gates() {
        let rows = vec![
            json!({"type":"phase_init","data":{"session_id":"s","phase":"build"}}),
            json!({"type":"phase_init","data":{"holder":"target-session:s","phase":"test"}}),
            json!({"type":"gate_written","data":{"session_id":"s","phase":"test"}}),
            json!({"type":"phase_init","data":{"session_id":"other","phase":"ship"}}),
        ];
        let result = audit(rows, "s", true);
        assert_eq!(result["ok"], false);
        assert_eq!(result["events"].as_array().unwrap().len(), 3);
        assert_eq!(result["gaps"], json!(["build: gate_written missing"]));
        let complete = audit(
            vec![
                json!({"type":"phase_init", "payload":{"session_id":"s", "phase":"build"}}),
                json!({"type":"gate_written", "payload":{"session_id":"s", "phase":"build"}}),
            ],
            "s",
            true,
        );
        assert_eq!(complete["ok"], true);
        assert!(complete.get("gaps").is_none());
    }
}
