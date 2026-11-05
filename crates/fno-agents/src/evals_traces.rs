//! Local failure evidence, bounded at the triggering turn and never exported.
use crate::correction_readings::{HarnessReadings, Reading};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

fn missing(reason: &str) -> Value {
    Reading::unmeasured(reason).to_json()
}
fn observed(value: Value, basis: &str) -> Value {
    json!({"value":value,"basis":basis})
}
fn fact(data: &Value, key: &str) -> Value {
    match data.get(key) {
        Some(v) if v.is_object() && v.get("basis").is_some() => v.clone(),
        Some(v) if !v.is_null() && v != "" => observed(v.clone(), "event"),
        _ => missing(&format!("source names no {key}")),
    }
}

pub(crate) fn create_once(path: &Path, bytes: &[u8]) -> Result<bool, String> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().ok_or("no parent")?)
        .map_err(|e| e.to_string())?;
    tmp.write_all(bytes)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    match tmp.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn transcript(data: &Value, resolver: &crate::context_run::SessionTranscripts) -> Option<PathBuf> {
    let sid = data["session_id"].as_str()?;
    let harness = data["harness"].as_str().unwrap_or("claude");
    resolver.find(sid, harness).or_else(|| {
        data["source_file"]
            .as_str()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    })
}

fn row_matches(row: &Value, turn: &str) -> bool {
    !turn.is_empty()
        && ["/uuid", "/id", "/payload/id", "/payload/call_id"]
            .iter()
            .any(|key| row.pointer(key).and_then(Value::as_str) == Some(turn))
        || (!turn.is_empty()
            && row
                .pointer("/message/content")
                .and_then(Value::as_array)
                .is_some_and(|bs| bs.iter().any(|b| b["tool_use_id"] == turn)))
}

fn read_rows(path: &Path) -> Result<Vec<Value>, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if f.metadata().map_err(|e| e.to_string())?.len() > 64 * 1024 * 1024 {
        return Err("transcript exceeds the 64 MiB intake budget".into());
    }
    std::io::BufReader::new(f)
        .lines()
        .map(|line| {
            let line = line.map_err(|e| e.to_string())?;
            serde_json::from_str(&line).map_err(|e| format!("invalid transcript row: {e}"))
        })
        .collect()
}

fn snapshot(data: &Value, resolver: &crate::context_run::SessionTranscripts) -> Value {
    let empty = |reason: &str| {
        json!({"tool_calls":missing(reason),"claims":missing(reason),
        "context_percent":missing(reason),"compaction_count":missing(reason)})
    };
    let Some(path) = transcript(data, resolver) else {
        return empty("transcript unavailable");
    };
    let rows = match read_rows(&path) {
        Ok(rows) => rows,
        Err(e) => return empty(&e),
    };
    let turn = data["turn"].as_str().unwrap_or("");
    let boundary = rows.iter().position(|r| row_matches(r, turn)).or_else(|| {
        let stamp = data["ts_turn"].as_str()?;
        rows.iter()
            .position(|r| r["timestamp"].as_str().is_some_and(|ts| ts >= stamp))
    });
    let Some(boundary) = boundary else {
        return empty("failure turn not found in transcript");
    };
    let harness = data["harness"].as_str().unwrap_or("claude");
    let mut readings = HarnessReadings::empty(harness);
    let mut entries = Vec::new();
    let mut compactions = 0;
    let mut percent = missing("no turn-bounded usage and window reading");
    for row in &rows[..boundary] {
        readings.observe(row);
        if row["subtype"] == "compact_boundary" || row["type"] == "compacted" {
            compactions += 1;
        }
        match harness {
            "claude" => crate::lead_hygiene::claude_row_entries(row, &mut entries),
            "codex" => {
                if let Err(e) = crate::lead_hygiene::codex_row_entries(row, &mut entries) {
                    return empty(e);
                }
            }
            _ => return empty("no transcript entry reader for this harness"),
        }
        if let Some(usage) = crate::context_window::parse_usage_record(row) {
            let window = crate::context_window::window_reading(&usage.model, Path::new("."));
            // Routed Astra windows require a harness receipt; the declared model window is insufficient.
            if !crate::context_window::is_astra_model(&usage.model) {
                if let Some(p) = usage
                    .used_tokens()
                    .and_then(|used| crate::context_window::used_percent(used, window.tokens))
                {
                    percent = observed(json!(p), "transcript_usage_and_model_window");
                }
            }
        }
        if let Some(info) = row.pointer("/payload/info") {
            if let (Some(used), Some(window)) = (
                info.pointer("/last_token_usage/input_tokens")
                    .and_then(Value::as_u64),
                info["model_context_window"].as_u64(),
            ) {
                if let Some(p) = crate::context_window::used_percent(used, window) {
                    percent = observed(json!(p), "transcript_usage_and_window");
                }
            }
        }
    }
    let tail = |kind: &str| -> Vec<Value> {
        let selected: Vec<_> = entries.iter().filter(|e| e.kind == kind).collect();
        selected
            .iter()
            .skip(selected.len().saturating_sub(10))
            .map(|e| {
                if kind == "tool_use" {
                    json!({"tool":e.tool,"intent":e.target})
                } else {
                    json!(e.text)
                }
            })
            .collect()
    };
    json!({"tool_calls":observed(json!(tail("tool_use")),"transcript_before_turn"),
        "claims":observed(json!(tail("assistant_text")),"transcript_before_turn"),
        "context_percent":percent,"compaction_count":observed(json!(compactions),"transcript_before_turn"),
        "model":readings.model.to_json(),"effort":readings.effort.to_json()})
}

fn write_trace(
    state: &Path,
    kind: &str,
    key: &str,
    data: &Value,
    resolver: &crate::context_run::SessionTranscripts,
) -> Result<bool, String> {
    let id = blake3::hash(format!("{kind}|{}|{key}", data["session_id"]).as_bytes())
        .to_hex()
        .to_string();
    let dir = state.join("evals/traces");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{id}.json"));
    if path.exists() {
        return Ok(false);
    }
    let mut trace = snapshot(data, resolver);
    trace["id"] = json!(id);
    trace["signal_kind"] = json!(kind);
    trace["trigger"] = fact(data, "quote");
    trace["truth"] = fact(data, "reason");
    trace["right_action"] = missing("not labeled");
    trace["layer_verdict"] = missing("not labeled");
    trace["profile_id"] = missing("profile pass not run");
    for key in ["node", "model", "harness", "effort", "session_id"] {
        if trace.get(key).is_none() || trace[key]["value"].is_null() {
            trace[key] = fact(data, key);
        }
    }
    trace["created_at"] = json!(chrono::Utc::now().to_rfc3339());
    crate::correction_detect::scrub_value(&mut trace, &mut Default::default());
    create_once(
        &path,
        &serde_json::to_vec_pretty(&trace).map_err(|e| e.to_string())?,
    )
}

fn routed_around(data: &Value, resolver: &crate::context_run::SessionTranscripts) -> bool {
    let Some(rows) = transcript(data, resolver).and_then(|p| read_rows(&p).ok()) else {
        return false;
    };
    let turn = data["turn"].as_str().unwrap_or("");
    let mut calls = std::collections::BTreeMap::new();
    let mut blocked = None;
    let mut retries = std::collections::BTreeSet::new();
    let mut following = 0;
    for row in rows {
        for block in row
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = block["id"].as_str().unwrap_or("");
            if block["type"] == "tool_use" {
                let call = json!([block["name"], block["input"]]);
                calls.insert(id.to_string(), call.clone());
                if let Some(original) = &blocked {
                    following += 1;
                    if following > 10 {
                        return false;
                    }
                    if original == &call {
                        retries.insert(id.to_string());
                    }
                }
            }
            if block["type"] == "tool_result" {
                let result_id = block["tool_use_id"].as_str().unwrap_or("");
                if result_id == turn && block["is_error"] == true {
                    blocked = calls.get(result_id).cloned();
                } else if retries.contains(result_id) && block["is_error"] != true {
                    return true;
                }
            }
        }
    }
    false
}

fn pass(state: &Path, journal: &Path, repo: &Path, prs: &[Value]) -> Result<Value, String> {
    let dir = state.join("evals/traces");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".pass.lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let mark = dir.join(".watermark");
    let after = std::fs::read_to_string(&mark)
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(0);
    let query = crate::event_store::EventQuery {
        types: vec![
            "agent_corrected".into(),
            "termination".into(),
            "review_attestation".into(),
        ],
        ..Default::default()
    };
    // Unsettled refusals are revisited: the successful call may arrive after yesterday's watermark.
    let rows = crate::event_store::query_events(journal, &query)?;
    let resolver = crate::context_run::SessionTranscripts::default();
    let mut count = 0;
    let mut last = after;
    for event in rows {
        let line: Value = serde_json::from_str(&event.line).map_err(|e| e.to_string())?;
        let mut data = line["data"].clone();
        if data["ts_turn"].is_null() {
            data["ts_turn"] = line["ts"].clone();
        }
        let machine = data["corrected_by"] == "machine" && data["kind"] == "refused";
        if event.seq <= after && !machine {
            continue;
        }
        let kind = match event.r#type.as_str() {
            "agent_corrected" if crate::evals_drafts::typed_correction(&data).is_some() => {
                Some("user_correction")
            }
            "agent_corrected" if machine && routed_around(&data, &resolver) => {
                Some("refusal_bypassed")
            }
            "termination" => match data["reason"]
                .as_str()
                .filter(|r| crate::run_outcome::classify_legacy(r).is_ok())
            {
                Some("Budget") => Some("budget"),
                Some("NoProgress") => Some("no_progress"),
                Some("Aborted") => Some("stuck"),
                _ => None,
            },
            "review_attestation"
                if data["verdict"] == "fail" && data["retracts_attester"].is_string() =>
            {
                data["reason"] = data["retraction_reason"].clone();
                Some("false_claim")
            }
            _ => None,
        };
        if let Some(kind) = kind {
            count += u64::from(write_trace(state, kind, &event.event_id, &data, &resolver)?);
        }
        last = last.max(event.seq);
    }
    let mut reverts = Vec::new();
    crate::evals_candidates::read_reverts(repo, &mut reverts);
    for revert in reverts {
        let data = json!({"quote":revert.quote,"reason":"repository revert","node":null});
        let key = format!("{}:{}", repo.display(), revert.source_ref);
        count += u64::from(write_trace(state, "revert", &key, &data, &resolver)?);
    }
    for pr in prs {
        if !pr["closedAt"].is_string() || !pr["mergedAt"].is_null() {
            continue;
        }
        let Some(url) = pr["url"].as_str() else {
            return Err("closed PR row has no url".into());
        };
        let data = json!({"quote":pr["title"],"reason":"PR closed without merge","node":pr["node_id"],"pr":url});
        count += u64::from(write_trace(
            state,
            "closed_unmerged",
            url,
            &data,
            &resolver,
        )?);
    }
    std::fs::write(mark, last.to_string()).map_err(|e| e.to_string())?;
    Ok(json!({"new":count}))
}

pub fn run(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("usage: fno-agents evals candidates --traces");
        return 2;
    }
    match daily(&crate::evals_candidates::state_root()) {
        Ok(receipt) => {
            println!("{receipt}");
            0
        }
        Err(e) => {
            eprintln!("eval traces: {e}");
            1
        }
    }
}

fn daily(state: &Path) -> Result<Value, String> {
    let repo = crate::evals::canonical_repo_root();
    let journal = state.join("events.jsonl");
    let local = pass(state, &journal, &repo, &[])?;
    let mut cmd = std::process::Command::new("fno");
    cmd.current_dir(&repo)
        .args(["do", "pr", "list", "--state", "closed"]);
    let out =
        crate::bounded_cmd::output_with_timeout_result(cmd, 120).map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "closed PR intake: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let prs: Vec<Value> =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("closed PR list: {e}"))?;
    let remote = pass(state, &journal, &repo, &prs)?;
    Ok(json!({"new":local["new"].as_u64().unwrap_or(0)+remote["new"].as_u64().unwrap_or(0)}))
}

#[derive(Default)]
pub struct Arm {
    next: std::sync::Mutex<Option<std::time::Instant>>,
    in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    use std::sync::atomic::Ordering;
    {
        let mut next = arm.next.lock().unwrap_or_else(|e| e.into_inner());
        if next.is_some_and(|t| t > std::time::Instant::now())
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *next = Some(std::time::Instant::now() + std::time::Duration::from_secs(86400));
    }
    let flag = std::sync::Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Some(state) = home.root().parent() {
            if let Err(e) = daily(state) {
                eprintln!("eval traces: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append(journal: &Path, kind: &str, mut data: Value) {
        if kind == "agent_corrected" {
            data["rule_version"] = json!("v1");
        }
        crate::event_store::append_envelope(journal, &json!({
            "ts": "2026-11-01T00:00:00Z", "type": kind,
            "source": if kind == "termination" { "hook" } else if kind == "review_attestation" { "target" } else { "daemon" }, "data": data,
        }).to_string(), None).unwrap();
    }

    fn traces(state: &Path) -> Vec<Value> {
        let mut rows: Vec<_> = std::fs::read_dir(state.join("evals/traces"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .map(|p| serde_json::from_slice::<Value>(&std::fs::read(p).unwrap()).unwrap())
            .collect();
        rows.sort_by_key(|r| r["signal_kind"].to_string());
        rows
    }

    #[test]
    fn evals_traces_correction_is_turn_bounded_missing_facts_are_explicit_and_replay_preserves_labels(
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("events.jsonl");
        let transcript = tmp.path().join("session.jsonl");
        let mut rows = vec![json!({"type":"system", "subtype":"compact_boundary"})];
        for i in 0..12 {
            rows.push(json!({"type":"assistant", "message":{"role":"assistant", "model":"claude-sonnet-4-5",
                "usage":{"input_tokens":1000}, "content":[{"type":"tool_use", "id":format!("c{i}"),
                "name":"Bash", "input":{"command":format!("echo {i}")}}, {"type":"text","text":format!("claim {i}")} ]}}));
        }
        rows.push(json!({"type":"user", "uuid":"turn-1", "message":{"content":"no, prove it"}}));
        rows.push(json!({"type":"assistant", "message":{"model":"claude-sonnet-4-5", "usage":{"input_tokens":99000}, "content":[{"type":"text","text":"future"}]}}));
        std::fs::write(
            &transcript,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        for (id, path) in [("one", transcript), ("missing", tmp.path().join("gone"))] {
            append(
                &journal,
                "agent_corrected",
                json!({"correction_id":id, "corrected_by":"user", "kind":"correct",
                "quote":"no, prove it", "quote_status":"kept", "session_id":"s", "harness":"claude", "turn":"turn-1", "source_file":path}),
            );
        }
        assert_eq!(
            pass(tmp.path(), &journal, tmp.path(), &[]).unwrap()["new"],
            2
        );
        let rows = traces(tmp.path());
        let found = rows
            .iter()
            .find(|r| r["tool_calls"]["value"].is_array())
            .unwrap();
        assert_eq!(found["tool_calls"]["value"].as_array().unwrap().len(), 10);
        assert!(!found["claims"].to_string().contains("future"));
        assert_eq!(found["compaction_count"]["value"], 1);
        assert!(found["context_percent"]["value"].as_u64().unwrap() < 2);
        let missing = rows
            .iter()
            .find(|r| r["tool_calls"]["value"].is_null())
            .unwrap();
        assert_eq!(missing["tool_calls"]["basis"], "unmeasured");
        assert!(missing["tool_calls"]["reason"].is_string());
        let path = tmp
            .path()
            .join("evals/traces")
            .join(format!("{}.json", found["id"].as_str().unwrap()));
        let mut labeled = found.clone();
        labeled["right_action"] = json!({"value":"run the check","basis":"user_set"});
        std::fs::write(&path, labeled.to_string()).unwrap();
        assert_eq!(
            pass(tmp.path(), &journal, tmp.path(), &[]).unwrap()["new"],
            0
        );
        assert!(std::fs::read_to_string(path)
            .unwrap()
            .contains("run the check"));
    }

    #[test]
    fn evals_traces_machine_signals_require_evidence_and_retry_an_unfinished_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("events.jsonl");
        let transcript = tmp.path().join("s.jsonl");
        let mut rows = vec![
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"blocked","name":"Bash","input":{"command":"git push"}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"blocked","is_error":true,"content":"hook denied"}]}}),
        ];
        let save = |rows: &[Value]| {
            std::fs::write(
                &transcript,
                rows.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap()
        };
        save(&rows);
        append(
            &journal,
            "agent_corrected",
            json!({"correction_id":"gate","corrected_by":"machine","kind":"refused","session_id":"s","harness":"claude","turn":"blocked","source_file":transcript}),
        );
        assert_eq!(
            pass(tmp.path(), &journal, tmp.path(), &[]).unwrap()["new"],
            0,
            "a refusal alone is no bypass"
        );
        rows.push(json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"retry","name":"Bash","input":{"command":"git push"}}]}}));
        rows.push(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"retry","is_error":false,"content":"pushed"}]}}));
        save(&rows);
        for reason in ["Budget", "NoProgress", "Aborted", "DonePRGreen"] {
            append(
                &journal,
                "termination",
                json!({"session_id":reason,"reason":reason,"message":"run ended"}),
            );
        }
        append(
            &journal,
            "review_attestation",
            json!({"reviewer":"code-review","head_sha":"abc","verdict":"fail","session_id":"s","retracts_attester":"s","retraction_reason":"claim contradicted"}),
        );
        let prs = [
            json!({"number":7,"url":"https://github.com/o/r/pull/7","closedAt":"2026-11-01T00:00:00Z","mergedAt":null}),
            json!({"number":8,"url":"https://github.com/o/r/pull/8","closedAt":"2026-11-01T00:00:00Z","mergedAt":"2026-11-01T00:00:00Z"}),
        ];
        let receipt = pass(tmp.path(), &journal, tmp.path(), &prs).unwrap();
        assert_eq!(receipt["new"], 6);
        let kinds: Vec<_> = traces(tmp.path())
            .iter()
            .map(|v| v["signal_kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "budget",
                "closed_unmerged",
                "false_claim",
                "no_progress",
                "refusal_bypassed",
                "stuck"
            ]
        );
        assert_eq!(
            pass(tmp.path(), &journal, tmp.path(), &prs).unwrap()["new"],
            0
        );
    }
}
