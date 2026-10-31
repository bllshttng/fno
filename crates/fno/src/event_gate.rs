use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

fn default_key(reason: &str) -> String {
    let session = ["FNO_SERVER", "FNO_SESSION", "FNO_SESSION_PID"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_default();
    format!(
        "{}:{}:{}",
        reason,
        session.trim(),
        chrono::Utc::now().format("%Y-%m-%d")
    )
}

fn default_journal() -> PathBuf {
    match std::env::var_os("FNO_EVENTS_PATH") {
        Some(pin) if !pin.is_empty() => PathBuf::from(pin),
        _ => crate::config_cli::canonical_repo_root(&crate::config_cli::repo_root())
            .join(".fno/events.jsonl"),
    }
}

fn record_failure(journal: &Path, node: &str, reason: &str, error: &str) {
    eprintln!("gate_escape emit failed (reason={reason}): EventStoreUnavailable: {error}; host op unaffected, telemetry fails open");
    let Some(parent) = journal.parent() else {
        return;
    };
    let path = parent.join("gate_escape_emit_failures.jsonl");
    if super::hermetic::check(&path).is_err() {
        return;
    }
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let line = json!({"ts": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(), "node": node, "reason": reason, "error": format!("EventStoreUnavailable: {error}")});
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn duplicate(
    journal: &Path,
    reason: &str,
    pr: Option<i64>,
    key: Option<&str>,
) -> Result<bool, String> {
    if pr.is_none() && key.is_none() {
        return Ok(false);
    }
    if !crate::event_store::store_path(journal).exists() {
        return Ok(false);
    }
    let query = crate::event_store::EventQuery {
        types: vec!["gate_escape".into()],
        include_rejected: true,
        ..Default::default()
    };
    let rows = crate::event_store::query_events(journal, &query)?;
    Ok(rows
        .iter()
        .filter_map(|row| serde_json::from_str::<Value>(&row.line).ok())
        .any(|row| {
            let data = &row["data"];
            data["reason"].as_str() == Some(reason)
                && (pr.is_some_and(|n| data["pr"].as_i64() == Some(n))
                    || key.is_some_and(|k| data["dedup_key"].as_str() == Some(k)))
        }))
}

pub(super) fn run(args: &[OsString], json_mode: bool) -> i32 {
    let mut reason = None;
    let mut pr = None;
    let mut node = None;
    let mut detail = None;
    let mut key = None;
    let mut journal = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let Some(word) = arg.to_str() else {
            eprintln!("error: invalid argument");
            return 2;
        };
        match word {
            "--pr" | "--pr-number" => {
                match iter.next().and_then(|x| x.to_str()?.parse::<i64>().ok()) {
                    Some(value) => pr = Some(value),
                    None => {
                        eprintln!("error: --pr-number requires an integer");
                        return 2;
                    }
                }
            }
            "--node" | "--detail" | "--dedup-key" | "--events" => {
                let Some(value) = iter.next() else {
                    eprintln!("error: {word} requires a value");
                    return 2;
                };
                match word {
                    "--node" => node = Some(value.to_string_lossy().into_owned()),
                    "--detail" => detail = Some(value.to_string_lossy().into_owned()),
                    "--dedup-key" => key = Some(value.to_string_lossy().into_owned()),
                    _ => journal = Some(PathBuf::from(value)),
                }
            }
            "--help" => {
                print!("{}", concat!(
                    "Usage: fno-py doctor event gate-escape [OPTIONS] {reason}\n\n",
                    "  Tag a human intervention the loop should have handled (Tier-2).\n\n",
                    "  Low-friction manual sugar for the reasons with no clean auto chokepoint\n",
                    "  (flake / stale-base / wedge): an operator runs this at the moment they\n",
                    "  intervene, so retro's autonomy-debt ranking sees all five reason buckets,\n",
                    "  not just the auto-emitted dead-bot (reconcile) and spawn-cap (spawn gates).\n",
                    "  Fail-closed on an unknown reason (loud non-zero exit, emits nothing).\n",
                    "  Deduped so a looped intervention counts once.\n\n",
                    "Arguments:\n",
                    "  reason  intervention class: dead-bot | flake | stale-base | wedge | spawn-\n",
                    "          cap | other  [required]\n\n",
                    "Options:\n",
                    "  --pr-number, --pr <int>  PR the escape rode on (becomes the dedup key when\n",
                    "                           set)\n",
                    "  --node <str>             graph node the escape rode on (attribution)\n",
                    "  --detail <str>           free-text context (retro flags an empty detail as\n",
                    "                           low-signal)\n",
                    "  --dedup-key <str>        explicit PR-less dedup bucket; defaults to\n",
                    "                           reason:session:day\n",
                    "  --events <path>          path to events.jsonl (default: canonical root)\n",
                    "  --help                   Show this message and exit.\n",
                ));
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown option {flag}");
                return 2;
            }
            value if reason.is_none() => reason = Some(value.to_string()),
            _ => {
                eprintln!("error: unexpected argument {word}");
                return 2;
            }
        }
    }
    let Some(reason) = reason else {
        eprintln!("error: missing reason");
        return 2;
    };
    let pr = pr.filter(|n| *n > 0);
    if pr.is_some() && key.is_some() {
        eprintln!("error: pass --pr-number XOR --dedup-key, not both");
        return 1;
    }
    if pr.is_none() && key.as_deref().is_none_or(str::is_empty) {
        key = Some(default_key(&reason));
    }
    let mut data = Map::new();
    data.insert("reason".into(), json!(reason));
    if let Some(value) = pr {
        data.insert("pr".into(), json!(value));
    }
    if let Some(value) = node.as_deref().filter(|x| !x.is_empty()) {
        data.insert("graph_node_id".into(), json!(value));
    }
    if let Some(value) = detail.as_deref().filter(|x| !x.is_empty()) {
        data.insert("detail".into(), json!(value));
    }
    if let Some(value) = key.as_deref().filter(|x| !x.is_empty()) {
        data.insert("dedup_key".into(), json!(value));
    }
    let event = json!({"ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true), "type": "gate_escape", "source": "backlog", "data": data});
    let line = event.to_string();
    match crate::event_store::validate::judge_line(&line) {
        crate::event_store::validate::Verdict::Valid => {}
        crate::event_store::validate::Verdict::Invalid(error)
        | crate::event_store::validate::Verdict::Substrate(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    }
    let journal = journal.unwrap_or_else(default_journal);
    let emitted = match duplicate(&journal, &reason, pr, key.as_deref()) {
        Ok(true) => false,
        Ok(false) => match super::hermetic::check(&journal)
            .and_then(|_| crate::event_store::append_envelope(&journal, &line, None).map(|_| ()))
        {
            Ok(()) => true,
            Err(error) => {
                record_failure(&journal, node.as_deref().unwrap_or(""), &reason, &error);
                false
            }
        },
        Err(error) => {
            record_failure(&journal, node.as_deref().unwrap_or(""), &reason, &error);
            false
        }
    };
    if json_mode {
        let mut result = Map::new();
        result.insert("emitted".into(), json!(emitted));
        result.insert("reason".into(), json!(reason));
        if emitted {
            result.insert("events".into(), json!(journal.to_string_lossy()));
        }
        println!("{}", crate::doctor::python_json(&Value::Object(result)));
    } else if emitted {
        println!("{}", journal.display());
    } else {
        println!("gate_escape[{reason}] not emitted (already counted or fail-open)");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gate_escape_dedup_matches_reason_and_bucket() {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join("events.jsonl");
        let row = r#"{"ts":"2026-10-30T00:00:00Z","type":"gate_escape","source":"backlog","data":{"reason":"flake","dedup_key":"bucket"}}"#;
        crate::event_store::append_envelope(&journal, row, None).unwrap();
        assert!(duplicate(&journal, "flake", None, Some("bucket")).unwrap());
        assert!(!duplicate(&journal, "wedge", None, Some("bucket")).unwrap());
        assert!(!duplicate(&journal, "flake", None, Some("other")).unwrap());
        assert!(!duplicate(&journal, "flake", Some(42), None).unwrap());
    }
}
