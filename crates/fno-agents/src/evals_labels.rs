//! The session label table: one append-only row per (session, judge,
//! judge_version) in `<state>/evals/labels.jsonl`, scored by a daily pass
//! that sends each finished session's trace snapshot to one judge on the
//! zai flash lane. A judge reads only the trace file, which `write_trace`
//! scrubbed at write time; the pass re-scrubs every string it adds.
//!
//! Proof state is never stored. A judge is proved only on hand labels,
//! computed on read, and its rows stay out of queries until then. The row
//! key skips a (session, judge, version, traces_digest) already answered,
//! so a trace that lands later rescores the session under a new digest and
//! every reader sees only the newest digest per key.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::event_store::{query_events, EventQuery};

const JUDGE_TIMEOUT_S: u64 = 240;
/// The judge lane: the cheap zai flash account, measured live to answer as
/// this model (the spawn is the golden drafter's one-shot shape plus the
/// account flag).
const MODEL: &str = "glm-5.3-flash";
const ACCOUNT: &str = "zai";
/// A finished session's transcript has been unwritten this long. Measured
/// over 825 resolved transcripts: none wrote again more than six hours
/// after the session's newest trace.
const QUIET_MS: i64 = 6 * 3600 * 1000;
const DEFAULT_MAX_SESSIONS: usize = 40;
const MAX_TRACES_PER_SESSION: usize = 8;
const TEXT_CHARS: usize = 400;
const NODE_TEXT_CHARS: usize = 600;
const REASON_CHARS: usize = 300;

/// One judge in the fixed v1 set. `version` bumps when its prompt changes,
/// so an old row never reads as the new judge's answer (the rubric
/// TEMPLATE_ID idea).
struct Judge {
    name: &'static str,
    version: &'static str,
    labels: &'static [&'static str],
    prompt: &'static str,
}

const JUDGES: &[Judge] = &[
    Judge {
        name: "user_corrected",
        version: "v1",
        labels: &["yes", "no"],
        prompt: include_str!("evals_labels/judges/user_corrected.md"),
    },
    Judge {
        name: "repeat_after_correction",
        version: "v1",
        labels: &["repeated", "not_repeated", "no_correction"],
        prompt: include_str!("evals_labels/judges/repeat_after_correction.md"),
    },
    Judge {
        name: "did_what_asked",
        version: "v1",
        labels: &["done", "partial", "not_done", "no_ask"],
        prompt: include_str!("evals_labels/judges/did_what_asked.md"),
    },
    Judge {
        name: "premise_checked",
        version: "v1",
        labels: &["checked", "unchecked", "not_applicable"],
        prompt: include_str!("evals_labels/judges/premise_checked.md"),
    },
    Judge {
        name: "failure_recovery",
        version: "v1",
        labels: &["recovered", "partial", "unresolved", "no_failure"],
        prompt: include_str!("evals_labels/judges/failure_recovery.md"),
    },
    Judge {
        name: "layer",
        version: "v1",
        labels: &["model", "harness", "unclear"],
        prompt: include_str!("evals_labels/judges/layer.md"),
    },
];

fn labels_path(state: &Path) -> PathBuf {
    state.join("evals").join("labels.jsonl")
}

fn read_rows(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn append_rows(path: &Path, rows: &[Value]) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut body = String::new();
    for row in rows {
        body.push_str(&row.to_string());
        body.push('\n');
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(body.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The newest-digest row per (session, judge, judge_version). Rows land in
/// append order, so the last row for a key carries the newest digest, and
/// this function is the only projection a reader needs.
pub(crate) fn newest_rows(rows: &[Value]) -> Vec<Value> {
    let mut out: BTreeMap<(String, String, String), Value> = BTreeMap::new();
    for row in rows {
        let key = (
            row.get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            row.get("judge")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            row.get("judge_version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
        if key.0.is_empty() || key.1.is_empty() {
            continue;
        }
        out.insert(key, row.clone());
    }
    out.into_values().collect()
}

fn fact_str(field: &Value) -> Option<&str> {
    field
        .get("value")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn scrub(text: &str) -> String {
    crate::intel_corpus::scrub_quote(text)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn parse_ms(stamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn trace_ms(trace: &Value) -> i64 {
    trace
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(|s| parse_ms(s))
        .unwrap_or(0)
}

fn mtime_ms(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
}

fn load_traces(state: &Path) -> Vec<Value> {
    let dir = state.join("evals").join("traces");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "json") {
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(trace) = serde_json::from_slice::<Value>(&bytes) {
                    out.push(trace);
                }
            }
        }
    }
    out
}

/// Every session with at least one trace, oldest session first, each
/// session's traces oldest first.
fn by_session(traces: &[Value]) -> Vec<(String, Vec<Value>)> {
    let mut map: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for trace in traces {
        if let Some(sid) = fact_str(&trace["session_id"]) {
            map.entry(sid.to_string()).or_default().push(trace.clone());
        }
    }
    let mut rows: Vec<(String, Vec<Value>)> = map.into_iter().collect();
    for (_, group) in rows.iter_mut() {
        group.sort_by_key(|t| trace_ms(t));
    }
    rows.sort_by_cached_key(|(sid, group)| (group.last().map(trace_ms).unwrap_or(0), sid.clone()));
    rows
}

fn termination_sessions(state: &Path) -> BTreeSet<String> {
    let journal = state.join("events.jsonl");
    let query = EventQuery::of_types(&["termination"]);
    let mut out = BTreeSet::new();
    for event in query_events(&journal, &query).unwrap_or_default() {
        if let Ok(line) = serde_json::from_str::<Value>(&event.line) {
            if let Some(sid) = line.pointer("/data/session_id").and_then(Value::as_str) {
                out.insert(sid.to_string());
            }
        }
    }
    out
}

/// A session is finished when a termination event names it, or its
/// transcript (resolved per harness) has sat unwritten past the quiet
/// window, or no transcript resolves and its newest trace predates the
/// window. A live session answers none of those, so it writes no row.
fn is_finished(sid: &str, traces: &[Value], terminated: &BTreeSet<String>, now: i64) -> bool {
    if terminated.contains(sid) {
        return true;
    }
    let harness = traces
        .iter()
        .find_map(|t| fact_str(&t["harness"]))
        .unwrap_or("claude");
    let quiet_before = now - QUIET_MS;
    match crate::context_run::session_transcript(sid, harness) {
        Some(path) => mtime_ms(&path).is_some_and(|m| m < quiet_before),
        None => traces.last().map(trace_ms).unwrap_or(0) < quiet_before,
    }
}

fn digest_of(ids: &[String]) -> String {
    let mut sorted = ids.to_vec();
    sorted.sort();
    blake3::hash(sorted.join("\n").as_bytes()).to_hex()[..16].to_string()
}

/// The node's title and details, one graph read per pass. An unknown node
/// id contributes nothing.
fn node_texts(ids: &BTreeSet<String>) -> BTreeMap<String, String> {
    if ids.is_empty() {
        return BTreeMap::new();
    }
    let graph = crate::backlog::settings::graph_path();
    let rows = crate::graph_store::read_rows(&graph).unwrap_or_default();
    let mut out = BTreeMap::new();
    for row in &rows {
        if let Some(id) = row.get("id").and_then(Value::as_str) {
            if ids.contains(id) {
                let title = row.get("title").and_then(Value::as_str).unwrap_or("");
                let details = row.get("details").and_then(Value::as_str).unwrap_or("");
                out.insert(
                    id.to_string(),
                    clip(&scrub(&format!("{title}\n{details}")), NODE_TEXT_CHARS),
                );
            }
        }
    }
    out
}

/// The judge's bounded view of one session: the traces' trigger and truth
/// quotes, their bounded tool calls and claims, any person-set label, and
/// the node text. Every string passes the scrub, however it was written.
fn evidence(
    sid: &str,
    harness: &str,
    model: Option<&str>,
    traces: &[Value],
    node_text: Option<&str>,
) -> Value {
    let text = |field: &Value| -> Value {
        match field.get("value") {
            Some(Value::String(s)) if !s.is_empty() => json!(clip(&scrub(s), TEXT_CHARS)),
            Some(v) if !v.is_null() => json!(clip(&scrub(&v.to_string()), TEXT_CHARS)),
            _ => Value::Null,
        }
    };
    let list = |field: &Value, key: &str| -> Value {
        match field.get("value").and_then(Value::as_array) {
            Some(items) => Value::Array(
                items
                    .iter()
                    .map(|item| match item {
                        Value::String(s) => json!(clip(&scrub(s), TEXT_CHARS)),
                        other => match other.get(key) {
                            Some(inner) => json!(clip(&scrub(&inner.to_string()), TEXT_CHARS)),
                            None => json!(clip(&scrub(&other.to_string()), TEXT_CHARS)),
                        },
                    })
                    .collect(),
            ),
            None => Value::Null,
        }
    };
    let rows: Vec<Value> = traces
        .iter()
        .take(MAX_TRACES_PER_SESSION)
        .map(|t| {
            json!({
                "id": t.get("id"),
                "signal_kind": t.get("signal_kind"),
                "trigger": text(&t["trigger"]),
                "truth": text(&t["truth"]),
                "tool_calls": list(&t["tool_calls"], "intent"),
                "claims": list(&t["claims"], "text"),
                "right_action": text(&t["right_action"]),
                "layer_verdict": text(&t["layer_verdict"]),
                "created_at": t.get("created_at"),
            })
        })
        .collect();
    json!({
        "session_id": sid,
        "harness": harness,
        "model": model,
        "node_text": node_text,
        "traces": rows,
    })
}

/// The first JSON object in a judge reply, read as (label, reason).
fn parse_label_reply(out: &str) -> Result<(String, String), String> {
    let (a, b) = (out.find('{'), out.rfind('}'));
    let (Some(a), Some(b)) = (a, b) else {
        return Err("no JSON object in the reply".into());
    };
    let reply: Value = serde_json::from_str(&out[a..=b])
        .map_err(|e| format!("reply is not the JSON asked for: {e}"))?;
    let label = reply
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if label.is_empty() {
        return Err("reply carries no label".into());
    }
    let reason = reply
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((label, reason))
}

/// One zai flash one-shot with read tools: the golden drafter's bounded
/// spawn shape plus the account flag. A hang dies at the kill bound, not
/// at the child's own timeout.
fn spawn_judge(name: &str, prompt: &str) -> Result<(String, String), String> {
    let cwd = crate::evals::canonical_repo_root();
    let argv: Vec<String> = [
        "agents",
        "spawn",
        "--name",
        name,
        prompt,
        "--harness",
        "claude",
        "--account",
        ACCOUNT,
        "--once",
        "--cwd",
        &cwd.display().to_string(),
        "--timeout",
        &JUDGE_TIMEOUT_S.to_string(),
        "--",
        "--model",
        MODEL,
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--tools",
        "Read,Grep,Glob",
        "--allowedTools",
        "Read,Grep,Glob",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let (code, out, err) = crate::blueprint_judge::headless_spawn(&argv, JUDGE_TIMEOUT_S + 30)?;
    if code != 0 {
        return Err(format!("spawn exit {code}: {}", err.trim()));
    }
    parse_label_reply(&out)
}

type SpawnJudge<'a> = &'a dyn Fn(&str, &str) -> Result<(String, String), String>;

/// One scoring pass. Finished, unscored sessions go oldest first under the
/// per-run cap; each judge answers once per (session, digest) and a failed
/// or unknown-label spawn writes no row, so the next run retries it.
pub(crate) fn score(
    state: &Path,
    now: i64,
    max_sessions: usize,
    spawn: SpawnJudge,
) -> Result<Value, String> {
    let path = labels_path(state);
    let dir = path.parent().ok_or("no state dir")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let done: BTreeSet<(String, String, String, String)> = read_rows(&path)
        .iter()
        .filter_map(|r| {
            Some((
                r.get("session_id")?.as_str()?.to_string(),
                r.get("judge")?.as_str()?.to_string(),
                r.get("judge_version")?.as_str()?.to_string(),
                r.get("traces_digest")?.as_str()?.to_string(),
            ))
        })
        .collect();
    let sessions = by_session(&load_traces(state));
    let terminated = termination_sessions(state);
    let mut node_ids = BTreeSet::new();
    for (_, group) in &sessions {
        for t in group {
            if let Some(node) = fact_str(&t["node"]) {
                node_ids.insert(node.to_string());
            }
        }
    }
    let texts = node_texts(&node_ids);
    let mut scored = 0usize;
    let mut written = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for (sid, group) in &sessions {
        if scored >= max_sessions {
            break;
        }
        if !is_finished(sid, group, &terminated, now) {
            continue;
        }
        let ids: Vec<String> = group
            .iter()
            .filter_map(|t| t.get("id").and_then(Value::as_str))
            .map(String::from)
            .collect();
        let digest = digest_of(&ids);
        let todo: Vec<&Judge> = JUDGES
            .iter()
            .filter(|j| {
                !done.contains(&(
                    sid.clone(),
                    j.name.to_string(),
                    j.version.to_string(),
                    digest.clone(),
                ))
            })
            .collect();
        if todo.is_empty() {
            continue;
        }
        scored += 1;
        let harness = group
            .iter()
            .find_map(|t| fact_str(&t["harness"]))
            .unwrap_or("claude");
        let model = group.iter().find_map(|t| fact_str(&t["model"]));
        let node_text = group
            .iter()
            .find_map(|t| fact_str(&t["node"]))
            .and_then(|n| texts.get(n))
            .cloned();
        let view = evidence(sid, harness, model, group, node_text.as_deref());
        let body = serde_json::to_string(&view).map_err(|e| e.to_string())?;
        for judge in todo {
            let hash =
                blake3::hash(format!("{sid}|{}", judge.name).as_bytes()).to_hex()[..12].to_string();
            let name = format!("eval-judge-{}-{hash}", judge.name);
            let prompt = format!("{}\n\nSESSION EVIDENCE:\n{body}", judge.prompt);
            match spawn(&name, &prompt) {
                Ok((label, reason)) if judge.labels.contains(&label.as_str()) => {
                    let newest = group.last();
                    let span_ref = format!(
                        "{}/{}",
                        newest
                            .and_then(|t| t.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("?"),
                        newest
                            .and_then(|t| fact_str(&t["source_ref"]))
                            .unwrap_or("?"),
                    );
                    append_rows(
                        &path,
                        &[json!({
                            "session_id": sid,
                            "judge": judge.name,
                            "judge_version": judge.version,
                            "label": label,
                            "reason": clip(&scrub(&reason), REASON_CHARS),
                            "span_ref": span_ref,
                            "traces_digest": digest,
                            "created_at": chrono::Utc::now().to_rfc3339(),
                            "model": MODEL,
                        })],
                    )?;
                    written += 1;
                }
                Ok((label, _)) => {
                    failed.push(format!("{sid}/{}: unknown label {label:?}", judge.name))
                }
                Err(e) => failed.push(format!("{sid}/{}: {}", judge.name, clip(&e, 120))),
            }
        }
    }
    let finished = sessions
        .iter()
        .filter(|(sid, group)| is_finished(sid, group, &terminated, now))
        .count();
    Ok(json!({
        "finished": finished,
        "scored": scored,
        "rows": written,
        "failed": failed.len(),
        "failed_sample": failed.iter().rev().take(5).cloned().collect::<Vec<_>>(),
        "cap": max_sessions,
        "fixer": match fixer(state) {
            Ok(v) => v,
            Err(e) => json!({"error": clip(&e, 120)}),
        },
    }))
}

fn max_sessions(repo: &Path) -> usize {
    match crate::agents_config::config_value_deep(
        repo,
        &["evals", "labels", "max_sessions_per_day"],
    ) {
        Some(toml::Value::Integer(n)) if n >= 0 => n as usize,
        _ => DEFAULT_MAX_SESSIONS,
    }
}

fn daily(state: &Path) -> Result<Value, String> {
    let repo = crate::evals::canonical_repo_root();
    score(state, now_ms(), max_sessions(&repo), &spawn_judge)
}

/// The categorical recall floor a judge's hand pairs must clear per label
/// class before the judge reads proved (the binary bar stays rubric.rs).
const PASS_RECALL: f64 = 0.8;

fn judge_by_name(name: &str) -> Option<&'static Judge> {
    JUDGES.iter().find(|j| j.name == name)
}

/// The hand labels a person set on traces: `layer_verdict` values for the
/// layer judge, a set `right_action` for user_corrected. Nothing else has
/// a hand slot in v1.
fn hand_labels(state: &Path) -> Vec<(String, &'static str, String)> {
    let mut out = Vec::new();
    for t in load_traces(state) {
        let Some(sid) = fact_str(&t["session_id"]) else {
            continue;
        };
        let layer = &t["layer_verdict"];
        if layer.get("basis").and_then(Value::as_str) == Some("user_set") {
            if let Some(v) = layer.get("value").and_then(Value::as_str) {
                if let Some(judge) = judge_by_name("layer") {
                    if judge.labels.contains(&v) {
                        out.push((sid.to_string(), "layer", v.to_string()));
                    }
                }
            }
        }
        let right = &t["right_action"];
        if right.get("basis").and_then(Value::as_str) == Some("user_set") {
            out.push((sid.to_string(), "user_corrected", "yes".to_string()));
        }
    }
    out
}

/// Proof state, computed on read and never stored. Binary judges clear the
/// rubric bar (n, agreement and kappa, so a one-class hand set never
/// proves); categorical judges clear a per-label recall floor over hand
/// pairs with both classes present.
pub(crate) fn calibration_rows(state: &Path) -> Vec<Value> {
    let traces = load_traces(state);
    let seed_sessions: BTreeSet<String> = traces
        .iter()
        .filter(|t| t.get("signal_kind").and_then(Value::as_str) == Some("seed_correction"))
        .filter_map(|t| fact_str(&t["session_id"]))
        .map(String::from)
        .collect();
    let rows = newest_rows(&read_rows(&labels_path(state)));
    let verdict = |sessions: &BTreeSet<String>, judge: &str| -> BTreeMap<String, String> {
        rows.iter()
            .filter(|r| r.get("judge").and_then(Value::as_str) == Some(judge))
            .filter_map(|r| {
                let sid = r.get("session_id").and_then(Value::as_str)?;
                let label = r.get("label").and_then(Value::as_str)?;
                sessions
                    .contains(sid)
                    .then(|| (sid.to_string(), label.to_string()))
            })
            .collect()
    };
    let mut out = Vec::new();
    for judge in JUDGES {
        let mut row = json!({
            "judge": judge.name,
            "seed_n": Value::Null,
            "seed_scored": Value::Null,
            "seed_recall": Value::Null,
            "hand_n": 0,
            "agreement": Value::Null,
            "kappa": Value::Null,
            "per_label": Value::Null,
            "proved": false,
        });
        if judge.name == "user_corrected" {
            let verdicts = verdict(&seed_sessions, judge.name);
            let mut pairs: Vec<(bool, bool)> = Vec::new();
            for sid in &seed_sessions {
                if let Some(label) = verdicts.get(sid) {
                    pairs.push((true, label == "yes"));
                }
            }
            let lane = crate::evals::rubric::lane_agreement("flash", &pairs);
            let scored = pairs.len();
            let yes = pairs.iter().filter(|(_, v)| *v).count();
            row["seed_n"] = json!(seed_sessions.len());
            row["seed_scored"] = json!(scored);
            row["seed_recall"] = if scored > 0 {
                json!(yes as f64 / scored as f64)
            } else {
                Value::Null
            };
            row["hand_n"] = json!(seed_sessions.len());
            row["agreement"] = lane.agreement().map(Value::from).unwrap_or(Value::Null);
            row["kappa"] = lane.kappa.map(Value::from).unwrap_or(Value::Null);
            row["proved"] = json!(crate::evals::rubric::agreement_proved(&[lane]));
        } else if judge.name == "layer" {
            let mut hand: Vec<(String, String)> = hand_labels(state)
                .into_iter()
                .filter(|(_, judge_name, _)| *judge_name == "layer")
                .map(|(sid, _, label)| (sid, label))
                .collect();
            hand.sort();
            hand.dedup();
            let verdicts = verdict(&hand.iter().map(|(s, _)| s.clone()).collect(), "layer");
            let mut hits: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
            let mut agreed = 0u32;
            for (sid, hand_label) in &hand {
                if let Some(label) = verdicts.get(sid) {
                    let cell = hits.entry(hand_label.as_str()).or_default();
                    cell.1 += 1;
                    cell.0 += u32::from(label == hand_label);
                    agreed += u32::from(label == hand_label);
                }
            }
            let per_label: BTreeMap<String, Value> = hits
                .iter()
                .map(|(label, (hit, total))| {
                    let ratio = f64::from(*hit) / f64::from(*total.max(&1));
                    (label.to_string(), json!(ratio))
                })
                .collect();
            let classes = hits.len();
            let proved = hand.len() >= 2
                && classes >= 2
                && hits
                    .values()
                    .all(|(hit, total)| f64::from(*hit) / f64::from(*total.max(&1)) >= PASS_RECALL);
            row["hand_n"] = json!(hand.len());
            row["agreement"] = if hand.is_empty() {
                Value::Null
            } else {
                json!(agreed as f64 / hand.len() as f64)
            };
            row["per_label"] = Value::Object(per_label.into_iter().collect());
            row["proved"] = json!(proved);
        }
        out.push(row);
    }
    out
}

fn since_ms(spec: &str) -> Option<i64> {
    let mut chars = spec.chars();
    let unit = chars.next_back()?;
    let n: i64 = chars.as_str().parse().ok()?;
    let secs = match unit {
        'd' => n * 86_400,
        'h' => n * 3_600,
        'm' => n * 60,
        's' => n,
        _ => return None,
    };
    Some(now_ms() - secs * 1000)
}

/// The query behind `evals candidates --labels`: newest-digest rows only,
/// unproved judges' rows excluded with a count unless `--unproved`, and no
/// transcript is read anywhere on this path.
fn query(
    state: &Path,
    judge: Option<&String>,
    label: Option<&String>,
    session: Option<&String>,
    since: Option<i64>,
    unproved: bool,
    limit: usize,
) -> (Vec<Value>, usize) {
    let proved: BTreeSet<String> = calibration_rows(state)
        .iter()
        .filter(|j| j["proved"] == json!(true))
        .filter_map(|j| j["judge"].as_str().map(String::from))
        .collect();
    let mut excluded = 0usize;
    let mut matching: Vec<Value> = Vec::new();
    for row in newest_rows(&read_rows(&labels_path(state))) {
        let name = row.get("judge").and_then(Value::as_str).unwrap_or("");
        if !proved.contains(name) && !unproved {
            excluded += 1;
            continue;
        }
        if let Some(want) = judge {
            if row.get("judge").and_then(Value::as_str) != Some(want.as_str()) {
                continue;
            }
        }
        if let Some(want) = label {
            if row.get("label").and_then(Value::as_str) != Some(want.as_str()) {
                continue;
            }
        }
        if let Some(want) = session {
            if row.get("session_id").and_then(Value::as_str) != Some(want.as_str()) {
                continue;
            }
        }
        if let Some(cutoff) = since {
            let at = row
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(parse_ms)
                .unwrap_or(0);
            if at < cutoff {
                continue;
            }
        }
        matching.push(json!({
            "session_id": row.get("session_id"),
            "judge": row.get("judge"),
            "judge_version": row.get("judge_version"),
            "label": row.get("label"),
            "reason": row.get("reason"),
            "span_ref": row.get("span_ref"),
            "created_at": row.get("created_at"),
        }));
        if matching.len() >= limit {
            break;
        }
    }
    (matching, excluded)
}

pub fn run(args: &[String]) -> i32 {
    let state = crate::evals_candidates::state_root();
    let json = crate::json_output::requested(args);
    if args.iter().any(|a| a == "--calibration") {
        let judges = calibration_rows(&state);
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"judges": judges})).unwrap_or_default()
            );
        } else {
            for judge in &judges {
                println!(
                    "{}\tproved={} seed_n={} hand_n={} agreement={} kappa={}",
                    judge["judge"].as_str().unwrap_or("?"),
                    judge["proved"],
                    judge["seed_n"],
                    judge["hand_n"],
                    judge["agreement"],
                    judge["kappa"],
                );
            }
        }
        return 0;
    }
    if args.iter().any(|a| a == "--pass") {
        let repo = crate::evals::canonical_repo_root();
        return match score(&state, now_ms(), max_sessions(&repo), &spawn_judge) {
            Ok(receipt) => {
                println!("{receipt}");
                0
            }
            Err(e) => {
                eprintln!("eval labels: {e}");
                1
            }
        };
    }
    let flag = |name: &str| crate::evals::flag_lookup(args, name);
    let judge = flag("--judge");
    let label = flag("--label");
    let session = flag("--session");
    let since = flag("--since").and_then(|s| since_ms(&s));
    let unproved = args.iter().any(|a| a == "--unproved");
    let limit = match flag("--limit") {
        None => 50,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("usage: fno-agents evals candidates --labels [--judge J] [--label L] [--session S] [--since 1d] [--calibration] [--pass] [--unproved] [--limit N] [--json]");
                return 2;
            }
        },
    };
    let (matching, excluded) = query(
        &state,
        judge.as_ref(),
        label.as_ref(),
        session.as_ref(),
        since,
        unproved,
        limit,
    );
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"rows": matching, "excluded_unproved": excluded}))
                .unwrap_or_default()
        );
    } else {
        for row in &matching {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                row["session_id"].as_str().unwrap_or("?"),
                row["judge"].as_str().unwrap_or("?"),
                row["label"].as_str().unwrap_or("?"),
                row["span_ref"].as_str().unwrap_or("?"),
                row["reason"].as_str().unwrap_or(""),
            );
        }
        if excluded > 0 {
            println!("excluded {excluded} rows of unproved judges (pass --unproved to read them)");
        }
    }
    0
}

/// The proved-label facts for one session.
#[derive(Default, Debug)]
pub(crate) struct SessionLabels {
    pub expected: String,
    pub failure_type: String,
    pub layer: String,
    pub span_ref: String,
}

/// The proved-label enrichment for each session with at least one proved
/// row: expected from the judgment reason (user_corrected, then
/// did_what_asked), layer from the layer judge, failure_type from the
/// newest trace's signal kind, and the anchor span. Only proved judges'
/// rows feed it, so an unproved judge never enriches a draft.
pub(crate) fn label_facts(state: &Path) -> BTreeMap<String, SessionLabels> {
    let proved: BTreeSet<String> = calibration_rows(state)
        .iter()
        .filter(|j| j["proved"] == json!(true))
        .filter_map(|j| j["judge"].as_str().map(str::to_string))
        .collect();
    let rows = newest_rows(&read_rows(&labels_path(state)));
    let kinds: BTreeMap<String, String> = by_session(&load_traces(state))
        .into_iter()
        .filter_map(|(sid, g)| {
            g.last()
                .and_then(|t| t.get("signal_kind"))
                .and_then(Value::as_str)
                .map(|k| (sid, k.to_string()))
        })
        .collect();
    let mut out: BTreeMap<String, SessionLabels> = BTreeMap::new();
    for row in rows {
        let judge = row.get("judge").and_then(Value::as_str).unwrap_or("");
        let sid = row.get("session_id").and_then(Value::as_str).unwrap_or("");
        if judge.is_empty() || sid.is_empty() || !proved.contains(judge) {
            continue;
        }
        let reason = row.get("reason").and_then(Value::as_str).unwrap_or("");
        let label = row.get("label").and_then(Value::as_str).unwrap_or("");
        let span = row.get("span_ref").and_then(Value::as_str).unwrap_or("");
        let entry = out.entry(sid.to_string()).or_default();
        match judge {
            "user_corrected" if entry.expected.is_empty() => {
                entry.expected = clip(&scrub(reason), 200)
            }
            "did_what_asked" if entry.expected.is_empty() => {
                entry.expected = clip(&scrub(reason), 200)
            }
            "layer" => entry.layer = label.to_string(),
            _ => {}
        }
        if !span.is_empty() {
            entry.span_ref = span.to_string();
        }
    }
    for (sid, kind) in &kinds {
        if let Some(entry) = out.get_mut(sid) {
            entry.failure_type = kind.clone();
        }
    }
    out
}

/// The go-N fixer: proved layer rows cluster by (layer, the newest trace's
/// signal kind) and file one go node per cluster carrying the sessions and
/// span refs, or append new evidence to the open node for that cluster. A
/// cluster never opens a PR and never dispatches; a person reads it. With
/// no proved layer judge it files nothing and names what it would file.
pub(crate) fn fixer(state: &Path) -> Result<Value, String> {
    let proved_layer = calibration_rows(state)
        .iter()
        .any(|j| j["judge"] == json!("layer") && j["proved"] == json!(true));
    let kinds: BTreeMap<String, String> = by_session(&load_traces(state))
        .into_iter()
        .filter_map(|(sid, g)| {
            g.last()
                .and_then(|t| t.get("signal_kind"))
                .and_then(Value::as_str)
                .map(|k| (sid, k.to_string()))
        })
        .collect();
    let mut clusters: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for row in newest_rows(&read_rows(&labels_path(state))) {
        if row.get("judge").and_then(Value::as_str) != Some("layer") {
            continue;
        }
        let Some(label) = row.get("label").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(label, "model" | "harness") {
            continue;
        }
        let sid = row.get("session_id").and_then(Value::as_str).unwrap_or("");
        let kind = kinds.get(sid).map(String::as_str).unwrap_or("unknown");
        let key = (label.to_string(), kind.to_string());
        clusters.entry(key).or_default().push(format!(
            "session {sid} span {}",
            row.get("span_ref").and_then(Value::as_str).unwrap_or("?")
        ));
    }
    let names: Vec<String> = clusters
        .keys()
        .map(|(l, k)| format!("layer={l} kind={k}"))
        .collect();
    if !proved_layer {
        return Ok(json!({"filed": 0, "would_file": names}));
    }
    let graph = crate::backlog::settings::graph_path();
    let parent = crate::agents_config::config_value_deep(
        &crate::evals::canonical_repo_root(),
        &["evals", "labels", "go_parent"],
    )
    .and_then(|v| v.as_str().map(str::to_string));
    let mut filed = 0usize;
    let mut appended: Vec<String> = Vec::new();
    for ((label, kind), lines) in &clusters {
        if lines.is_empty() {
            continue;
        }
        let marker = format!("cluster: layer={label} kind={kind}");
        // The append carries the evidence lines alone, so a rerun over
        // identical evidence dedupes on the tail and writes nothing.
        let lines_text = lines.join("\n");
        let body = format!("{marker}\n{lines_text}");
        let open = crate::graph_store::read_rows(&graph)
            .unwrap_or_default()
            .into_iter()
            .find(|r| {
                r.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("go-"))
                    && r.get("completed_at").is_none_or(Value::is_null)
                    && r.get("details")
                        .and_then(Value::as_str)
                        .is_some_and(|d| d.contains(&marker))
            });
        if let Some(node) = open {
            let id = node
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                continue;
            }
            let mut changed = false;
            crate::graph_store::mutate_rows(
                &graph,
                crate::graph_store::DEFAULT_LOCK_TIMEOUT,
                None,
                None,
                |rows| {
                    if let Some(row) = rows
                        .iter_mut()
                        .find(|r| r.get("id").and_then(Value::as_str) == Some(id.as_str()))
                    {
                        let existing = row
                            .get("details")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        if let Some(next) = crate::backlog::update_ops::append_text(
                            existing.as_deref(),
                            &lines_text,
                        ) {
                            row["details"] = json!(next);
                            changed = true;
                            return Ok(true);
                        }
                    }
                    Ok(false)
                },
            )
            .map_err(|e| e.to_string())?;
            if changed {
                appended.push(id);
            }
            continue;
        }
        let here = std::env::current_dir().unwrap_or_default();
        let mut minted = String::new();
        crate::graph_store::mutate_rows(
            &graph,
            crate::graph_store::DEFAULT_LOCK_TIMEOUT,
            None,
            None,
            |rows| {
                let known: BTreeSet<String> = rows
                    .iter()
                    .filter_map(|r| r.get("id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect();
                let id = crate::backlog::create_cli::mint_node_id_prefixed(&known, "go")
                    .map_err(crate::graph_store::StoreError::Invalid)?;
                let now = crate::graph_store::now_isoformat();
                let (origin, evidence, _) =
                    crate::backlog::create_cli::stamp_request_origin("organic", "eval_fixer", None);
                let (sid, harness, scwd, snode, splan, _dropped) =
                    crate::backlog::create_cli::session_provenance(&here, None, Some(&known));
                let node = json!({
                    "id": id,
                    "parent": parent.clone(),
                    "tags": [],
                    "title": format!("go: fix {label} layer {kind} failures"),
                    "type": "bug",
                    "project": None::<String>,
                    "cwd": None::<String>,
                    "status": "idea",
                    "priority": "p2",
                    "blocks_everything": false,
                    "difficulty": "low",
                    "difficulty_history": [{"value": "low", "source": "eval_fixer", "ts": now}],
                    "domain": "code",
                    "blocked_by": [],
                    "session_id": null,
                    "locked_at": null,
                    "completed_at": null,
                    "has_brief": false,
                    "roadmap_id": null,
                    "vision_path": null,
                    "details": body,
                    "size": null,
                    "batch": null,
                    "cost_usd": null,
                    "cost_sessions": [],
                    "plan_path": null,
                    "pr_number": null,
                    "pr_url": null,
                    "merge_status": null,
                    "created_at": now,
                    "source": null,
                    "source_kind": "organic",
                    "source_project": null,
                    "source_inbox_msg": null,
                    "artifact_url": null,
                    "completion_note": null,
                    "source_session_id": sid,
                    "source_harness": harness,
                    "source_cwd": scwd,
                    "source_node_id": snode,
                    "source_plan_path": splan,
                    "request_origin": origin,
                    "origin_evidence": evidence,
                });
                minted = id;
                rows.push(node);
                Ok(true)
            },
        )
        .map_err(|e| e.to_string())?;
        if !minted.is_empty() {
            filed += 1;
        }
    }
    Ok(json!({"filed": filed, "appended": appended, "clusters": names}))
}

#[derive(Default)]
pub struct Arm {
    next: Arc<Mutex<Option<Instant>>>,
    in_flight: Arc<AtomicBool>,
}

/// The fleet tick's entry: once a day, one pass in flight, off the core
/// loop. A failed pass retries in five minutes.
pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    {
        let mut next = arm.next.lock().unwrap_or_else(|e| e.into_inner());
        if next.is_some_and(|t| t > Instant::now()) || arm.in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        *next = Some(Instant::now() + Duration::from_secs(86400));
    }
    let flag = Arc::clone(&arm.in_flight);
    let next = Arc::clone(&arm.next);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Some(state) = home.root().parent() {
            if let Err(e) = daily(state) {
                eprintln!("eval labels: {e}");
                *next.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Instant::now() + Duration::from_secs(300));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace_file(state: &Path, sid: &str, kind: &str, created: &str, trigger: &str) -> String {
        let id = blake3::hash(format!("t|{sid}|{kind}|{created}|{trigger}").as_bytes()).to_hex()
            [..16]
            .to_string();
        let trace = json!({
            "id": id,
            "signal_kind": kind,
            "trigger": {"value": trigger, "basis": "event"},
            "truth": {"value": "the stated reason", "basis": "event"},
            "tool_calls": {"value": [{"tool": "Bash", "intent": "cargo test"}], "basis": "transcript_before_turn"},
            "claims": {"value": ["it is green"], "basis": "transcript_before_turn"},
            "right_action": {"value": null, "basis": "unmeasured"},
            "layer_verdict": {"value": null, "basis": "unmeasured"},
            "source_ref": {"value": "ev:1", "basis": "source_identity"},
            "node": {"value": Value::Null, "basis": "unmeasured"},
            "model": {"value": "glm-5.3-flash", "basis": "event"},
            "harness": {"value": "claude", "basis": "event"},
            "session_id": {"value": sid, "basis": "event"},
            "created_at": created,
        });
        let dir = state.join("evals").join("traces");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{id}.json")), trace.to_string()).unwrap();
        id
    }

    fn fake_labels(
        judge_override: Option<(&'static str, &'static str)>,
    ) -> impl Fn(&str, &str) -> Result<(String, String), String> {
        move |name: &str, _: &str| {
            let rest = name.strip_prefix("eval-judge-").unwrap_or(name);
            let judge = rest.rsplit_once('-').map(|(j, _)| j).unwrap_or(rest);
            if let Some((bad, label)) = judge_override {
                if judge == bad {
                    return Ok((label.to_string(), "because test".into()));
                }
            }
            let label = match judge {
                "user_corrected" => "yes",
                "repeat_after_correction" => "repeated",
                "did_what_asked" => "done",
                "premise_checked" => "checked",
                "failure_recovery" => "recovered",
                "layer" => "harness",
                other => return Err(format!("fake spawn asked for {other}")),
            };
            Ok((label.to_string(), "because test".into()))
        }
    }

    fn rows(state: &Path) -> Vec<Value> {
        read_rows(&labels_path(state))
    }

    #[test]
    fn finished_sessions_score_once_and_a_live_session_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let old = "2026-11-01T00:00:00+00:00";
        // Two hours before `now`: inside the six-hour quiet window, so the
        // session stays live unless a termination event names it.
        let fresh = "2026-11-05T10:00:00+00:00";
        trace_file(state, "s-old", "user_correction", old, "no, prove it");
        trace_file(state, "s-fresh", "user_correction", fresh, "no, prove it");
        trace_file(state, "s-term", "user_correction", fresh, "stop re-ranking");
        let journal = state.join("events.jsonl");
        crate::event_store::append_envelope(
            &journal,
            &json!({"ts": "2026-11-05T01:00:00Z", "type": "termination", "source": "hook",
                    "data": {"session_id": "s-term", "reason": "Budget", "message": "run ended"}})
            .to_string(),
            None,
        )
        .unwrap();
        let now = parse_ms("2026-11-05T12:00:00+00:00").unwrap();
        let receipt = score(state, now, 40, &fake_labels(None)).unwrap();
        assert_eq!(receipt["rows"], 12, "{receipt}");
        assert_eq!(
            receipt["scored"], 2,
            "only the old and the terminated session"
        );
        assert_eq!(receipt["failed"], 0);
        assert_eq!(rows(state).len(), 12);
        let again = score(state, now, 40, &fake_labels(None)).unwrap();
        assert_eq!(again["rows"], 0, "a second pass is a no-op: {again}");
        assert_eq!(rows(state).len(), 12);
    }

    #[test]
    fn unknown_label_writes_no_row_and_a_later_trace_rescores_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let created = "2026-11-01T00:00:00+00:00";
        let id1 = trace_file(state, "s-old", "user_correction", created, "no, prove it");
        let now = parse_ms("2026-11-05T12:00:00+00:00").unwrap();
        let receipt = score(state, now, 40, &fake_labels(Some(("layer", "maybe")))).unwrap();
        assert_eq!(receipt["rows"], 5, "{receipt}");
        assert_eq!(receipt["failed"], 1);
        assert!(rows(state).iter().all(|r| r["judge"] != "layer"));
        let retry = score(state, now, 40, &fake_labels(None)).unwrap();
        assert_eq!(
            retry["rows"], 1,
            "the unknown-label judge is retried: {retry}"
        );
        assert_eq!(
            newest_rows(&rows(state)).len(),
            6,
            "one newest row per judge"
        );
        // A trace that lands later moves the digest and rescores the session.
        let id2 = trace_file(state, "s-old", "no_progress", created, "same stall");
        let third = score(state, now, 40, &fake_labels(None)).unwrap();
        assert_eq!(third["rows"], 6, "{third}");
        let expected = digest_of(&[id1, id2]);
        let latest = newest_rows(&rows(state));
        assert_eq!(latest.len(), 6);
        assert!(
            latest.iter().all(|r| r["traces_digest"] == json!(expected)),
            "the newest rows carry the new digest"
        );
    }

    #[test]
    fn evidence_is_scrubbed_clipped_and_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let id = trace_file(
            state,
            "s-old",
            "user_correction",
            "2026-11-01T00:00:00+00:00",
            "mail me at a@b.co then prove it",
        );
        let traces = load_traces(state);
        assert_eq!(traces.len(), 1);
        let view = evidence("s-old", "claude", Some("glm-5.3-flash"), &traces, None);
        let body = view.to_string();
        assert!(!body.contains("a@b.co"), "{body}");
        assert!(body.contains(&id), "the trace id anchors the evidence");
        assert_eq!(view["traces"].as_array().unwrap().len(), 1);
        let long = "x".repeat(2_000);
        let mut group = traces.clone();
        group[0]["trigger"] = json!({"value": long, "basis": "event"});
        let clipped = evidence("s-old", "claude", None, &group, None);
        assert!(clipped["traces"][0]["trigger"].as_str().unwrap().len() <= TEXT_CHARS);
    }

    #[test]
    fn newest_rows_keeps_one_row_per_session_judge_and_version() {
        let row = |digest: &str, at: &str| {
            json!({"session_id": "s1", "judge": "layer", "judge_version": "v1",
                   "label": "harness", "reason": "r", "span_ref": "t/ev",
                   "traces_digest": digest, "created_at": at})
        };
        let rows = vec![
            row("aaa", "2026-11-01T00:00:00+00:00"),
            row("bbb", "2026-11-02T00:00:00+00:00"),
        ];
        let newest = newest_rows(&rows);
        assert_eq!(newest.len(), 1);
        assert_eq!(newest[0]["traces_digest"], json!("bbb"));
        let mut other = row("aaa", "2026-11-01T00:00:00+00:00");
        other["judge"] = json!("user_corrected");
        assert_eq!(newest_rows(&[rows[0].clone(), other]).len(), 2);
    }

    #[test]
    fn a_reply_without_json_or_label_is_a_judge_fault() {
        assert!(parse_label_reply("no braces here").is_err());
        assert!(parse_label_reply("{\"verdict\": 1}").is_err());
        let (label, reason) =
            parse_label_reply("sure: {\"label\": \"yes\", \"reason\": \"r\"}").unwrap();
        assert_eq!((label.as_str(), reason.as_str()), ("yes", "r"));
    }

    #[test]
    fn calibration_reads_seeds_as_user_corrected_positives_and_layer_hand_labels() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        for sid in ["sd-one", "sd-two", "sd-three"] {
            trace_file(
                state,
                sid,
                "seed_correction",
                "2026-11-01T00:00:00+00:00",
                "no, prove it",
            );
        }
        let now = parse_ms("2026-11-05T12:00:00+00:00").unwrap();
        score(state, now, 40, &fake_labels(None)).unwrap();
        let judges: BTreeMap<String, Value> = calibration_rows(state)
            .into_iter()
            .map(|j| (j["judge"].as_str().unwrap_or("").to_string(), j))
            .collect();
        let uc = &judges["user_corrected"];
        assert_eq!(uc["seed_n"], 3);
        assert_eq!(uc["seed_scored"], 3);
        assert_eq!(uc["seed_recall"], json!(1.0));
        assert_eq!(uc["kappa"], Value::Null, "one hand class proves no kappa");
        assert_eq!(uc["proved"], json!(false));
        assert_eq!(judges["layer"]["hand_n"], 0);
        assert_eq!(judges["layer"]["proved"], json!(false));
        // A person-set layer verdict becomes the hand pair the gate reads.
        let id = trace_file(
            state,
            "sd-one",
            "user_correction",
            "2026-11-02T00:00:00+00:00",
            "then it broke again",
        );
        let dir = state.join("evals").join("traces");
        let path = dir.join(format!("{id}.json"));
        let mut labeled =
            serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap();
        labeled["layer_verdict"] = json!({"value": "model", "basis": "user_set"});
        std::fs::write(&path, labeled.to_string()).unwrap();
        score(state, now, 40, &fake_labels(None)).unwrap();
        let judges: BTreeMap<String, Value> = calibration_rows(state)
            .into_iter()
            .map(|j| (j["judge"].as_str().unwrap_or("").to_string(), j))
            .collect();
        assert_eq!(judges["layer"]["hand_n"], 1);
        assert_eq!(judges["layer"]["proved"], json!(false));
    }

    /// Pins FNO_STATE_DIR for the duration of one test and restores it on
    /// drop, so the fixer's graph writes land in the test's tempdir.
    struct PinState(Option<std::ffi::OsString>);
    impl PinState {
        fn new(path: &Path) -> Self {
            let prior = std::env::var_os("FNO_STATE_DIR");
            std::env::set_var("FNO_STATE_DIR", path);
            Self(prior)
        }
    }
    impl Drop for PinState {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("FNO_STATE_DIR", v),
                None => std::env::remove_var("FNO_STATE_DIR"),
            }
        }
    }

    fn stamp_hand(state: &Path, id: &str, hand: &str) {
        let path = state
            .join("evals")
            .join("traces")
            .join(format!("{id}.json"));
        let mut t =
            serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap();
        t["layer_verdict"] = json!({"value": hand, "basis": "user_set"});
        std::fs::write(&path, t.to_string()).unwrap();
    }

    fn label_row(sid: &str, label: &str, digest: &str) -> Value {
        json!({
            "session_id": sid, "judge": "layer", "judge_version": "v1",
            "label": label, "reason": "test", "span_ref": format!("t-{sid}/ev"),
            "traces_digest": digest, "created_at": "2026-11-02T00:00:00+00:00",
            "model": "glm-5.3-flash",
        })
    }

    fn go_nodes() -> Vec<Value> {
        let graph = crate::backlog::settings::graph_path();
        crate::graph_store::read_rows(&graph)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                r.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("go-"))
            })
            .collect()
    }

    #[test]
    fn the_fixer_files_nothing_while_the_layer_judge_is_unproved() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let state = state.as_path();
        let _pin = PinState::new(&state);
        for sid in ["ha-one", "ha-two"] {
            trace_file(state, sid, "no_progress", "2026-11-01T00:00:00+00:00", "q");
        }
        append_rows(
            &labels_path(state),
            &[
                label_row("ha-one", "harness", "d1"),
                label_row("ha-two", "harness", "d1"),
                label_row("mo-one", "model", "d1"),
            ],
        )
        .unwrap();
        let receipt = fixer(state).unwrap();
        assert_eq!(receipt["filed"], 0, "{receipt}");
        assert_eq!(receipt["would_file"].as_array().unwrap().len(), 2);
        assert!(go_nodes().is_empty());
    }

    #[test]
    fn the_fixer_files_one_go_node_per_cluster_and_appends_on_a_rerun() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let state = state.as_path();
        let _pin = PinState::new(&state);
        for (sid, kind, hand) in [
            ("mo-one", "user_correction", "model"),
            ("mo-two", "user_correction", "model"),
            ("ha-one", "no_progress", "harness"),
            ("ha-two", "no_progress", "harness"),
        ] {
            let id = trace_file(state, sid, kind, "2026-11-01T00:00:00+00:00", "q");
            stamp_hand(state, &id, hand);
        }
        append_rows(
            &labels_path(state),
            &[
                label_row("mo-one", "model", "d1"),
                label_row("mo-two", "model", "d1"),
                label_row("ha-one", "harness", "d1"),
                label_row("ha-two", "harness", "d1"),
            ],
        )
        .unwrap();
        let receipt = fixer(state).unwrap();
        assert_eq!(receipt["filed"], 2, "{receipt}");
        assert_eq!(go_nodes().len(), 2);
        // A rerun with identical evidence appends nothing.
        let again = fixer(state).unwrap();
        assert_eq!(again["filed"], 0);
        assert_eq!(again["appended"], json!([]));
        assert_eq!(go_nodes().len(), 2);
        // A new session in one cluster joins the open node.
        let _id3 = trace_file(
            state,
            "ha-three",
            "no_progress",
            "2026-11-02T00:00:00+00:00",
            "q",
        );
        append_rows(
            &labels_path(state),
            &[label_row("ha-three", "harness", "d2")],
        )
        .unwrap();
        let third = fixer(state).unwrap();
        assert_eq!(third["filed"], 0, "{third}");
        assert_eq!(go_nodes().len(), 2);
        let details = go_nodes()
            .iter()
            .find(|r| {
                r.get("details")
                    .and_then(Value::as_str)
                    .is_some_and(|d| d.contains("kind=no_progress"))
            })
            .and_then(|r| r.get("details"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            details.contains("ha-three") && details.contains("ha-one"),
            "{details}"
        );
    }
}
