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

fn transcript_key(data: &Value) -> String {
    json!([data["session_id"], data["harness"], data["source_file"]]).to_string()
}

struct TranscriptEvidence {
    resolver: crate::context_run::SessionTranscripts,
    key: String,
    rows: Result<Vec<Value>, String>,
    bypassed: std::collections::BTreeSet<String>,
}

impl Default for TranscriptEvidence {
    fn default() -> Self {
        Self {
            resolver: Default::default(),
            key: String::new(),
            rows: Err("transcript unavailable".into()),
            bypassed: Default::default(),
        }
    }
}

impl TranscriptEvidence {
    fn prepare(&mut self, data: &Value) {
        let key = transcript_key(data);
        if key == self.key {
            return;
        }
        self.rows = Err("transcript unavailable".into());
        self.bypassed.clear();
        let rows = transcript(data, &self.resolver)
            .ok_or_else(|| "transcript unavailable".to_string())
            .and_then(|p| read_rows(&p));
        if let Ok(rows) = &rows {
            self.bypassed = bypassed_turns(rows);
        }
        self.rows = rows;
        self.key = key;
    }
    fn rows(&mut self, data: &Value) -> Result<&[Value], &str> {
        self.prepare(data);
        self.rows.as_deref().map_err(String::as_str)
    }
}

fn snapshot(data: &Value, evidence: &mut TranscriptEvidence) -> Value {
    let empty = |reason: &str| {
        json!({"tool_calls":missing(reason),"claims":missing(reason),
        "context_percent":missing(reason),"compaction_count":missing(reason)})
    };
    let rows = match evidence.rows(data) {
        Ok(rows) => rows,
        Err(e) => return empty(e),
    };
    let turn = data["turn"].as_str().unwrap_or("");
    let boundary = rows.iter().position(|r| row_matches(r, turn)).or_else(|| {
        let stamp = crate::event_store::parse_rfc3339_ms(data["ts_turn"].as_str()?)?;
        rows.iter().position(|r| {
            r["timestamp"]
                .as_str()
                .and_then(crate::event_store::parse_rfc3339_ms)
                .is_some_and(|ts| ts >= stamp)
        })
    });
    let Some(boundary) = boundary else {
        return empty("failure turn not found in transcript");
    };
    let harness = data["harness"].as_str().unwrap_or("claude");
    let mut readings = HarnessReadings::empty(harness);
    let mut entries = Vec::new();
    let mut compactions = 0;
    let mut windows = std::collections::BTreeMap::new();
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
            percent = missing("the latest usage has no provable context window");
            // Routed Astra windows require a harness receipt; the declared model window is insufficient.
            if !usage.model.trim().is_empty()
                && !crate::context_window::is_astra_model(&usage.model)
            {
                let window = *windows.entry(usage.model.clone()).or_insert_with(|| {
                    crate::context_window::window_reading(&usage.model, Path::new(".")).tokens
                });
                if let Some(p) = usage
                    .used_tokens()
                    .and_then(|used| crate::context_window::used_percent(used, window))
                {
                    percent = observed(json!(p), "transcript_usage_and_model_window");
                }
            }
        }
        if let Some(info) = row.pointer("/payload/info") {
            if info.get("last_token_usage").is_some_and(Value::is_object) {
                percent = missing("the latest usage has no provable context window");
            }
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

fn trace_id(kind: &str, key: &str, data: &Value) -> String {
    let failure = if kind == "seed_correction" {
        key
    } else {
        data["turn"]
            .as_str()
            .filter(|turn| !turn.is_empty())
            .unwrap_or(key)
    };
    let session = if kind == "seed_correction" {
        Value::Null
    } else {
        data["session_id"].clone()
    };
    blake3::hash(format!("{kind}|{session}|{failure}").as_bytes())
        .to_hex()
        .to_string()
}

fn write_trace(
    state: &Path,
    kind: &str,
    key: &str,
    data: &Value,
    evidence: &mut TranscriptEvidence,
) -> Result<bool, String> {
    let id = trace_id(kind, key, data);
    let dir = state.join("evals/traces");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{id}.json"));
    if path.exists() {
        return Ok(false);
    }
    let mut trace = snapshot(data, evidence);
    trace["id"] = json!(id);
    trace["signal_kind"] = json!(kind);
    trace["source_ref"] = observed(json!(key), "source_identity");
    let trigger = ["quote", "message", "reason"]
        .into_iter()
        .find(|key| !data[*key].is_null())
        .unwrap_or("quote");
    trace["trigger"] = if kind == "user_correction" && data["quote"].is_null() {
        missing("the correction quote is unavailable")
    } else if data[trigger].is_null() {
        observed(json!(kind), "source_signal")
    } else {
        fact(data, trigger)
    };
    trace["truth"] = if data["reason"].is_null() {
        fact(data, "quote")
    } else {
        fact(data, "reason")
    };
    trace["right_action"] = missing("not labeled");
    trace["layer_verdict"] = missing("not labeled");
    trace["profile_id"] = observed(
        json!(crate::evals_profile::id(state)?),
        "local_seeded_user_hash",
    );
    for key in ["node", "model", "harness", "effort", "session_id"] {
        if trace.get(key).is_none() || trace[key]["value"].is_null() {
            trace[key] = fact(data, key);
        }
    }
    trace["created_at"] = json!(chrono::Utc::now().to_rfc3339());
    crate::correction_detect::scrub_value(&mut trace, &mut Default::default());
    trace["id"] = json!(id);
    if kind == "revert" && key.len() == 40 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
        trace["source_ref"] = observed(json!(key), "git_commit");
    }
    create_once(
        &path,
        &serde_json::to_vec_pretty(&trace).map_err(|e| e.to_string())?,
    )
}

fn bypassed_turns(rows: &[Value]) -> std::collections::BTreeSet<String> {
    let mut calls = std::collections::BTreeMap::<String, (Value, usize)>::new();
    let mut pending = Vec::<(Value, String, usize)>::new();
    let mut refused = std::collections::BTreeSet::new();
    let mut proven = std::collections::BTreeSet::new();
    let mut step = 0usize;
    for row in rows {
        for block in row
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if block["type"] == "tool_use" {
                let Some(id) = block["id"].as_str().filter(|id| !id.is_empty()) else {
                    continue;
                };
                step += 1;
                pending.retain(|(_, _, start)| step.saturating_sub(*start) <= 10);
                calls.insert(
                    id.to_string(),
                    (json!([block["name"], block["input"]]), step),
                );
            }
            if block["type"] != "tool_result" {
                continue;
            }
            let Some(id) = block["tool_use_id"].as_str() else {
                continue;
            };
            let Some((call, requested)) = calls.get(id) else {
                continue;
            };
            if block["is_error"] == true {
                if refused.insert(id.to_string()) {
                    pending.push((call.clone(), id.to_string(), step));
                }
            } else {
                for (original, blocked, start) in &pending {
                    if requested > start && step.saturating_sub(*start) <= 10 && call == original {
                        proven.insert(blocked.clone());
                    }
                }
            }
        }
    }
    proven
}

fn routed_around(data: &Value, evidence: &mut TranscriptEvidence) -> bool {
    evidence.prepare(data);
    data["turn"]
        .as_str()
        .is_some_and(|turn| evidence.bypassed.contains(turn))
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
    let mut rows = crate::event_store::query_events(journal, &query)?;
    rows.sort_by_cached_key(|event| {
        serde_json::from_str::<Value>(&event.line)
            .ok()
            .map(|v| transcript_key(&v["data"]))
            .unwrap_or_default()
    });
    let mut evidence = TranscriptEvidence::default();
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
            "agent_corrected"
                if data["corrected_by"] == "user"
                    && matches!(data["kind"].as_str(), Some("correct" | "reject")) =>
            {
                Some("user_correction")
            }
            "agent_corrected" if machine && routed_around(&data, &mut evidence) => {
                Some("refusal_bypassed")
            }
            "termination" => match data["reason"]
                .as_str()
                .filter(|r| crate::run_outcome::classify_legacy(r).is_ok())
            {
                Some("Budget") => Some("budget"),
                Some("NoProgress") => Some("no_progress"),
                Some("Aborted") => Some("aborted"),
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
            count += u64::from(write_trace(
                state,
                kind,
                &event.event_id,
                &data,
                &mut evidence,
            )?);
        }
        last = last.max(event.seq);
    }
    let mut reverts = Vec::new();
    crate::evals_candidates::read_reverts(repo, &mut reverts);
    for revert in reverts {
        let data = json!({"quote":revert.quote,"reason":"repository revert","node":null});
        let key = revert.source_ref;
        count += u64::from(write_trace(state, "revert", &key, &data, &mut evidence)?);
    }
    count += write_closed(state, prs, &mut evidence)?;
    std::fs::write(mark, last.to_string()).map_err(|e| e.to_string())?;
    Ok(json!({"new":count}))
}

fn write_closed(
    state: &Path,
    prs: &[Value],
    evidence: &mut TranscriptEvidence,
) -> Result<u64, String> {
    let mut count = 0;
    for pr in prs {
        if !pr["closedAt"].is_string() || !pr["mergedAt"].is_null() {
            continue;
        }
        let Some(url) = pr["url"].as_str() else {
            return Err("closed PR row has no url".into());
        };
        let data = json!({"quote":pr["title"],"reason":"PR closed without merge","node":pr["node_id"],"pr":url});
        count += u64::from(write_trace(state, "closed_unmerged", url, &data, evidence)?);
    }
    Ok(count)
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

fn import_seed(state: &Path, log: &Path) -> Result<usize, String> {
    let dir = state.join("evals/traces");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let seed = dir.join("seed-s2.log");
    if !seed.exists() {
        let text = match std::fs::read_to_string(log) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("seed source {}: {e}", log.display())),
        };
        let frozen = text
            .lines()
            .filter(|l| l.splitn(5, " | ").nth(1) == Some("S2"))
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        create_once(&seed, frozen.as_bytes())?;
    }
    let text = std::fs::read_to_string(&seed).map_err(|e| e.to_string())?;
    let pattern = regex::Regex::new(r#"^- "(.+)" \(x\d+, ([0-9a-f]+), signal=(\w+)"#)
        .expect("seed row pattern");
    let mut evidence = TranscriptEvidence::default();
    let mut count = 0;
    let mut lines: Vec<_> = text.lines().enumerate().collect();
    lines.sort_by_cached_key(|(_, line)| {
        pattern
            .captures(line.splitn(5, " | ").last().unwrap_or(""))
            .map(|caps| caps[2].to_string())
            .unwrap_or_default()
    });
    for (index, line) in lines {
        let fields: Vec<_> = line.splitn(5, " | ").collect();
        let [stamp, "S2", _, _, body] = fields.as_slice() else {
            return Err(format!("invalid frozen S2 row {}", index + 1));
        };
        let Some(caps) = pattern.captures(body.trim()) else {
            return Err(format!("unparseable frozen S2 row {}", index + 1));
        };
        let key = format!("seed:{index}:{}", blake3::hash(line.as_bytes()));
        if dir
            .join(format!(
                "{}.json",
                trace_id("seed_correction", &key, &Value::Null)
            ))
            .exists()
        {
            count += 1;
            continue;
        }
        let prefix = &caps[2];
        let found = seed_transcript(prefix);
        let mut data = json!({"quote":&caps[1],"logged_at":stamp,"session_id":prefix});
        if let Some((sid, path)) = found {
            data["session_id"] = json!(sid);
            data["harness"] = json!("claude");
            data["source_file"] = json!(path);
            if let Ok(rows) = evidence.rows(&data) {
                let flat = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
                let quote = flat(&caps[1]);
                let matching: Vec<_> = rows
                    .iter()
                    .filter(|row| {
                        row["type"] == "user"
                            && flat(&crate::lead_hygiene::message_text(
                                &row["message"]["content"],
                            )) == quote
                    })
                    .collect();
                if let [row] = matching.as_slice() {
                    data["turn"] = row["uuid"].clone();
                    data["ts_turn"] = row["timestamp"].clone();
                }
            }
        }
        write_trace(state, "seed_correction", &key, &data, &mut evidence)?;
        count += 1;
    }
    Ok(count)
}

fn seed_transcript(prefix: &str) -> Option<(String, PathBuf)> {
    let mut matches = Vec::new();
    for project in std::fs::read_dir(crate::claude_drive::claude_projects_dir())
        .ok()?
        .flatten()
    {
        for entry in std::fs::read_dir(project.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            let Some(sid) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().is_some_and(|s| s == "jsonl") && sid.starts_with(prefix) {
                matches.push((sid.to_string(), path));
            }
        }
    }
    matches.sort();
    matches.dedup();
    (matches.len() == 1).then(|| matches.remove(0))
}

fn daily(state: &Path) -> Result<Value, String> {
    let source = crate::state_layout::place(state, "corrections.log");
    let seeded = import_seed(state, &source)?;
    crate::evals_profile::local(state, None)?;
    let repo = crate::evals::canonical_repo_root();
    let journal = state.join("events.jsonl");
    let local = pass(state, &journal, &repo, &[])?;
    let prs = crate::pr_list::rows(&["--state".into(), "closed".into()], &repo)?;
    let remote = write_closed(state, &prs, &mut TranscriptEvidence::default())?;
    Ok(json!({"new":local["new"].as_u64().unwrap_or(0)+remote,"seed":seeded}))
}

#[derive(Default)]
pub struct Arm {
    next: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
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
    let next = std::sync::Arc::clone(&arm.next);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Some(state) = home.root().parent() {
            if let Err(e) = daily(state) {
                eprintln!("eval traces: {e}");
                *next.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(300));
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
        for (id, path) in [
            ("one", transcript),
            ("missing", tmp.path().join("gone")),
            ("scrubbed", tmp.path().join("scrubbed-gone")),
        ] {
            append(
                &journal,
                "agent_corrected",
                json!({"correction_id":id, "corrected_by":"user", "kind":"correct",
                "quote":if id=="scrubbed" { Value::Null } else { json!("no, prove it") }, "quote_status":if id=="scrubbed" { "dropped" } else { "kept" }, "session_id":id, "harness":"claude", "turn":"turn-1", "source_file":path}),
            );
        }
        assert_eq!(
            pass(tmp.path(), &journal, tmp.path(), &[]).unwrap()["new"],
            3
        );
        let rows = traces(tmp.path());
        let scrubbed = rows
            .iter()
            .find(|r| r["session_id"]["value"] == "scrubbed")
            .unwrap();
        assert!(scrubbed["trigger"]["value"].is_null());
        assert_eq!(scrubbed["trigger"]["basis"], "unmeasured");
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
        append(
            &journal,
            "agent_corrected",
            json!({"correction_id":"duplicate-envelope","corrected_by":"user","kind":"correct",
            "quote":"no, prove it","quote_status":"kept","session_id":"one","harness":"claude","turn":"turn-1"}),
        );
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
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(tmp.path())
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@example.org",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        std::fs::write(tmp.path().join("a"), "one").unwrap();
        git(&["add", "a"]);
        git(&["commit", "-qm", "initial"]);
        std::fs::write(tmp.path().join("a"), "two").unwrap();
        git(&["add", "a"]);
        git(&["commit", "-qm", "change"]);
        git(&["revert", "--no-edit", "HEAD"]);
        let receipt = pass(tmp.path(), &journal, tmp.path(), &prs).unwrap();
        assert_eq!(receipt["new"], 7);
        let revert = traces(tmp.path())
            .into_iter()
            .find(|r| r["signal_kind"] == "revert")
            .unwrap();
        assert_eq!(
            revert["source_ref"]["value"]
                .as_str()
                .unwrap()
                .rsplit(':')
                .next()
                .unwrap()
                .len(),
            40,
            "revert identity needs the full commit hash"
        );
        let budget = traces(tmp.path())
            .into_iter()
            .find(|r| r["signal_kind"] == "budget")
            .unwrap();
        assert_eq!(budget["trigger"]["value"], "run ended");
        let kinds: Vec<_> = traces(tmp.path())
            .iter()
            .map(|v| v["signal_kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "aborted",
                "budget",
                "closed_unmerged",
                "false_claim",
                "no_progress",
                "refusal_bypassed",
                "revert"
            ]
        );
        assert_eq!(
            pass(tmp.path(), &journal, tmp.path(), &prs).unwrap()["new"],
            0
        );
    }
    #[test]
    fn evals_traces_context_readings_do_not_reuse_unknown_windows_or_compare_clock_strings() {
        let tmp = tempfile::tempdir().unwrap();
        let mut actual = Vec::new();
        for (name, model, input, cache) in [
            ("astra", "gpt-6-astra", 1000, 0),
            ("missing_model", "", 1000, 0),
            ("overflow", "claude-sonnet-4-5", u64::MAX, 1),
        ] {
            let path = tmp.path().join(format!("{name}.jsonl"));
            let rows = [
                json!({"type":"assistant","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1000},"content":[]}}),
                json!({"type":"assistant","message":{"model":model,"usage":{"input_tokens":input,"cache_creation_input_tokens":cache},"content":[]}}),
                json!({"type":"user","uuid":"failure","message":{"content":"no, prove it"}}),
            ];
            std::fs::write(
                &path,
                rows.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap();
            let frame = snapshot(
                &json!({"session_id":name,"harness":"claude","turn":"failure","source_file":path}),
                &mut TranscriptEvidence::default(),
            );
            assert_eq!(frame["compaction_count"]["value"], 0);
            actual.push(json!([name, frame["context_percent"]["value"]]));
        }
        for (name, info) in [
            (
                "codex_missing_window",
                json!({"last_token_usage":{"input_tokens":100000},"model_context_window":null}),
            ),
            ("codex_heartbeat", Value::Null),
        ] {
            let path = tmp.path().join(format!("{name}.jsonl"));
            let rows = [
                json!({"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000},"model_context_window":200000}}}),
                json!({"type":"event_msg","payload":{"type":"token_count","info":info}}),
                json!({"type":"response_item","payload":{"id":"failure","type":"message","role":"user","content":[]}}),
            ];
            std::fs::write(
                &path,
                rows.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap();
            let frame = snapshot(
                &json!({"session_id":name,"harness":"codex","turn":"failure","source_file":path}),
                &mut TranscriptEvidence::default(),
            );
            assert_eq!(frame["compaction_count"]["value"], 0);
            actual.push(json!([name, frame["context_percent"]["value"]]));
        }
        let path = tmp.path().join("offsets.jsonl");
        let rows = [
            json!({"type":"assistant","timestamp":"2026-11-01T11:00:00+01:00","message":{"content":[{"type":"tool_use","id":"a","name":"Bash","input":{"command":"echo first"}}]}}),
            json!({"type":"assistant","timestamp":"2026-11-01T13:00:00+02:00","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":"echo second"}}]}}),
            json!({"type":"user","timestamp":"2026-11-01T12:00:00Z","message":{"content":"no, prove it"}}),
        ];
        std::fs::write(
            &path,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let frame = snapshot(
            &json!({"session_id":"offsets","harness":"claude","ts_turn":"2026-11-01T12:00:00Z","source_file":path}),
            &mut TranscriptEvidence::default(),
        );
        actual.push(json!([
            "utc_boundary_calls",
            frame["tool_calls"]["value"].as_array().unwrap().len()
        ]));
        assert_eq!(
            json!(actual),
            json!([
                ["astra", null],
                ["missing_model", null],
                ["overflow", null],
                ["codex_missing_window", null],
                ["codex_heartbeat", 1],
                ["utc_boundary_calls", 2]
            ])
        );
    }

    #[test]
    fn evals_traces_seed_freezes_only_s2_rows_and_finishes_after_an_interrupted_import() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("corrections.log");
        let text = "2026-11-01T00:00:00Z | S1 | postmortem | a.md | pointer\n2026-11-01T00:00:00Z | S2 | insights-tag | r.md:1 | - \"no, prove it\" (x1, 01a0, signal=correction)\n2026-11-01T00:00:00Z | S2 | insights-tag | r.md:2 | - \"use the other branch\" (x1, 01b0, signal=repeated_correction)\n";
        std::fs::write(&log, text).unwrap();
        assert_eq!(import_seed(tmp.path(), &log).unwrap(), 2);
        assert_eq!(traces(tmp.path()).len(), 2);
        let line = text.lines().find(|l| l.contains(" | S2 | ")).unwrap();
        let key = format!("seed:0:{}", blake3::hash(line.as_bytes()));
        assert!(
            !write_trace(
                tmp.path(),
                "seed_correction",
                &key,
                &json!({"session_id":"newly-resolved-full-session"}),
                &mut TranscriptEvidence::default()
            )
            .unwrap(),
            "enrichment cannot change a seed identity"
        );
        let seed = tmp.path().join("evals/traces/seed-s2.log");
        let frozen = std::fs::read(&seed).unwrap();
        assert_eq!(
            String::from_utf8(frozen.clone()).unwrap().lines().count(),
            2
        );
        let trace = std::fs::read_dir(seed.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|x| x == "json"))
            .unwrap();
        std::fs::remove_file(trace).unwrap();
        std::fs::remove_file(log).unwrap();
        assert_eq!(import_seed(tmp.path(), Path::new("gone")).unwrap(), 2);
        assert_eq!(std::fs::read(seed).unwrap(), frozen);
        assert_eq!(traces(tmp.path()).len(), 2);
    }
}
