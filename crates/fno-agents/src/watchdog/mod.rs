//! The external fleet watchdog: one verdict per fleet row, from transcript truth.
//!
//! Runs outside every session (a manual command, or a leg on the pr-watch tick)
//! and decides, per fleet row: wake it or leave it. The decision reads
//! TRANSCRIPT truth keyed by session id; the registry and `claude agents --json`
//! are hints (measured: 8 roster rows claimed `working` on silent transcripts).
//!
//! Traps a stranger inherits: node identity joins on the recorded `node:<id>`
//! claim or the worktree manifest, never on a name regex; a wake is confirmed
//! by transcript content, never by a state field; `unclaimed` is advisory and
//! never wakes.

pub mod apply;
pub mod cli;
pub mod keeper;
pub mod outage;
pub mod recoverable;
pub mod sweep;
pub mod tail;
pub mod tick;
pub mod unfinished;

use crate::backlog::worked::FleetRow;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) use tail::TailFacts;

pub(crate) const GHOST: &str = "ghost";
pub(crate) const WAKE: &str = "wake";
pub(crate) const STALE: &str = "stale";
pub(crate) const LEAVE: &str = "leave";
/// Advisory only: the worker is fine, the RECORD is wrong. It replaces LEAVE so
/// the row surfaces in the digest; no action lane names it.
pub(crate) const UNCLAIMED: &str = "unclaimed";
pub(crate) const SANDBOX_BLOCKED: &str = "sandbox-blocked";
pub(crate) const RECOVERABLE: &str = "recoverable";
/// A `--only keeper` filter value, not a row verdict: keepers have no registry row.
pub(crate) const KEEPER: &str = "keeper";
pub(crate) const CONTENDED: &str = "contended";
pub(crate) const POLLING_SETTLED: &str = "polling_settled";
/// Open node, spawn row, no role, quiet past the drive threshold. Driven like WAKE.
pub(crate) const SILENCE: &str = "silence";
/// A past-ceiling row whose evidence says finished work; it never enters the
/// needs-human ask it could never age out of.
pub(crate) const SPENT: &str = "spent";

/// Every verdict word, sorted. `--only` validates against this list.
pub(crate) const VERDICTS: [&str; 12] = [
    CONTENDED,
    GHOST,
    KEEPER,
    LEAVE,
    POLLING_SETTLED,
    RECOVERABLE,
    SANDBOX_BLOCKED,
    SILENCE,
    SPENT,
    STALE,
    UNCLAIMED,
    WAKE,
];

/// How long a transcript must stay quiet before a session reads as finished
/// with its tree.
pub(crate) const QUIET_AFTER_S: f64 = 900.0;

/// States that make a transcript-less row a ghost. `stopped` is absent: it is
/// already the operator's answer.
const GHOST_STATES: [&str; 3] = ["working", "busy", "blocked"];

/// Membership is candidacy, not a wake: the lane wakes only on a `stalled`
/// tail. `working` belongs because a state word is what a session claims and
/// this lane exists because that claim lies. `stopped` belongs because a
/// session that dies mid-turn is exactly a stopped row.
pub(crate) const WAKE_STATES: [&str; 4] = ["working", "blocked", "stopped", "idle"];

/// Graph statuses that mean the work is over, so an absent claim is the system
/// working rather than a gap.
const FINISHED_NODE_STATUSES: [&str; 3] = ["done", "superseded", "deferred"];

/// Tail readings that mean the session is still in play.
const ENGAGED_TAILS: [&str; 3] = ["watching", "your-move", "working"];

pub(crate) const TERMINAL_STATES: [&str; 5] = ["stopped", "done", "completed", "exited", "killed"];

/// Past this age a row reads `stale` and never reaches an action lane. A reset
/// stamp carries no date, and `parse_sgt_stamp` picks the nearest day, so a
/// date-less stamp is unambiguous for only half a day.
const WAKE_MAX_AGE_S: f64 = 12.0 * 3600.0;

const SGT_OFFSET_S: f64 = 8.0 * 3600.0;

/// One row's verdict. Field order is the JSON payload's key order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Verdict {
    pub row_id: String,
    pub name: String,
    pub state: String,
    pub verdict: String,
    pub basis: String,
    pub action: String,
    pub agent: String,
}

/// The `node:<id>` claim view the classifier reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ClaimView {
    pub(crate) state: String,
    pub(crate) holder: Option<String>,
}

/// The classifier's injected seams. A read that fails is `Err`, a different
/// fact from an empty answer: an unreadable read is never a verdict.
pub(crate) struct Seams<'a> {
    pub(crate) transcript_for: &'a dyn Fn(&str) -> Option<TailFacts>,
    pub(crate) claim_for: &'a dyn Fn(&str) -> Result<ClaimView, String>,
    pub(crate) node_state_for: &'a dyn Fn(&str) -> Result<Option<Value>, String>,
    pub(crate) worktree_check: &'a dyn Fn(&str) -> bool,
    pub(crate) pr_state_for: &'a dyn Fn(&str, i64) -> Option<String>,
    pub(crate) commit_count: &'a dyn Fn(&str) -> Option<i64>,
}

fn verdict_of(row: &FleetRow, verdict: &str, basis: String, action: &str) -> Verdict {
    Verdict {
        row_id: row.row_id.clone(),
        name: row.name.clone(),
        state: row.state.clone(),
        verdict: verdict.to_string(),
        basis,
        action: action.to_string(),
        agent: row.agent.clone(),
    }
}

/// `HH:MM:SS SGT` to UTC epoch seconds, or None when out of range. A time-only
/// stamp carries no date, so the day is the one of `now` rolled to the nearest
/// candidate.
pub(crate) fn parse_sgt_stamp(hour: u32, minute: u32, second: u32, now_s: f64) -> Option<f64> {
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let now_dt = chrono::DateTime::from_timestamp(now_s as i64, 0)?;
    let base = now_dt
        .date_naive()
        .and_hms_opt(hour, minute, second)?
        .and_utc()
        .timestamp() as f64;
    [-1.0_f64, 0.0, 1.0]
        .iter()
        .map(|day| base - SGT_OFFSET_S + day * 86400.0)
        .min_by(|a, b| {
            (a - now_s)
                .abs()
                .partial_cmp(&(b - now_s).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn reset_stamp_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(\d{1,2}):(\d{2}):(\d{2})\s*(?:SGT|UTC\+8)").expect("reset stamp regex")
    })
}

/// `("none"|"live"|"passed"|"unknown", reset_epoch, stamp)` over newest-last
/// records. A rate event is the newest record the shipped classifier marks
/// quota-swap-class; `unknown` is fail-safe, and an older record's stamp never
/// stands in for the newest one's.
pub(crate) fn rate_limit_window(
    records: &[(Option<f64>, String)],
    now_s: f64,
) -> (&'static str, Option<f64>, String) {
    for (_epoch, text) in records.iter().rev() {
        let swaps = crate::recovery_sweep::classify_session_error(text)
            .is_some_and(|err| err.triggers_swap());
        if !swaps {
            continue;
        }
        let Some(caps) = reset_stamp_re().captures(text) else {
            return ("unknown", None, String::new());
        };
        let stamp = caps[0].to_string();
        let parts = (
            caps[1].parse::<u32>().ok(),
            caps[2].parse::<u32>().ok(),
            caps[3].parse::<u32>().ok(),
        );
        let (Some(hour), Some(minute), Some(second)) = parts else {
            return ("unknown", None, stamp);
        };
        return match parse_sgt_stamp(hour, minute, second, now_s) {
            None => ("unknown", None, stamp),
            Some(epoch) if epoch > now_s => ("live", Some(epoch), stamp),
            Some(epoch) => ("passed", Some(epoch), stamp),
        };
    }
    ("none", None, String::new())
}

fn tail_word(facts: &TailFacts, age_s: Option<f64>) -> String {
    crate::session_truth::classify_tail(
        facts.last_role.as_deref(),
        &facts.last_text,
        age_s,
        crate::session_truth::STALLED_AFTER_S,
    )
}

/// Is this session done with its worktree? Quiet past the bar AND a tail not
/// still in play. False is the safe answer; every unreadable input returns it.
pub(crate) fn finished_with_the_tree(
    facts: Option<&TailFacts>,
    now_s: f64,
    quiet_after_s: f64,
) -> bool {
    let Some(facts) = facts else {
        return false;
    };
    let Some(epoch) = facts.last_event_epoch else {
        return false;
    };
    let age_s = (now_s - epoch).max(0.0);
    if age_s <= quiet_after_s {
        return false;
    }
    !ENGAGED_TAILS.contains(&tail_word(facts, Some(age_s)).as_str())
}

fn mins(now_s: f64, epoch: Option<f64>) -> Option<i64> {
    epoch.map(|e| (((now_s - e) / 60.0) as i64).max(0))
}

fn age_clause(now_s: f64, epoch: Option<f64>) -> String {
    match mins(now_s, epoch) {
        Some(n) => format!("{n}m"),
        None => "unknown".to_string(),
    }
}

fn node_status(node_state: &Value) -> String {
    node_state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn spent_basis(
    row: &FleetRow,
    facts: Option<&TailFacts>,
    facts_age_s: f64,
    seams: &Seams<'_>,
) -> Option<String> {
    let age = format!(
        "quiet {}h past the wake ceiling",
        (facts_age_s / 3600.0).floor() as i64
    );
    if let Some(node) = row.node.as_deref().filter(|n| !n.is_empty()) {
        // A failed read is never evidence.
        let status = (seams.node_state_for)(node)
            .ok()
            .flatten()
            .map(|n| node_status(&n).to_lowercase())
            .unwrap_or_default();
        if FINISHED_NODE_STATUSES.contains(&status.as_str()) {
            return Some(format!(
                "node {node} {status}; {age}; finished row, nothing to triage"
            ));
        }
    }
    if let Some(facts) = facts {
        if tail_word(facts, Some(facts_age_s)) == "done" {
            return Some(format!(
                "tail reads done; {age}; finished row, nothing to triage"
            ));
        }
    }
    None
}

/// The commits on the checked-out branch beyond `origin/main`.
pub(crate) fn branch_commit_count(cwd: &str) -> Option<i64> {
    if cwd.is_empty() {
        return None;
    }
    let out = std::process::Command::new("git")
        .args(["-C", cwd, "rev-list", "--count", "origin/main..HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn help_tag_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)<help(?:\s([^>]*))?>").expect("help tag regex")
    })
}

/// `(?<![\w.])\.git[/\\]`: the `regex` crate has no lookbehind, so the guard is
/// a scan of each hit's preceding character.
fn names_git_dir(evidence: &str) -> bool {
    let bytes = evidence.as_bytes();
    let mut from = 0usize;
    while let Some(found) = evidence[from..].find(".git") {
        let at = from + found;
        let next = bytes.get(at + 4).copied();
        let guarded = at > 0 && {
            let before = evidence[..at].chars().next_back();
            before.is_some_and(|c| c == '.' || c == '_' || c.is_alphanumeric())
        };
        if !guarded && matches!(next, Some(b'/') | Some(b'\\')) {
            return true;
        }
        from = at + 4;
    }
    false
}

/// The last distress tag, only for the Git sandbox signature.
pub(crate) fn sandbox_denial_text(facts: Option<&TailFacts>) -> Option<String> {
    let facts = facts?;
    if facts.last_role.as_deref() != Some("assistant") {
        return None;
    }
    let text: &str = if facts.last_text.is_empty() {
        &facts.tail_text
    } else {
        &facts.last_text
    };
    let tag = help_tag_re().captures(text)?;
    let attributes = shlex::split(tag.get(1).map_or("", |m| m.as_str()))?;
    let evidence = attributes
        .iter()
        .find_map(|value| {
            let (key, val) = value.split_once('=')?;
            key.eq_ignore_ascii_case("evidence").then(|| val.to_string())
        })
        .unwrap_or_default();
    if evidence.is_empty()
        || !evidence.contains("Operation not permitted")
        || !names_git_dir(&evidence)
    {
        return None;
    }
    Some(tag[0].to_string())
}

/// Classify a Codex Git denial only when both reaping guards are clear.
fn sandbox_blocked_verdict(
    row: &FleetRow,
    facts: Option<&TailFacts>,
    seams: &Seams<'_>,
) -> Option<Verdict> {
    if row.agent != "codex" {
        return None;
    }
    let evidence = sandbox_denial_text(facts)?;
    let Some(node) = row.node.as_deref().filter(|n| !n.is_empty()) else {
        return Some(verdict_of(
            row,
            LEAVE,
            "sandbox denial held: node identity unreadable; no reap".to_string(),
            "none",
        ));
    };
    let claim = match (seams.claim_for)(node) {
        Ok(claim) => claim,
        Err(exc) => {
            return Some(verdict_of(
                row,
                LEAVE,
                format!("sandbox denial held: claim unreadable ({exc}); no reap"),
                "none",
            ))
        }
    };
    if claim.state != "free" {
        let state = if claim.state.is_empty() {
            "unknown"
        } else {
            claim.state.as_str()
        };
        let holder = claim.holder.as_deref().unwrap_or("unknown");
        return Some(verdict_of(
            row,
            LEAVE,
            format!("sandbox denial held: node claim {state} ({holder}); no reap"),
            "none",
        ));
    }
    match (seams.commit_count)(&row.cwd) {
        None => Some(verdict_of(
            row,
            LEAVE,
            "sandbox denial held: branch commit count unreadable; no reap".to_string(),
            "none",
        )),
        Some(count) if count != 0 => Some(verdict_of(
            row,
            LEAVE,
            format!("sandbox denial held: branch carries {count} commit(s); no reap"),
            "none",
        )),
        Some(_) => Some(verdict_of(
            row,
            SANDBOX_BLOCKED,
            format!(
                "Codex sandbox blocked Git writes: {evidence}; node claim free; branch commits=0"
            ),
            "reap",
        )),
    }
}

/// Is this live row working a node no claim covers? A row whose node did not
/// resolve cannot be checked, so this catches the manifest-written-but-unclaimed
/// case only. Everything else degrades to silence: an advisory that fires on an
/// unreadable store trains its reader to ignore it.
fn unclaimed_node_basis(row: &FleetRow, seams: &Seams<'_>) -> String {
    let Some(node) = row.node.as_deref().filter(|n| !n.is_empty()) else {
        return String::new();
    };
    if TERMINAL_STATES.contains(&row.state.as_str()) {
        return String::new();
    }
    let Ok(claim) = (seams.claim_for)(node) else {
        return String::new();
    };
    // Plain `free` only; `stale` is the normal reading for a healthy worker
    // parked in a CI wait.
    if claim.state != "free" {
        return String::new();
    }
    let Ok(node_state) = (seams.node_state_for)(node) else {
        return String::new();
    };
    let status = node_state
        .map(|n| node_status(&n).to_lowercase())
        .unwrap_or_default();
    if FINISHED_NODE_STATUSES.contains(&status.as_str()) {
        return String::new();
    }
    format!("node {node} carries NO claim while this row is live")
}

fn verdict_one(
    row: &FleetRow,
    facts: Option<&TailFacts>,
    seams: &Seams<'_>,
    now_s: f64,
    in_quorum_breaker: bool,
    peers: &[String],
    silence_after_s: Option<f64>,
) -> Verdict {
    if facts.is_none() && GHOST_STATES.contains(&row.state.as_str()) {
        if row.substrate.as_deref() == Some("cloud") {
            return verdict_of(
                row,
                LEAVE,
                "transcript lives on claude.ai, not locally".to_string(),
                "report",
            );
        }
        let basis = if row.agent == "claude" || row.agent == "codex" {
            format!("no transcript for {}", row.row_id)
        } else {
            format!(
                "harness {} keeps no per-session transcript, unmeasurable",
                row.agent
            )
        };
        return verdict_of(row, GHOST, basis, "report");
    }

    if !peers.is_empty() {
        return verdict_of(
            row,
            CONTENDED,
            format!(
                "worktree {} holds {} live sessions, peers {}",
                row.cwd,
                peers.len() + 1,
                peers.join("/")
            ),
            "report",
        );
    }

    let facts_epoch = facts.and_then(|f| f.last_event_epoch);
    if let (Some(after), Some(epoch)) = (silence_after_s, facts_epoch) {
        if (now_s - epoch).max(0.0) > after {
            let node_state = match row.node.as_deref().filter(|n| !n.is_empty()) {
                Some(node) => match (seams.node_state_for)(node) {
                    Ok(state) => state,
                    Err(_) => {
                        return verdict_of(
                            row,
                            LEAVE,
                            "graph unreadable, silence verdict refused".to_string(),
                            "none",
                        )
                    }
                },
                None => None,
            };
            let node_open = node_state.is_some_and(|n| {
                let status = node_status(&n);
                status != "done" && status != "superseded"
            });
            if node_open {
                return verdict_of(
                    row,
                    SILENCE,
                    format!(
                        "open node {}, transcript quiet {}m",
                        row.node.as_deref().unwrap_or(""),
                        mins(now_s, Some(epoch)).unwrap_or(0)
                    ),
                    "drive",
                );
            }
        }
    }

    let (window, reset_epoch, stamp) = match facts {
        Some(f) => rate_limit_window(&f.records, now_s),
        None => ("none", None, String::new()),
    };
    let facts_age_s = facts_epoch.map(|e| (now_s - e).max(0.0));
    let wakeable = WAKE_STATES.contains(&row.state.as_str());
    if let (true, Some(age)) = (wakeable, facts_age_s) {
        if age > WAKE_MAX_AGE_S {
            if let Some(spent) = spent_basis(row, facts, age, seams) {
                return verdict_of(row, SPENT, spent, "none");
            }
            return verdict_of(
                row,
                STALE,
                format!(
                    "{} {}h old, past the {}h wake ceiling, needs a human",
                    row.state,
                    (age / 3600.0).floor() as i64,
                    (WAKE_MAX_AGE_S / 3600.0).floor() as i64
                ),
                "report",
            );
        }
    }

    if row.state == "blocked" && window == "live" && reset_epoch.is_some() && facts_epoch.is_some()
    {
        // The cap actor owns every cap move: the watchdog reports the strand.
        let basis = if in_quorum_breaker {
            "429 terminal for this session; cap: owned by provider-cap"
        } else {
            "429 terminal for this session; waiting for positive provider quorum"
        };
        return verdict_of(row, LEAVE, basis.to_string(), "none");
    }

    if let Some(verdict) = sandbox_blocked_verdict(row, facts, seams) {
        return verdict;
    }

    if let (true, Some(f)) = (wakeable, facts) {
        let Some(epoch) = f.last_event_epoch else {
            return verdict_of(
                row,
                LEAVE,
                "no parseable transcript evidence, not wakeable".to_string(),
                "none",
            );
        };
        if window == "unknown" {
            let named = if stamp.is_empty() { "no stamp" } else { stamp.as_str() };
            return verdict_of(
                row,
                LEAVE,
                format!("429 present, reset window unknown ({named})"),
                "none",
            );
        }
        if let ("live", Some(reset)) = (window, reset_epoch) {
            let reset_utc = chrono::DateTime::from_timestamp(reset as i64, 0)
                .map(|d| d.format("%H:%M:%SZ").to_string())
                .unwrap_or_default();
            return verdict_of(
                row,
                LEAVE,
                format!("429 resets {reset_utc}, window not open"),
                "none",
            );
        }
        let truth = tail_word(f, facts_age_s);
        if truth != "stalled" {
            return verdict_of(
                row,
                LEAVE,
                format!("tail reads {truth}, session does not owe a move"),
                "none",
            );
        }
        if window == "passed" {
            // A clock-only return races the provider-cap canary and wakes every
            // capped row at once.
            return verdict_of(
                row,
                LEAVE,
                "429 window passed; return owned by provider-cap".to_string(),
                "none",
            );
        }
        return verdict_of(
            row,
            WAKE,
            format!(
                "{} {}m silent, no 429 in tail",
                row.state,
                mins(now_s, Some(epoch)).unwrap_or(0)
            ),
            "resume",
        );
    }

    let basis = match facts {
        Some(f) => format!(
            "state {}, last turn {} ago, no lane applies",
            row.state,
            age_clause(now_s, f.last_event_epoch)
        ),
        None => format!("no transcript, state {}", row.state),
    };
    verdict_of(row, LEAVE, basis, "none")
}

/// One verdict per row, in table precedence (ghost, contended, silence, stale,
/// sandbox-blocked, wake, leave). Each basis names the measurement that decided
/// it, so a reader can falsify the call. `silence_after_s` is None (disabled)
/// unless a caller arms it.
pub(crate) fn verdicts(
    rows: &[FleetRow],
    seams: &Seams<'_>,
    now_s: f64,
    provider_outages: Option<&Value>,
    silence_after_s: Option<f64>,
) -> Vec<Verdict> {
    let mut facts_by_row: BTreeMap<&str, Option<TailFacts>> = BTreeMap::new();
    for row in rows {
        facts_by_row.insert(row.row_id.as_str(), (seams.transcript_for)(&row.row_id));
    }
    let mut quorum_row_ids: Vec<String> = Vec::new();
    for breaker in provider_outages
        .and_then(|r| r.get("breakers"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for id in breaker
            .get("row_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            quorum_row_ids.push(id.to_string());
        }
    }
    let mut live_in_tree: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for row in rows {
        let facts = facts_by_row.get(row.row_id.as_str()).and_then(|f| f.as_ref());
        if !row.cwd.is_empty()
            && (seams.worktree_check)(&row.cwd)
            && !finished_with_the_tree(facts, now_s, QUIET_AFTER_S)
        {
            live_in_tree
                .entry(row.cwd.as_str())
                .or_default()
                .push(row.row_id.clone());
        }
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let facts = facts_by_row.get(row.row_id.as_str()).and_then(|f| f.as_ref());
        let occupants = live_in_tree.get(row.cwd.as_str());
        let peers: Vec<String> = match occupants {
            Some(ids) if ids.contains(&row.row_id) => {
                ids.iter().filter(|id| **id != row.row_id).cloned().collect()
            }
            _ => Vec::new(),
        };
        let mut verdict = verdict_one(
            row,
            facts,
            seams,
            now_s,
            quorum_row_ids.contains(&row.row_id),
            &peers,
            silence_after_s,
        );
        if verdict.verdict == LEAVE {
            if let Some(f) = facts {
                if let Some((n, state, count)) =
                    tail::polling_basis(&f.pr_polls, seams.pr_state_for, &row.cwd)
                {
                    verdict.verdict = POLLING_SETTLED.to_string();
                    verdict.basis =
                        format!("{count} PR-status reads of #{n} after the tail read it {state}");
                    verdict.action = "report".to_string();
                }
            }
        }
        if verdict.verdict == LEAVE {
            let unclaimed = unclaimed_node_basis(row, seams);
            if !unclaimed.is_empty() {
                verdict.verdict = UNCLAIMED.to_string();
                verdict.basis = format!("{}; {unclaimed}", verdict.basis);
            }
        }
        out.push(verdict);
    }
    out
}

/// The named-unknown outage report: a crash or missing measure is never a
/// missing key.
pub(crate) fn unknown_provider_report(reason: &str) -> Value {
    json!({
        "instrument": "unknown",
        "breakers": [],
        "counts": {reason: 1},
        "refusals": [{"reason": reason, "count": 1}],
    })
}

#[cfg(test)]
mod tests;
