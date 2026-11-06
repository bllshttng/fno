//! `fno agents loop consult`: a worker asks a stronger model one bounded
//! question and continues. The caller's own transcript is trimmed to its last
//! turns, one headless frontier one-shot the config names reads the digest
//! file, and the call is recorded as a `consult_call` event whose
//! per-session count is the cap. Default state is off: `agents.consult.lanes`
//! empty refuses every real call, so only the lanes the lead names ever
//! reach the model.

use crate::event_store::{EventQuery, EventRow};
use crate::paths::AgentsHome;
use crate::transcript_resolve::{resolve_transcript, ResolvedTranscript, Roots};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

const CONSULT_PROMPT: &str = include_str!("consult.md");
const DEFAULT_MAX_CALLS: i64 = 2;
const DEFAULT_MODEL: &str = "opus";
const DEFAULT_CONTEXT_BYTES: i64 = 400_000;
const SPAWN_TIMEOUT_SECS: u64 = 600;

const EXIT_SPAWN_FAILED: i32 = 1;
const EXIT_CAP: i32 = 3;
const EXIT_NO_TRANSCRIPT: i32 = 4;
const EXIT_LANE_REFUSED: i32 = 5;

/// One question: text, optional node, output shape, and the dry-run posture.
struct Ask<'a> {
    question: &'a str,
    node: Option<String>,
    json: bool,
    dry_run: bool,
}

/// The seams a test replaces: the stores, the transcript, the session's
/// routing row name, and the one-shot spawner.
pub(crate) struct Deps<'a> {
    pub journal: PathBuf,
    pub registry: PathBuf,
    pub graph: Option<PathBuf>,
    /// `None` resolves the caller's own transcript through
    /// `transcript_resolve`; a test names a file it wrote.
    pub transcript: Option<PathBuf>,
    /// The session's routing row name. `None` resolves through the registry
    /// and the declared rows; a test names one directly.
    pub lane: Option<String>,
    pub cwd: PathBuf,
    pub spawner: &'a (dyn Fn(&[String], u64) -> Result<(i32, String, String), String> + Sync),
}

pub fn run(args: &[String]) -> i32 {
    let home = AgentsHome::from_env();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let deps = Deps {
        journal: home.events_jsonl(),
        registry: AgentsHome::shared_registry_json(),
        graph: Some(crate::backlog::settings::graph_path_at(Some(&cwd))),
        transcript: None,
        lane: None,
        spawner: &real_spawn,
        cwd,
    };
    run_with(args, &deps)
}

fn real_spawn(argv: &[String], kill_after_secs: u64) -> Result<(i32, String, String), String> {
    crate::blueprint_judge::headless_spawn(argv, kill_after_secs)
}

pub(crate) fn run_with(args: &[String], deps: &Deps) -> i32 {
    if args.first().map(String::as_str) == Some("report") {
        return run_report_with(&args[1..], deps);
    }
    let mut question = String::new();
    let mut node: Option<String> = None;
    let mut json_out = false;
    let mut dry_run = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--question" => question = it.next().cloned().unwrap_or_default(),
            "--node" => node = Some(it.next().cloned().unwrap_or_default()),
            "--json" | "-J" => json_out = true,
            "--dry-run" => dry_run = true,
            other => {
                eprintln!("consult: unknown argument {other}; usage: fno agents loop consult --question <text> [--node <id>] [--dry-run] [--json] | report [--since <dur>] --json");
                return 2;
            }
        }
    }
    if question.trim().is_empty() {
        eprintln!("consult: --question is required");
        return 2;
    }
    let get = |name: &str| std::env::var(name).ok();
    let home = AgentsHome::from_env();
    let ident = crate::spawn_context::resolve_self_identity(&get, None, None, &home);
    let harness = ident.harness.clone().unwrap_or_else(|| "unknown".into());
    let session = match ident.session_id.clone() {
        Some(s) if !s.trim().is_empty() => s,
        _ => {
            eprintln!("consult: no resolvable session identity (missing-input)");
            return EXIT_NO_TRANSCRIPT;
        }
    };
    ask(
        deps,
        &harness,
        &session,
        &Ask {
            question: &question,
            node,
            json: json_out,
            dry_run,
        },
    )
}

fn config_i64(cwd: &Path, key: &str, default: i64) -> i64 {
    crate::agents_config::config_lookup(cwd, &["agents", "consult", key])
        .and_then(|v| v.as_integer())
        .filter(|v| *v >= 0)
        .unwrap_or(default)
}

fn config_str(cwd: &Path, key: &str, default: &str) -> String {
    crate::agents_config::config_lookup(cwd, &["agents", "consult", key])
        .and_then(|v| v.as_str().map(str::to_string))
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// The lane names `agents.consult.lanes` allows, empty (off) by default.
fn config_lanes(cwd: &Path) -> Vec<String> {
    crate::agents_config::config_lookup(cwd, &["agents", "consult", "lanes"])
        .and_then(|v| v.as_array().cloned())
        .map(|rows| {
            rows.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn session_lane(deps: &Deps, harness: &str, session: &str) -> Option<String> {
    if let Some(lane) = deps.lane.clone() {
        return Some(lane);
    }
    let rows = crate::client_commands::load_registry_entries(&deps.registry).ok()?;
    let row = crate::whoami::find_by_session(&rows, session, harness)?;
    let model = row
        .get("model")
        .and_then(Value::as_str)
        .or_else(|| row.get("model_name").and_then(Value::as_str))
        .unwrap_or_default();
    let declared = crate::route_gather::declared_rows(&deps.cwd);
    declared.as_object()?.iter().find_map(|(name, decl)| {
        (decl.get("harness").and_then(Value::as_str) == Some(harness)
            && decl.get("model").and_then(Value::as_str) == Some(model))
        .then(|| name.clone())
    })
}

// ── chunk: ask ──
fn ask(deps: &Deps, harness: &str, session: &str, ask: &Ask) -> i32 {
    let max_calls = config_i64(&deps.cwd, "max_calls", DEFAULT_MAX_CALLS);
    let count = consult_calls(deps, session).len() as i64;
    if ask.dry_run {
        let model = config_str(&deps.cwd, "model", DEFAULT_MODEL);
        let context_bytes = config_i64(&deps.cwd, "context_bytes", DEFAULT_CONTEXT_BYTES);
        let answer = json!({
            "max_calls": max_calls,
            "count": count,
            "model": model,
            "context_bytes": context_bytes,
            "transcript": deps.transcript.as_ref().map(|p| p.display().to_string()),
            "argv_tail": argv_tail(&model),
        });
        println!("{}", serde_json::to_string(&answer).unwrap_or_default());
        return 0;
    }
    if count >= max_calls {
        eprintln!("consult: cap {max_calls} reached for this session");
        return EXIT_CAP;
    }
    let lanes = config_lanes(&deps.cwd);
    let lane = session_lane(deps, harness, session);
    let allowed = !lanes.is_empty()
        && lane
            .as_deref()
            .is_some_and(|l| lanes.iter().any(|a| a == l));
    if !allowed {
        match lane {
            Some(l) if !lanes.is_empty() => {
                eprintln!("consult: lane {l} is not in agents.consult.lanes");
            }
            _ => {
                eprintln!("consult: agents.consult.lanes is empty; the action is off");
            }
        }
        return EXIT_LANE_REFUSED;
    }
    // ── chunk: transcript ──
    let transcript = match deps.transcript.clone().map(|p| ResolvedTranscript {
        resolved: true,
        path: Some(p),
        kind: "jsonl",
        ambiguous: false,
        reason: None,
    }) {
        Some(t) => t,
        None => resolve_transcript(
            harness,
            session,
            &deps.cwd.display().to_string(),
            &Roots::default(),
        ),
    };
    let path = match transcript.path {
        Some(p) if transcript.resolved => p,
        _ => {
            eprintln!(
                "consult: no transcript for {harness} session {}: {}",
                short(session),
                transcript.reason.unwrap_or("not-found")
            );
            return EXIT_NO_TRANSCRIPT;
        }
    };
    let node = ask
        .node
        .clone()
        .or_else(|| crate::registry_commands::find_held_node_at(&deps.cwd, Some(session)));
    // ── chunk: digest ──
    let context_bytes = config_i64(&deps.cwd, "context_bytes", DEFAULT_CONTEXT_BYTES) as usize;
    let digest = build_digest(&path, context_bytes);
    let digest_file = match tempfile::Builder::new()
        .prefix("fno-consult-")
        .suffix(".md")
        .tempfile()
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("consult: digest write failed: {e}");
            return EXIT_SPAWN_FAILED;
        }
    };
    if write!(digest_file.as_file(), "{digest}").is_err() {
        eprintln!("consult: digest write failed");
        return EXIT_SPAWN_FAILED;
    }
    let digest_path = digest_file.path().to_path_buf();
    let prompt = CONSULT_PROMPT
        .replace("{QUESTION}", ask.question.trim())
        .replace("{NODE}", node.as_deref().unwrap_or("none"))
        .replace("{DIGEST_PATH}", &digest_path.display().to_string());
    // ── chunk: spawn ──
    let model = config_str(&deps.cwd, "model", DEFAULT_MODEL);
    let mut argv = vec!["agents".to_string(), "spawn".to_string(), prompt];
    argv.extend([
        "--harness".to_string(),
        "claude".to_string(),
        "--once".to_string(),
    ]);
    argv.extend(["--cwd".to_string(), deps.cwd.display().to_string()]);
    argv.extend([
        "--timeout".to_string(),
        SPAWN_TIMEOUT_SECS.to_string(),
        "--".to_string(),
    ]);
    argv.extend(argv_tail(&model));
    let spawned = (deps.spawner)(&argv, SPAWN_TIMEOUT_SECS + 60);
    drop(digest_file);
    let _ = std::fs::remove_file(&digest_path);
    let (rc, stdout, stderr) = match spawned {
        Ok(x) => x,
        Err(e) => {
            eprintln!("consult: spawn failed: {e}");
            return EXIT_SPAWN_FAILED;
        }
    };
    // ── chunk: parse ──
    let envelope: Value = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    let guidance = envelope
        .get("result")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| stdout.trim().to_string());
    // ── chunk: print ──
    let usage = |key: &str| {
        envelope
            .pointer(&format!("/usage/{key}"))
            .and_then(Value::as_i64)
    };
    let cost = envelope.get("total_cost_usd").and_then(Value::as_f64);
    // ── chunk: print2 ──
    if ask.json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "guidance": guidance,
                "model": model,
                "exit": rc,
                "input_tokens": usage("input_tokens"),
                "output_tokens": usage("output_tokens"),
                "cost_usd": cost,
            }))
            .unwrap_or_default()
        );
    } else {
        println!("consult ({}):", model);
        println!("{guidance}");
    }
    if rc != 0 {
        let tail = stderr.lines().last().unwrap_or_default();
        eprintln!("consult: the one-shot exited {rc}: {tail}");
    }
    let emit_ok = emit_call(
        deps,
        harness,
        session,
        ask,
        &model,
        &envelope,
        rc,
        context_bytes,
    )
    .is_ok();
    if rc != 0 || !emit_ok {
        return EXIT_SPAWN_FAILED;
    }
    0
}

// ── chunk: tail-fns ──
fn argv_tail(model: &str) -> Vec<String> {
    [
        "--model",
        model,
        "--output-format",
        "json",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--tools",
        "Read",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// One `consult_call` event per call: identity, spend, and the exit. A figure
/// the envelope lacked records as the string `unknown`, never 0.
fn emit_call(
    deps: &Deps,
    harness: &str,
    session: &str,
    ask: &Ask,
    model: &str,
    envelope: &Value,
    exit: i32,
    context_bytes: usize,
) -> Result<(), ()> {
    let unknown_or = |v: Option<i64>| v.map(|n| json!(n)).unwrap_or_else(|| json!("unknown"));
    let mut fields = Map::new();
    fields.insert("session_id".into(), json!(session));
    fields.insert("harness".into(), json!(harness));
    fields.insert(
        "node".into(),
        ask.node.clone().map(|n| json!(n)).unwrap_or(Value::Null),
    );
    fields.insert("model".into(), json!(model));
    fields.insert("question_chars".into(), json!(ask.question.chars().count()));
    fields.insert("context_bytes".into(), json!(context_bytes));
    fields.insert(
        "input_tokens".into(),
        unknown_or(usage_tokens(envelope, "input_tokens")),
    );
    fields.insert(
        "output_tokens".into(),
        unknown_or(usage_tokens(envelope, "output_tokens")),
    );
    fields.insert(
        "cost_usd".into(),
        envelope
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .map(|c| json!(c))
            .unwrap_or_else(|| json!("unknown")),
    );
    fields.insert("exit".into(), json!(exit));
    crate::events::EventEmitter::new(&deps.journal, "consult")
        .emit_fields("consult_call", fields)
        .map_err(|e| eprintln!("consult: event write failed: {e}"))
}

fn usage_tokens(envelope: &Value, key: &str) -> Option<i64> {
    envelope
        .pointer(&format!("/usage/{key}"))
        .and_then(Value::as_i64)
}

fn short(session: &str) -> String {
    session.chars().take(8).collect()
}

fn consult_calls(deps: &Deps, session: &str) -> Vec<EventRow> {
    crate::event_store::query_events(
        &deps.journal,
        &EventQuery {
            types: vec!["consult_call".into()],
            session_id: Some(session.to_string()),
            ..EventQuery::default()
        },
    )
    .unwrap_or_default()
}

fn build_digest(path: &Path, context_bytes: usize) -> String {
    let mut turns: Vec<String> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if let Some(t) = crate::transcript_turns::turn_text(&event) {
                turns.push(format!("- worker asked: {t}"));
            } else if let Some(t) = crate::transcript_turns::reply_text(&event) {
                turns.push(format!("- worker said: {t}"));
            }
        }
    }
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    for turn in turns.iter().rev() {
        let len = turn.len() + 1;
        if used + len > context_bytes {
            break;
        }
        used += len;
        kept.push(turn.clone());
    }
    kept.reverse();
    kept.join("\n")
}

// ── chunk: report-parse ──
pub(crate) fn run_report_with(args: &[String], deps: &Deps) -> i32 {
    let mut since_ms: Option<i64> = None;
    let mut json_out = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--since" => since_ms = it.next().and_then(|v| parse_since(v)),
            "--json" | "-J" => json_out = true,
            other => {
                eprintln!("consult report: unknown argument {other}");
                return 2;
            }
        }
    }
    let report = report_json(deps, since_ms);
    if json_out {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        let arms = &report["arms"];
        println!(
            "consult arm: stuck {} rescued {} green {} mean_cost {}",
            arms["consult"]["stuck_nodes"],
            arms["consult"]["rescued_by_frontier_worker"],
            arms["consult"]["green_without_rescue"],
            arms["consult"]["mean_cost_usd"],
        );
        println!(
            "baseline arm: stuck {} rescued {} green {} mean_cost {}",
            arms["baseline"]["stuck_nodes"],
            arms["baseline"]["rescued_by_frontier_worker"],
            arms["baseline"]["green_without_rescue"],
            arms["baseline"]["mean_cost_usd"],
        );
    }
    0
}

/// The whole fold behind `consult report`: one `Value`, so tests assert the
/// arms directly instead of scraping stdout.
pub(crate) fn report_json(deps: &Deps, since_ms: Option<i64>) -> Value {
    let rows = crate::event_store::query_events(
        &deps.journal,
        &EventQuery {
            types: vec![
                "consult_call".into(),
                "first_check_notice".into(),
                "loop_check".into(),
            ],
            since_ms,
            ..EventQuery::default()
        },
    )
    .unwrap_or_default();
    let mut consult_ts: Vec<i64> = Vec::new();
    let mut consult_cost: BTreeMap<String, f64> = BTreeMap::new();
    let mut stuck: BTreeMap<String, i64> = BTreeMap::new();
    let mut red_rounds: BTreeMap<String, i64> = BTreeMap::new();
    let mut nodeless = 0usize;
    for row in &rows {
        let Ok(line) = serde_json::from_str::<Value>(row.line.trim()) else {
            continue;
        };
        let data = line.get("data").cloned().unwrap_or(Value::Null);
        let node = data
            .get("node_id")
            .or_else(|| data.get("node"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let session = data
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match row.r#type.as_str() {
            "consult_call" => {
                consult_ts.push(row.ts_ms);
                if let Some(n) = &node {
                    if let Some(c) = data.get("cost_usd").and_then(Value::as_f64) {
                        *consult_cost.entry(n.clone()).or_default() += c;
                    }
                }
            }
            "first_check_notice" => {
                if mark_stuck(&mut stuck, node, &session, row.ts_ms, deps).is_err() {
                    nodeless += 1;
                }
            }
            "loop_check" => {
                if data.get("ci").and_then(Value::as_str) == Some("red") {
                    let rounds = red_rounds.entry(session.clone()).or_default();
                    *rounds += 1;
                    if *rounds == 2 {
                        if mark_stuck(&mut stuck, node, &session, row.ts_ms, deps).is_err() {
                            nodeless += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let consult_start = consult_ts.iter().min().copied();
    let graph = deps
        .graph
        .as_ref()
        .and_then(|p| crate::graph_store::read_rows(p).ok())
        .unwrap_or_default();
    let mut consult_nodes: Vec<Value> = Vec::new();
    let mut baseline_nodes: Vec<Value> = Vec::new();
    let mut rescued = [0usize; 2];
    let mut greens = [0usize; 2];
    let mut cost_sums = [0.0f64; 2];
    let mut cost_ns = [0usize; 2];
    for (node, stuck_at) in &stuck {
        let row = graph
            .iter()
            .find(|r| r.get("id").and_then(Value::as_str) == Some(node.as_str()));
        let arm = if consult_start.is_some_and(|start| *stuck_at >= start) {
            0
        } else {
            1
        };
        let cost = if arm == 0 {
            consult_cost.get(node).copied()
        } else {
            node_cost(row)
        };
        if let Some(c) = cost {
            cost_sums[arm] += c;
            cost_ns[arm] += 1;
        }
        let saved = node_rescued(row, *stuck_at);
        if saved {
            rescued[arm] += 1;
        }
        let done = row.and_then(|r| r.get("status")).and_then(Value::as_str) == Some("done");
        if done && !saved {
            greens[arm] += 1;
        }
        let entry = json!({
            "node": node,
            "title": row.and_then(|r| r.get("title")).cloned().unwrap_or(Value::Null),
            "status": row.and_then(|r| r.get("status")).cloned().unwrap_or(Value::Null),
            "stuck_at_ms": stuck_at,
            "rescued_by_frontier_worker": saved,
            "cost_usd": cost,
        });
        if arm == 0 {
            consult_nodes.push(entry);
        } else {
            baseline_nodes.push(entry);
        }
    }
    let mean = |sum: f64, n: usize| -> Value {
        if n > 0 {
            json!(sum / n as f64)
        } else {
            Value::Null
        }
    };
    let report = json!({
        "window": {
            "baseline_since_ms": since_ms,
            "consult_start_ms": consult_start,
        },
        "arms": {
            "consult": {
                "stuck_nodes": consult_nodes.len(),
                "rescued_by_frontier_worker": rescued[0],
                "green_without_rescue": greens[0],
                "mean_cost_usd": mean(cost_sums[0], cost_ns[0]),
                "nodes": consult_nodes,
            },
            "baseline": {
                "stuck_nodes": baseline_nodes.len(),
                "rescued_by_frontier_worker": rescued[1],
                "green_without_rescue": greens[1],
                "mean_cost_usd": mean(cost_sums[1], cost_ns[1]),
                "nodes": baseline_nodes,
            },
        },
        "sessions_without_node": nodeless,
    });
    report
}

fn mark_stuck(
    stuck: &mut BTreeMap<String, i64>,
    node: Option<String>,
    session: &str,
    ts_ms: i64,
    deps: &Deps,
) -> Result<(), ()> {
    let resolved = node.or_else(|| session_node(deps, session));
    match resolved {
        Some(n) => {
            stuck
                .entry(n)
                .and_modify(|old| *old = (*old).min(ts_ms))
                .or_insert(ts_ms);
            Ok(())
        }
        None => Err(()),
    }
}

fn session_node(deps: &Deps, session: &str) -> Option<String> {
    let graph = deps.graph.as_ref()?;
    let rows = crate::graph_store::read_rows(graph).ok()?;
    rows.iter()
        .find(|row| {
            row.get("sessions")
                .and_then(Value::as_array)
                .is_some_and(|sessions| {
                    sessions
                        .iter()
                        .any(|s| s.get("session_id").and_then(Value::as_str) == Some(session))
                })
        })
        .and_then(|row| row.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

fn node_cost(row: Option<&Value>) -> Option<f64> {
    let sessions = row?.get("cost_sessions")?.as_array()?;
    let mut sum = 0.0;
    let mut any = false;
    for entry in sessions {
        if let Some(c) = entry.get("cost_usd").and_then(Value::as_f64) {
            sum += c;
            any = true;
        }
    }
    any.then_some(sum)
}

fn node_rescued(row: Option<&Value>, stuck_at: i64) -> bool {
    let Some(row) = row else {
        return false;
    };
    if row.get("status").and_then(Value::as_str) != Some("done") {
        return false;
    }
    row.get("sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| {
            sessions.iter().any(|s| {
                s.get("harness").and_then(Value::as_str) == Some("claude")
                    && s.get("started_at")
                        .and_then(started_at)
                        .is_some_and(|at| at > stuck_at)
                    && s.pointer("/observed_model/kind")
                        .and_then(Value::as_str)
                        .is_some_and(|k| k.contains("opus") || k.contains("fable"))
            })
        })
}

fn started_at(value: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn parse_since(text: &str) -> Option<i64> {
    let idx = text.find(|c: char| !c.is_ascii_digit())?;
    let n: i64 = text[..idx].parse().ok()?;
    let secs = match &text[idx..] {
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    Some(chrono::Utc::now().timestamp_millis() - n * secs * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn deps<'a>(
        dir: &Path,
        spawner: &'a (dyn Fn(&[String], u64) -> Result<(i32, String, String), String> + Sync),
    ) -> Deps<'a> {
        Deps {
            journal: dir.join("events.jsonl"),
            registry: dir.join("registry.json"),
            graph: None,
            transcript: None,
            lane: None,
            cwd: dir.to_path_buf(),
            spawner,
        }
    }

    fn with_lane<'a>(
        dir: &Path,
        spawner: &'a (dyn Fn(&[String], u64) -> Result<(i32, String, String), String> + Sync),
        transcript: Option<PathBuf>,
    ) -> Deps<'a> {
        let config_dir = dir.join(".fno");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[agents.consult]\nlanes = [\"lane-x\"]\n",
        )
        .unwrap();
        Deps {
            lane: Some("lane-x".into()),
            transcript,
            ..deps(dir, spawner)
        }
    }

    fn ask_run<'a>(d: &Deps<'a>, session: &str, question: &str, dry: bool) -> i32 {
        ask(
            d,
            "claude",
            session,
            &Ask {
                question,
                node: None,
                json: false,
                dry_run: dry,
            },
        )
    }

    fn envelope(out: &str) -> String {
        json!({
            "result": out,
            "usage": {"input_tokens": 11, "output_tokens": 7},
            "total_cost_usd": 0.02
        })
        .to_string()
    }

    fn calls(journal: &Path) -> Vec<Value> {
        crate::event_store::query_events(
            journal,
            &EventQuery {
                types: vec!["consult_call".into()],
                ..EventQuery::default()
            },
        )
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::from_str::<Value>(r.line.trim()).ok())
        .collect()
    }

    fn silent_ok(a: &[String], _t: u64) -> Result<(i32, String, String), String> {
        let _ = a;
        Ok((0, envelope("ok"), String::new()))
    }

    /// The allowed-lane Deps: a temp config naming `lane-x`, the lane seam
    /// set to match, and the spawner handed in as a `dyn` reference.
    fn allowed_deps<'a>(
        spawner: &'a (dyn Fn(&[String], u64) -> Result<(i32, String, String), String> + Sync),
        transcript: Option<PathBuf>,
    ) -> (tempfile::TempDir, Deps<'a>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
        std::fs::write(
            dir.path().join(".fno/config.toml"),
            "[agents.consult]\nlanes = [\"lane-x\"]\n",
        )
        .unwrap();
        let mut d = deps(dir.path(), spawner);
        d.lane = Some("lane-x".into());
        d.transcript = transcript;
        (dir, d)
    }

    fn bare_ok(a: &[String], _t: u64) -> Result<(i32, String, String), String> {
        let _ = a;
        Ok((0, "{\"result\":\"bare\"}".to_string(), String::new()))
    }

    #[test]
    fn the_cap_refuses_and_spawns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
        std::fs::write(
            dir.path().join(".fno/config.toml"),
            "[agents.consult]\nlanes = [\"lane-x\"]\n",
        )
        .unwrap();
        let calls_n = Arc::new(AtomicUsize::new(0));
        let counter = calls_n.clone();
        let spawner = move |_: &[String], _: u64| -> Result<(i32, String, String), String> {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok((0, envelope("ok"), String::new()))
        };
        let mut d = deps(dir.path(), &spawner);
        d.lane = Some("lane-x".into());
        let emitter = crate::events::EventEmitter::new(&d.journal, "consult");
        for _ in 0..2 {
            let mut m = Map::new();
            m.insert("session_id".into(), json!("sess-cap"));
            m.insert("exit".into(), json!(0));
            emitter.emit_fields("consult_call", m).unwrap();
        }
        let code = ask_run(&d, "sess-cap", "why", false);
        assert_eq!(code, EXIT_CAP);
        assert_eq!(calls_n.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_allowed_call_spawns_and_records_usage() {
        let (dir, d) = allowed_deps(&silent_ok, None);
        let transcript = dir.path().join("t.jsonl");
        std::fs::write(
            &transcript,
            json!({"type": "user", "message": {"content": "fix the drawer"}}).to_string(),
        )
        .unwrap();
        let mut d2 = d;
        d2.transcript = Some(transcript);
        let code = ask_run(&d2, "sess-use", "why", false);
        assert_eq!(code, 0);
        let rows = calls(&d2.journal);
        assert_eq!(rows.len(), 1);
        let data = &rows[0]["data"];
        assert_eq!(data["input_tokens"], json!(11));
        assert_eq!(data["output_tokens"], json!(7));
        assert_eq!(data["cost_usd"], json!(0.02));
        assert_eq!(data["question_chars"], json!(3));
    }

    #[test]
    fn an_envelope_without_usage_records_unknown() {
        let (dir, mut d) = allowed_deps(&bare_ok, None);
        let transcript = dir.path().join("t.jsonl");
        std::fs::write(
            &transcript,
            json!({"type": "user", "message": {"content": "q"}}).to_string(),
        )
        .unwrap();
        d.transcript = Some(transcript);
        let code = ask_run(&d, "sess-bare", "why", false);
        assert_eq!(code, 0);
        let rows = calls(&d.journal);
        assert_eq!(rows.len(), 1);
        let data = &rows[0]["data"];
        assert_eq!(data["input_tokens"], json!("unknown"));
        assert_eq!(data["output_tokens"], json!("unknown"));
        assert_eq!(data["cost_usd"], json!("unknown"));
    }

    #[test]
    fn report_with_no_events_prints_two_zero_arms_and_exits_0() {
        let dir = tempfile::tempdir().unwrap();
        let d = deps(dir.path(), &silent_ok);
        let code = run_report_with(&["--json".to_string()], &d);
        assert_eq!(code, 0);
        let report = report_json(&d, None);
        assert_eq!(report["arms"]["consult"]["stuck_nodes"], json!(0));
        assert_eq!(report["arms"]["baseline"]["stuck_nodes"], json!(0));
        assert_eq!(report["arms"]["consult"]["green_without_rescue"], json!(0));
        assert_eq!(report["arms"]["baseline"]["mean_cost_usd"], Value::Null);
    }

    #[test]
    fn dry_run_prints_the_cap_and_spawns_nothing() {
        let (_dir, d) = allowed_deps(&silent_ok, None);
        let code = ask_run(&d, "sess-dry", "probe", true);
        assert_eq!(code, 0);
        assert_eq!(calls(&d.journal).is_empty(), true);
    }
}
