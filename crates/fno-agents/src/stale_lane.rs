//! The stale, reap-hold and friction question lanes over the fleet-task channel.
//!
//! A row past the wake ceiling is the needs-human bucket: no action lane may take
//! it, so the only honest surface is a human's. Each lane reconciles one fleet
//! task to the set the sweep measured: same key is a duplicate, a changed set
//! supersedes, an empty set closes.

use crate::backlog::worked::FleetRow;
use crate::paths::AgentsHome;
use crate::watchdog::sweep::{now_epoch_s, payload_verdicts, run_sweep};
use crate::watchdog::unfinished::dedupe_key;
use crate::watchdog::{Verdict, CONTENDED, POLLING_SETTLED, STALE};
use serde_json::{json, Value};

const STALE_MARKER: &str = "watchdog-stale";
const HOLD_MARKER: &str = "reap-hold";
const FRICTION_MARKER: &str = "watchdog-friction";

type Pair = (Verdict, FleetRow);

fn age_h_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(\d+)h old").expect("age regex"))
}

/// The oldest `<n>h old` age named in any basis.
pub(crate) fn oldest_h(bases: &[&str]) -> Option<i64> {
    bases
        .iter()
        .filter_map(|basis| age_h_re().captures(basis))
        .filter_map(|caps| caps[1].parse::<i64>().ok())
        .max()
}

fn reconcile_channel(
    marker: &str,
    identities: &[String],
    empty: bool,
    question: &dyn Fn(&str) -> String,
    ask: &str,
) -> Result<(String, String), String> {
    let home = AgentsHome::from_env();
    let store = crate::provider_cap::questions_path(&home);
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let key = dedupe_key(identities);
    crate::fleet_task::reconcile(&store, marker, &key, &cwd, &question(&key), ask, empty)
}

fn shown_rows(pairs: &[Pair]) -> String {
    pairs
        .iter()
        .map(|(v, row)| {
            let node = row
                .node
                .as_deref()
                .filter(|n| !n.is_empty())
                .unwrap_or("unknown");
            format!("{} [node {node}]: {}", v.name, v.basis)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The stale lane's question: rows past the wake ceiling, oldest age named.
pub(crate) fn reconcile_stale(pairs: &[Pair]) -> Result<(String, String), String> {
    let bases: Vec<&str> = pairs.iter().map(|(v, _)| v.basis.as_str()).collect();
    let age_clause = oldest_h(&bases).map_or(String::new(), |h| format!(", oldest {h}h"));
    let count = pairs.len();
    let shown = shown_rows(pairs);
    let identities: Vec<String> = pairs
        .iter()
        .map(|(v, _)| format!("stale:{}", v.row_id))
        .collect();
    let ask = format!(
        "triage {count} stale watchdog row(s){age_clause}: fno agents watchdog --only stale"
    );
    reconcile_channel(
        STALE_MARKER,
        &identities,
        pairs.is_empty(),
        &|key: &str| {
            format!(
                "[{STALE_MARKER}:{key}] The fleet watchdog holds {count} stale row(s) no lane will act on{age_clause}. Nothing in the sweep clears these; each needs a human to reap it or resume it. Rows: {shown}"
            )
        },
        &ask,
    )
}

/// The reap-hold lane's question. Identities carry the reason, so a hold that
/// changes reason asks again. The ask is the first row's release command.
pub(crate) fn reconcile_holds(holds: &[Value]) -> Result<(String, String), String> {
    if holds.is_empty() {
        // An empty set closes: the channel decides, never the caller.
        return reconcile_channel(HOLD_MARKER, &[], true, &|_key: &str| String::new(), "");
    }
    let text = |hold: &Value, key: &str| -> String {
        hold.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut shown: Vec<String> = Vec::new();
    let mut identities: Vec<String> = Vec::new();
    for hold in holds {
        let age = hold
            .get("age_s")
            .and_then(Value::as_i64)
            .map_or("unmeasured".to_string(), |a| format!("{a}s"));
        shown.push(format!(
            "{} held {age} under {}: {}",
            text(hold, "id"),
            text(hold, "reason"),
            text(hold, "detail")
        ));
        identities.push(format!(
            "hold:{}:{}",
            text(hold, "id"),
            text(hold, "reason")
        ));
    }
    let ask_cmd = format!("fno agents reap --release {}", text(&holds[0], "id"));
    let count = holds.len();
    let joined = shown.join("; ");
    reconcile_channel(
        HOLD_MARKER,
        &identities,
        false,
        &|key: &str| {
            format!(
                "[{HOLD_MARKER}:{key}] The reaper holds {count} row(s) past agents.hold_escalate_after_s. Each hold is correct and none clears on its own. Rows: {joined} -> {ask_cmd}"
            )
        },
        &ask_cmd,
    )
}

/// The friction lane's question: contended and polling_settled rows.
pub(crate) fn reconcile_friction(pairs: &[Pair]) -> Result<(String, String), String> {
    let count = pairs.len();
    let shown = shown_rows(pairs);
    let identities: Vec<String> = pairs
        .iter()
        .map(|(v, _)| format!("{}:{}", v.verdict, v.row_id))
        .collect();
    let ask = format!(
        "triage {count} friction row(s): fno agents watchdog --only contended; fno agents watchdog --only polling_settled"
    );
    reconcile_channel(
        FRICTION_MARKER,
        &identities,
        pairs.is_empty(),
        &|key: &str| {
            format!(
                "[{FRICTION_MARKER}:{key}] The fleet watchdog holds {count} contention/polling row(s) no lane will act on. Each needs a human to separate the sessions or stop it. Rows: {shown}"
            )
        },
        &ask,
    )
}

/// The escalated holds from one reap dry-run read; `None` when the read fails, so
/// the channel closes nothing on an unreadable instrument.
fn escalated_holds() -> Option<Vec<Value>> {
    let out = std::process::Command::new(crate::scrape::fno_bin())
        .args(["agents", "reap", "--dry-run", "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let summary: Value = serde_json::from_slice(&out.stdout).ok()?;
    Some(
        summary
            .get("holds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|h| h.get("escalated").and_then(Value::as_bool).unwrap_or(false))
            .cloned()
            .collect(),
    )
}

fn pairs_with(payload: &Value, rows: Vec<FleetRow>, words: &[&str]) -> Vec<Pair> {
    payload_verdicts(payload)
        .into_iter()
        .zip(rows)
        .filter(|(v, _)| words.contains(&v.verdict.as_str()))
        .collect()
}

/// The stale lane's whole answer, as the JSON the daemon's `stale_sweep` parses.
/// The hold channel runs even when the sweep refused: the reaper's clock is a
/// different instrument, and its question must not vanish behind that refusal.
pub(crate) fn stale_escalate() -> Value {
    let (hold_count, hold_outcome, hold_qid) = match escalated_holds() {
        None => (0usize, "refused".to_string(), String::new()),
        Some(holds) => match reconcile_holds(&holds) {
            Ok((outcome, qid)) => (holds.len(), outcome, qid),
            Err(_) => (holds.len(), "refused".to_string(), String::new()),
        },
    };
    let (payload, rows) = run_sweep(now_epoch_s(), None);
    let (outcome, qid, stale_count, oldest) = if payload.get("refused").is_some() {
        ("refused".to_string(), String::new(), 0usize, 0i64)
    } else {
        let pairs = pairs_with(&payload, rows, &[STALE]);
        let bases: Vec<&str> = pairs.iter().map(|(v, _)| v.basis.as_str()).collect();
        let oldest = oldest_h(&bases).unwrap_or(0);
        match reconcile_stale(&pairs) {
            Ok((outcome, qid)) => (outcome, qid, pairs.len(), oldest),
            Err(_) => ("refused".to_string(), String::new(), pairs.len(), oldest),
        }
    };
    let summary = format!(
        "Summary: {stale_count} stale, outcome {outcome}, oldest {oldest}h; {hold_count} escalated hold(s), outcome {hold_outcome}"
    );
    json!({
        "outcome": outcome,
        "question_id": qid,
        "stale_count": stale_count,
        "oldest_h": oldest,
        "hold_count": hold_count,
        "hold_outcome": hold_outcome,
        "hold_question_id": hold_qid,
        "summary": summary,
    })
}

/// `fno agents stale-escalate`, nested under the census action.
pub fn run_stale(args: &[String]) -> i32 {
    let answer = stale_escalate();
    if args.iter().any(|a| a == "--json" || a == "-J") {
        println!("{answer}");
    } else {
        println!("{}", answer["summary"].as_str().unwrap_or(""));
    }
    0
}

/// `fno agents friction-escalate`, nested under the census action.
pub fn run_friction(args: &[String]) -> i32 {
    let (payload, rows) = run_sweep(now_epoch_s(), None);
    let (outcome, qid, count) = if payload.get("refused").is_some() {
        ("refused".to_string(), String::new(), 0usize)
    } else {
        let pairs = pairs_with(&payload, rows, &[CONTENDED, POLLING_SETTLED]);
        match reconcile_friction(&pairs) {
            Ok((outcome, qid)) => (outcome, qid, pairs.len()),
            Err(_) => ("refused".to_string(), String::new(), pairs.len()),
        }
    };
    let summary = format!("Summary: {count} friction, outcome {outcome}");
    if args.iter().any(|a| a == "--json" || a == "-J") {
        println!(
            "{}",
            json!({
                "outcome": outcome,
                "question_id": qid,
                "friction_count": count,
                "summary": summary,
            })
        );
    } else {
        println!("{summary}");
    }
    0
}
