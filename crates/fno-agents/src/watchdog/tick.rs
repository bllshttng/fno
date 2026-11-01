//! The pr-watch tick's watchdog leg, whole: the tick calls one command and folds
//! its answer into its arm row. The tick's deadline is fatal, so every expensive
//! step reads the budget left first.

use super::apply::{apply_verdict, outcome_event};
use super::recoverable::{apply_recoverable, run_recoverable_sweep};
use super::sweep::{
    breaker_part, emit_event, fresh_non_leave, last_events_signature,
    last_recovery_events_signature, now_epoch_s, payload_verdicts, read_entries, run_sweep,
    silence_verdicts, union_signature, verdict_signature, write_sweep_file, SweepWrite,
};
use super::tail::{tail_facts, TAIL_RECORDS};
use super::{finished_with_the_tree, LEAVE, QUIET_AFTER_S, RECOVERABLE, WAKE};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A budget under the roster probe's measured cost buys a guaranteed timeout.
const ROSTER_FLOOR_S: f64 = 8.0;
/// `fno agents resume` waits up to 180s and the landing confirmation polls after.
const WAKE_APPLY_FLOOR_S: f64 = 200.0;
/// One Codex recovery scan's cost over a root.
const RECOVERY_ROOT_FLOOR_S: f64 = 3.0;
const RECOVERY_RECENCY_S: f64 = 24.0 * 3600.0;

struct Clock {
    deadline: Instant,
}

impl Clock {
    fn left(&self) -> f64 {
        self.deadline
            .saturating_duration_since(Instant::now())
            .as_secs_f64()
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn config_bool(keys: &[&str], default: bool) -> Option<bool> {
    match crate::agents_config::config_lookup(&cwd(), keys) {
        None => Some(default),
        Some(value) => value.as_bool(),
    }
}

/// The lane's arming question, answered as its first false key=value, or None
/// when armed. `mode` None asks only whether the lane runs at all.
fn gate_reason(mode: Option<&str>) -> Option<String> {
    let block = crate::agents_config::config_lookup(&cwd(), &["recovery", "watchdog"]);
    // A one-word `watchdog = "off"` predates the nested block and still parses.
    let (enabled, lane_mode) = match block.as_ref() {
        Some(toml::Value::String(word)) => {
            let word = word.trim().to_lowercase();
            let on = !word.is_empty() && word != "off";
            (on, if on { word } else { "report".to_string() })
        }
        Some(toml::Value::Table(table)) => (
            table
                .get("enabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false),
            table
                .get("mode")
                .and_then(toml::Value::as_str)
                .unwrap_or("report")
                .to_string(),
        ),
        _ => (false, "report".to_string()),
    };
    if !enabled {
        return Some("recovery.watchdog.enabled=false".to_string());
    }
    if let Some(wanted) = mode {
        if lane_mode != wanted {
            return Some(format!(
                "recovery.watchdog.mode='{lane_mode}' (wanted {wanted})"
            ));
        }
    }
    match config_bool(&["recovery", "enabled"], true) {
        Some(true) => {}
        Some(false) => return Some("recovery.enabled=false".to_string()),
        None => return Some("watchdog keys unresolved (partial settings)".to_string()),
    }
    match config_bool(&["autonomy", "enabled"], true) {
        Some(true) => {}
        Some(false) => return Some("autonomy.enabled=false".to_string()),
        None => return Some("watchdog keys unresolved (partial settings)".to_string()),
    }
    None
}

pub(crate) fn wake_armed() -> bool {
    gate_reason(Some("wake")).is_none() && crate::autonomy_policy::restart_allowed(&cwd())
}

/// `census watchdog-arming`: one condition, every reader. A freshness reader that
/// does not share the producer's condition once printed a dead cadence for a lane
/// that was off on purpose.
pub fn run_arming() -> i32 {
    let off = gate_reason(None);
    println!(
        "{}",
        json!({"armed": off.is_none(), "wake_armed": wake_armed(), "off_detail": off})
    );
    0
}

fn apply_and_emit(verdict: &super::Verdict, cwd: &str, agent: &str) {
    let (outcome, detail) = apply_verdict(verdict, "wake", cwd, Some(agent), None);
    emit_event(
        outcome_event(&outcome),
        json!({"row_id": verdict.row_id, "verdict": verdict.verdict, "detail": detail, "outcome": outcome}),
    );
}

/// The wake lane, the silence lane and the Codex recovery lane. Returns the acted
/// count and every recovery result.
fn wake_mode(clock: &Clock, now: f64, roots: &[PathBuf], outages: &Value) -> (i64, Vec<Value>) {
    let mut acted = 0i64;
    let mut results: Vec<Value> = Vec::new();
    if clock.left() / 2.0 < ROSTER_FLOOR_S {
        eprintln!(
            "pr-watch: watchdog wake budget spent ({:.1}s left for the next tick)",
            clock.left() / 2.0
        );
        return (acted, results);
    }
    let (mut payload, mut rows) = run_sweep(now, Some(outages.clone()));
    if let Some(refused) = payload.get("refused").and_then(Value::as_str) {
        // Zero rows read is an instrument failure: no events, no gates.
        eprintln!("pr-watch: watchdog sweep refused: {refused}");
        payload = json!({"verdicts": [], "counts": {}, "warnings": []});
        rows = Vec::new();
    }
    let prev_recovery = last_recovery_events_signature();
    let fresh = fresh_non_leave(&payload, &prev_recovery);
    for (verdict, row) in payload_verdicts(&payload).iter().zip(rows.iter()) {
        if verdict.verdict == LEAVE {
            continue;
        }
        if fresh.contains(&verdict.row_id) {
            emit_event(
                "watchdog_verdict",
                json!({"row_id": verdict.row_id, "name": verdict.name, "verdict": verdict.verdict, "basis": verdict.basis}),
            );
        }
        if verdict.verdict == WAKE {
            // A row skipped here is not lost: the next tick re-classifies it.
            if clock.left() < WAKE_APPLY_FLOOR_S {
                eprintln!(
                    "pr-watch: watchdog wake budget spent, {} left for the next tick",
                    verdict.row_id
                );
                continue;
            }
            apply_and_emit(verdict, &row.cwd, &row.agent);
            acted += 1;
        }
    }
    if clock.left() < WAKE_APPLY_FLOOR_S {
        eprintln!("pr-watch: watchdog silence budget spent");
    } else {
        for (verdict, row) in silence_verdicts(roots, now) {
            if clock.left() < WAKE_APPLY_FLOOR_S {
                eprintln!("pr-watch: watchdog silence budget spent");
                break;
            }
            apply_and_emit(&verdict, &row.cwd, &row.agent);
            acted += 1;
        }
    }
    let mut scans = Vec::new();
    for (done, root) in roots.iter().enumerate() {
        // Per root, not once before the loop.
        if clock.left() < RECOVERY_ROOT_FLOOR_S {
            eprintln!("pr-watch: Codex recovery scan stopped after {done} root(s)");
            break;
        }
        let (_payload, _rows, scan) =
            run_recoverable_sweep(root, RECOVERY_RECENCY_S, Some(now), None);
        if !scan.complete {
            eprintln!(
                "pr-watch: Codex recovery scan refused for {}",
                root.display()
            );
            continue;
        }
        scans.push(scan);
    }
    for scan in &scans {
        for item in apply_recoverable(scan) {
            let outcome = item.get("outcome").and_then(Value::as_str).unwrap_or("");
            let session = item.get("session_id").and_then(Value::as_str).unwrap_or("");
            // A non-applied candidate is refound by every tick until it ages out;
            // publish it once per recovery signature. Deferred was never attempted.
            if outcome == "applied" || (outcome != "deferred" && fresh.contains(session)) {
                emit_event(
                    outcome_event(outcome),
                    json!({
                        "row_id": session,
                        "verdict": RECOVERABLE,
                        "detail": item.get("detail").cloned().unwrap_or(Value::Null),
                        "outcome": outcome,
                    }),
                );
            }
            results.push(item);
        }
    }
    // What was published, plus what was already published, minus deferred ids a
    // publish-before-apply never actually said.
    let session_of = |item: &Value| -> String {
        item.get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let outcome_of = |item: &Value| -> String {
        item.get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let deferred: BTreeSet<String> = results
        .iter()
        .filter(|i| outcome_of(i) == "deferred")
        .map(session_of)
        .collect();
    let published: BTreeSet<String> = results
        .iter()
        .filter(|i| outcome_of(i) != "deferred")
        .map(session_of)
        .collect();
    let merged = union_signature(&[&prev_recovery, &verdict_signature(&payload)]);
    let mut parts: Vec<String> = merged
        .split(';')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    parts.extend(published.iter().map(|sid| format!("{sid}:{RECOVERABLE}")));
    let recovery_signature = parts
        .into_iter()
        .filter(|part| match part.split_once(':') {
            Some((head, _)) => !deferred.contains(head),
            None => true,
        })
        .collect::<Vec<_>>()
        .join(";");
    write_sweep_file(
        "tick",
        now,
        SweepWrite {
            recovery_events_signature: Some(recovery_signature),
            provider_outages: Some(outages.clone()),
            ..SweepWrite::default()
        },
    );
    (acted, results)
}

/// `{armed, off_detail}` when the lane is off, else the leg's whole answer. A
/// spent budget is `skip_reason: budget_spent`, not a failure of the sweep.
fn run_leg(clock: &Clock) -> Value {
    if let Some(off) = gate_reason(None) {
        return json!({"armed": false, "off_detail": off, "wake_armed": false});
    }
    let now = now_epoch_s();
    let mut roots = super::unfinished::report_roots();
    if roots.is_empty() {
        if let Some(root) = crate::paths_cli::repo_root() {
            roots.push(root);
        }
    }
    // The report honors the deadline by leaving late roots unscanned: their
    // dimensions read unknown, never clean.
    let report_deadline = Instant::now() + Duration::from_secs_f64((clock.left() / 2.0).max(0.0));
    let snapshot =
        super::unfinished::build_report_with_deadline(&roots, now, Some(report_deadline));
    let mail_to = crate::agents_config::config_lookup(&cwd(), &["recovery", "watchdog", "mail_to"])
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let log = |line: &str| eprintln!("pr-watch: {line}");
    super::unfinished::publish_report(&snapshot, "tick", now, &mail_to, Some(&log));
    if clock.left() < ROSTER_FLOOR_S {
        return json!({
            "armed": true,
            "skip_reason": "budget_spent",
            "detail": format!(
                "{:.1}s left, under the {ROSTER_FLOOR_S:.0}s a roster probe costs",
                clock.left()
            ),
        });
    }
    // Measured once per tick, in both modes: a breaker must be visible from a
    // plain report tick.
    let (rows, _warnings) = super::sweep::fleet_rows();
    let entries = read_entries(&rows);
    let outages = super::outage::measure_provider_outages(&rows, now, &entries);
    // The sweep file is the only memory of what the event lane already said, so a
    // first tick after a wipe re-announces the open breaker. That is correct.
    let prev_events = last_events_signature();
    let previous: BTreeSet<&str> = prev_events.split(';').filter(|p| !p.is_empty()).collect();
    let mut emitted: Vec<String> = Vec::new();
    for breaker in outages
        .get("breakers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let part = breaker_part(breaker);
        if !previous.contains(part.as_str()) {
            let text = |key: &str| breaker.get(key).and_then(Value::as_str).unwrap_or("");
            emit_event(
                "provider_breaker_transition",
                json!({
                    "outage_epoch": text("outage_epoch"),
                    "provider": text("provider"),
                    "account": text("account"),
                    "phase": "open",
                    "count": breaker.get("row_ids").and_then(Value::as_array).map_or(0, Vec::len),
                }),
            );
        }
        emitted.push(part);
    }
    let mut stamped: BTreeSet<String> = previous.iter().map(|p| p.to_string()).collect();
    stamped.extend(emitted);
    write_sweep_file(
        "tick",
        now,
        SweepWrite {
            events_signature: Some(stamped.into_iter().collect::<Vec<_>>().join(";")),
            provider_outages: Some(outages.clone()),
            ..SweepWrite::default()
        },
    );
    let wake = wake_armed();
    let (acted, results) = if wake {
        wake_mode(clock, now, &roots, &outages)
    } else {
        (0, Vec::new())
    };
    let count_outcome = |word: &str| {
        results
            .iter()
            .filter(|i| i.get("outcome").and_then(Value::as_str) == Some(word))
            .count()
    };
    let payload = super::unfinished::snapshot_payload(&snapshot);
    let counts: Vec<String> = payload
        .get("counts")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    json!({
        "armed": true,
        "wake_armed": wake,
        "acted": acted,
        "counts": counts.join(" "),
        "recoverable_applied": count_outcome("applied"),
        "recoverable_remaining": count_outcome("deferred"),
    })
}

/// `census watchdog-fleet`: the roster the claim readers index by node. The
/// warnings travel with the rows, since a blocking one refuses the whole read.
pub fn run_fleet() -> i32 {
    let (rows, warnings) = super::sweep::fleet_rows();
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "row_id": row.row_id,
                "name": row.name,
                "state": row.state,
                "node": row.node,
                "cwd": row.cwd,
                "pid": row.pid,
                "pid_start_time": row.pid_start_time,
                "mux": row.mux,
                "stopped_at": row.stopped_at,
            })
        })
        .collect();
    println!("{}", json!({"rows": rows, "warnings": warnings}));
    0
}

/// `census watchdog-tick --budget-s N`: one JSON answer on stdout, always exit 0
/// once the budget parses, so the tick's own wrap stays the only fatal path.
pub fn run(args: &[String]) -> i32 {
    let budget = match args {
        [flag, value] if flag == "--budget-s" => value.parse::<f64>().ok(),
        _ => None,
    };
    let Some(budget) = budget else {
        eprintln!("usage: fno-agents census watchdog-tick --budget-s SECONDS");
        return 2;
    };
    let clock = Clock {
        deadline: Instant::now() + Duration::from_secs_f64(budget.max(0.0)),
    };
    println!("{}", run_leg(&clock));
    0
}

/// `census watchdog-tail --session S --cwd C [--harness H]`: the claim and roster
/// readers' one question. Is this session finished with its tree, and what does
/// its tail read? Unreadable input answers `finished: false`, which keeps a claim.
pub fn run_tail(args: &[String]) -> i32 {
    let mut session = String::new();
    let mut cwd_arg = String::new();
    let mut harness: Option<String> = None;
    let mut index = 0;
    while index + 1 < args.len() {
        match args[index].as_str() {
            "--session" => session = args[index + 1].clone(),
            "--cwd" => cwd_arg = args[index + 1].clone(),
            "--harness" => harness = Some(args[index + 1].clone()),
            other => {
                eprintln!("fno-agents census watchdog-tail: unknown flag {other}");
                return 2;
            }
        }
        index += 2;
    }
    let harness = harness.unwrap_or_else(|| harness_for_session(&session));
    let facts = tail_facts(&session, &cwd_arg, &harness, TAIL_RECORDS, &[]);
    let now = now_epoch_s();
    let word = facts.as_ref().and_then(|f| {
        f.last_event_epoch.map(|epoch| {
            crate::session_truth::classify_tail(
                f.last_role.as_deref(),
                &f.last_text,
                Some((now - epoch).max(0.0)),
                crate::session_truth::STALLED_AFTER_S,
            )
        })
    });
    println!(
        "{}",
        json!({
            "has_transcript": facts.is_some(),
            "last_event_epoch": facts.as_ref().and_then(|f| f.last_event_epoch),
            "truth": word,
            "finished": finished_with_the_tree(facts.as_ref(), now, QUIET_AFTER_S),
        })
    );
    0
}

/// The harness owning a session; claude when the registry does not know it.
fn harness_for_session(session_id: &str) -> String {
    crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
        .ok()
        .and_then(|registry| {
            registry
                .entries
                .iter()
                .find(|e| e.harness_session_id.as_deref() == Some(session_id))
                .and_then(|e| e.harness.clone())
        })
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "claude".to_string())
}
