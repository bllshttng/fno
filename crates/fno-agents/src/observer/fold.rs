//! Pure fold: corpus construction + scoring for the observer harness.
//!
//! Ported from `cli/src/fno/observer/fold.py` (deleted in the same change).
//! Read-only over already-captured signals (ledger, graph, events,
//! postmortems) - corpus construction is a read-time fold, never a new
//! write-path for historical facts. The scoreboard-fold helpers the Python
//! leg imported are carried here as mirrors, each naming its Python oracle,
//! until the scoreboard family itself ports.
//!
//! I/O (plan-file reads, gh PR fetches, transcript reads) stays at the
//! [`super`] command layer or behind the injected readers, so the functions
//! here are testable without touching disk or the network.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::loopcheck::TerminationReason;

/// Mirrors `_CALIBRATION_MIN_VERDICTS` (scoreboard/fold.py): below this, a
/// ranking is fabricated confidence, not a real trend.
pub(crate) const MIN_ATTRIBUTABLE: usize = 10;

/// `_WEDGE_REASONS` (scoreboard/fold.py:31): the stuck-terminal set.
pub(crate) const WEDGE_REASONS: [&str; 3] = ["NoProgress", "Budget", "Aborted"];

/// no-PR class terminals (fold.py:267): a run that reached one of these
/// STOPPED without shipping anything. The discriminator is the terminal
/// reason, NOT the phase set. Ship terminals, `None`, `delegated` and
/// `NoWork` are all excluded.
pub(crate) fn is_no_pr_terminal(reason: &str) -> bool {
    WEDGE_REASONS.contains(&reason) || reason == "Interrupted"
}

/// `_SKILL_IDS` (fold.py:48).
pub(crate) fn skill_id_for(skill: &str) -> String {
    match skill {
        "blueprint" => "fno:blueprint".to_string(),
        "review" => "fno:review".to_string(),
        "target" => "fno:target".to_string(),
        other => format!("fno:{other}"),
    }
}

/// Claude's Discretion item 6: postmortem blocked_reason.kind -> attribution
/// class. `None` (unclassified) is a real third state, not an error - a kind
/// outside both sets stays unclassified rather than forcing a guess.
pub(crate) fn classify_postmortem(kind: Option<&str>) -> Option<&'static str> {
    const PLAN_ATTRIBUTABLE: [&str; 3] = [
        "missing_dependency",
        "architecture_decision",
        "ambiguous_requirement",
    ];
    const EXECUTION_ATTRIBUTABLE: [&str; 3] = ["test_failure", "tool_error", "environment"];
    let kind = kind?;
    if PLAN_ATTRIBUTABLE.contains(&kind) {
        Some("plan-attributable")
    } else if EXECUTION_ATTRIBUTABLE.contains(&kind) {
        Some("execution-attributable")
    } else {
        None
    }
}

/// `_parse_ts` (scoreboard/fold.py:284): parse an ISO timestamp to a naive
/// LOCAL datetime - the one timeline the whole fold uses. The dominant
/// source, the ledger's `completed`, is written naive-local; aware
/// timestamps (events carry a `...Z` / offset) are converted to local before
/// their tzinfo is stripped, so an offset is not silently mis-read.
pub(crate) fn parse_ts(raw: &Value) -> Option<NaiveDateTime> {
    let raw = raw.as_str()?;
    if raw.is_empty() {
        return None;
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M%.f",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(dt);
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0);
    }
    let normalized = raw
        .strip_suffix('Z')
        .map(|s| format!("{s}+00:00"))
        .unwrap_or_else(|| raw.to_string());
    DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|dt| dt.with_timezone(&Local).naive_local())
}

/// `_pct` (scoreboard/fold.py:319). Python's `round` is banker's rounding:
/// a .5 tie rounds to the even neighbor, so `(x).round()` would answer a
/// different fold.
pub(crate) fn pct(n: usize, d: usize) -> i64 {
    if d == 0 {
        return 0;
    }
    let f = 100.0 * n as f64 / d as f64;
    let floor = f.floor();
    if (f - floor - 0.5).abs() < f64::EPSILON {
        let below = floor as i64;
        if below % 2 == 0 {
            below
        } else {
            below + 1
        }
    } else {
        f.round() as i64
    }
}

/// `_sum_opt` (fold.py:377): sum possibly-null/junk numerics; all-missing ->
/// None (not 0.0), so an unmeasured aggregate stays distinct from a measured
/// zero.
pub(crate) fn sum_opt<'a>(values: impl Iterator<Item = &'a Value>) -> Option<f64> {
    let mut total = 0.0f64;
    let mut seen = false;
    for v in values {
        let n = match v {
            Value::Number(_) => v.as_f64(),
            Value::String(s) if !s.is_empty() => s.parse::<f64>().ok(),
            _ => continue,
        };
        if let Some(n) = n.filter(|n| n.is_finite()) {
            total += n;
            seen = true;
        }
    }
    if seen {
        Some(total)
    } else {
        None
    }
}

/// `_row_session_ids` (scoreboard/fold.py:1024): the `sessions` list (both
/// UUID and 20260708T-style), falling back to the scalar `session_id` for
/// rows predating the list.
pub(crate) fn row_session_ids(row: &Value) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let Some(sessions) = row.get("sessions").and_then(Value::as_array) {
        for s in sessions {
            if let Some(s) = s.as_str().filter(|s| !s.is_empty()) {
                ids.insert(s.to_string());
            }
        }
    }
    if let Some(scalar) = row.get("session_id").and_then(Value::as_str) {
        if !scalar.is_empty() {
            ids.insert(scalar.to_string());
        }
    }
    ids
}

/// `_is_shipped_reason` (scoreboard/fold.py:34): true iff a terminal reason
/// counts as a delivered node for telemetry - the `delivered` vocabulary
/// [`TerminationReason::is_delivered`] serves. Non-string junk (a
/// hand-edited or partial ledger row) is never a ship reason.
pub(crate) fn is_shipped_reason(termination_reason: Option<&str>) -> bool {
    let Some(tr) = termination_reason else {
        return false;
    };
    match serde_json::from_value::<TerminationReason>(Value::String(tr.to_string())) {
        Ok(t) => t.is_delivered(),
        Err(_) => false,
    }
}

/// `_row_shipped` (scoreboard/fold.py:85): a terminal proves the ROW shipped
/// only when its node delivered; a row with no node keeps the terminal check.
/// A reconcile backstop is a delivery row created after a merged PR, even
/// though it is not a worker terminal.
pub(crate) fn row_shipped(row: &Value, deliveries: Option<&Value>) -> bool {
    let termination_reason = row.get("termination_reason").and_then(Value::as_str);
    let is_backstop = termination_reason == Some("reconcile-backstop")
        && row.get("pr_number").is_some_and(|v| !v.is_null());
    if !is_shipped_reason(termination_reason) && !is_backstop {
        return false;
    }
    let Some(nid) = row
        .get("graph_node_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return true;
    };
    let Some(deliveries) = deliveries else {
        return true;
    };
    deliveries
        .get(nid)
        .and_then(|c| c.get("delivered"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `_node_outcome` (scoreboard/fold.py:482): derive a shipped node's outcome
/// from the W4/W5 signals: `reverted` flag, else a caused_by fix-node within
/// the follow-up window -> bounced, else merged_clean. Un-time-boundable
/// fixes count against the node (conservative, mirrors `_survival`).
pub(crate) fn node_outcome(
    nid: &str,
    shipped_at: Option<NaiveDateTime>,
    by_id: &BTreeMap<String, Value>,
    fixes: &HashMap<String, Vec<Value>>,
) -> &'static str {
    /// `_SURVIVAL_FOLLOWUP_DAYS` (scoreboard/fold.py:123).
    const FOLLOWUP_DAYS: i64 = 14;
    if by_id
        .get(nid)
        .is_some_and(|n| n.get("reverted").and_then(Value::as_bool) == Some(true))
    {
        return "reverted";
    }
    if let Some(list) = fixes.get(nid) {
        for fx in list {
            let fx_at = fx.get("created_at").map(parse_ts).unwrap_or(None);
            match (shipped_at, fx_at) {
                (Some(ship), Some(fx)) => {
                    let delta = (fx - ship).num_seconds();
                    if (0..=FOLLOWUP_DAYS * 86400).contains(&delta) {
                        return "bounced";
                    }
                }
                _ => return "bounced",
            }
        }
    }
    "merged_clean"
}

/// `_CI_RED_PREFIX` / `_CI_RECOGNIZED_NONRED` (scoreboard/fold.py:1008).
const CI_RED_PREFIX: &str = "FAILURE:";
const CI_RECOGNIZED_NONRED: [&str; 5] = ["SUCCESS", "PENDING", "none", "unknown", "skipped"];

/// `_ci_reds_from_fires` (scoreboard/fold.py:1037): count distinct red
/// EPISODES (transitions into FAILURE:*) across a session's loop_check ci
/// values in ts order. A red streak of N fires counts once. Returns
/// (ci_reds, unparsed_count); ci_reds is None when any fire carried an
/// unrecognized ci shape (drift must not be silently miscounted).
pub(crate) fn ci_reds_from_fires(ci_ordered: &[Option<&str>]) -> (Option<u64>, u64) {
    let mut unparsed = 0u64;
    for ci in ci_ordered {
        let recognized = ci
            .map(|ci| ci.starts_with(CI_RED_PREFIX) || CI_RECOGNIZED_NONRED.contains(&ci))
            .unwrap_or(false);
        if !recognized {
            unparsed += 1;
        }
    }
    if unparsed > 0 {
        return (None, unparsed);
    }
    let mut episodes = 0u64;
    let mut prev_red = false; // the pre-run state is non-red, so a leading FAILURE is one episode
    for ci in ci_ordered {
        let is_red = ci.is_some_and(|ci| ci.starts_with(CI_RED_PREFIX));
        if is_red && !prev_red {
            episodes += 1;
        }
        prev_red = is_red;
    }
    (Some(episodes), 0)
}

/// `_PHASE_TO_SKILL` (scoreboard/fold.py:584). "do" is the phase's retired
/// spelling: ledger.json history rows still carry it, so its key stays until
/// those rows age out.
fn phase_to_skill(phase: &str) -> Option<&'static str> {
    match phase {
        "think" => Some("fno:think"),
        "plan" => Some("fno:blueprint"),
        "do" => Some("fno:do"),
        "execute" => Some("fno:execute"),
        "review" => Some("fno:review"),
        "docs" => Some("fno:ship-docs"),
        "ship" | "external" => Some("fno:ship"),
        _ => None,
    }
}

/// `cost/_session_cost.UUID_RE`: `[0-9a-f]{8}-...{12}`.
fn is_uuid(session_id: &str) -> bool {
    let bytes = session_id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if *b != b'-' {
                    return false;
                }
            }
            _ => {
                if !b.is_ascii_hexdigit() || b.is_ascii_uppercase() {
                    return false;
                }
            }
        }
    }
    true
}

/// `_skills_from_transcript` (scoreboard/fold.py:596): scan transcript JSONL
/// lines for Skill tool_use blocks; return the `input.skill` id of each.
/// A transcript line that names no Skill tool_use cannot yield a skill run:
/// the match below needs a tool_use block whose `name` is this literal, and
/// the literal itself is the line-level pre-filter. Transcript lines are the
/// sweep's dominant input, so the parse only runs on candidate lines.
pub(crate) const SKILL_USE_MARKER: &str = "Skill";

fn skills_from_transcript(lines: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() || !line.contains(SKILL_USE_MARKER) {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(content) = obj
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for block in content {
            let is_skill = block.get("type").and_then(Value::as_str) == Some("tool_use")
                && block.get("name").and_then(Value::as_str) == Some(SKILL_USE_MARKER);
            if !is_skill {
                continue;
            }
            if let Some(skill) = block
                .get("input")
                .and_then(|i| i.get("skill"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                found.push(skill.to_string());
            }
        }
    }
    found
}

/// `_extract_skill_runs` (scoreboard/fold.py:702): (distinct skill ids,
/// method) for one ledger row. method is "transcript" (real Skill tool_use
/// found), "phase-proxy" (fallback to phases_completed), or "unattributed"
/// (neither).
pub(crate) fn extract_skill_runs(
    row: &Value,
    read_transcript: &dyn Fn(&str) -> Option<Vec<String>>,
) -> (Vec<String>, &'static str) {
    let sessions: Vec<String> = row
        .get("sessions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|s| is_uuid(s))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut found: Vec<String> = Vec::new();
    for sid in &sessions {
        if let Some(lines) = read_transcript(sid) {
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            found.extend(skills_from_transcript(&refs));
        }
    }
    if !found.is_empty() {
        let distinct: BTreeSet<String> = found.into_iter().collect();
        return (distinct.into_iter().collect(), "transcript");
    }
    let proxy: BTreeSet<&str> = row
        .get("phases_completed")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter_map(phase_to_skill)
                .collect()
        })
        .unwrap_or_default();
    if !proxy.is_empty() {
        return (
            proxy.into_iter().map(str::to_string).collect(),
            "phase-proxy",
        );
    }
    (Vec::new(), "unattributed")
}

/// `_first_session_id` (fold.py:68): the row's first session, unless it is
/// the unresolved marker (`cost/_register.LEDGER_SESSION_UNRESOLVED` - "we
/// looked and found nothing" is not a session id).
fn first_session_id(row: &Value) -> Option<String> {
    const LEDGER_SESSION_UNRESOLVED: &str = "unresolved:no-harness-session";
    let sessions = row.get("sessions").and_then(Value::as_array)?;
    let first = sessions.first()?;
    let s = first.as_str()?;
    if s == LEDGER_SESSION_UNRESOLVED {
        return None;
    }
    Some(s.to_string())
}

/// Postmortem attribution lookup built once per corpus (fold.py's pm_by_node
/// / pm_by_session pair).
struct PostmortemIndex {
    by_node: HashMap<String, String>,
    by_session: HashMap<String, String>,
}

impl PostmortemIndex {
    fn new(postmortems: &[Value]) -> Self {
        let mut by_node = HashMap::new();
        let mut by_session = HashMap::new();
        for pm in postmortems {
            let Some(kind) = pm
                .get("blocked_reason_kind")
                .and_then(Value::as_str)
                .filter(|k| !k.is_empty())
            else {
                continue;
            };
            if let Some(nid) = pm.get("graph_node_id").and_then(Value::as_str) {
                if !nid.is_empty() {
                    by_node.insert(nid.to_string(), kind.to_string());
                }
            }
            if let Some(sid) = pm.get("session_id").and_then(Value::as_str) {
                if !sid.is_empty() {
                    by_session.insert(sid.to_string(), kind.to_string());
                }
            }
        }
        Self {
            by_node,
            by_session,
        }
    }
}

/// The injectable per-row readers the corpus fold takes (I/O stays at the
/// caller; tests inject fixtures, mirroring the Python defaults' seams).
pub(crate) struct CorpusReaders<'a> {
    pub read_transcript: &'a dyn Fn(&str) -> Option<Vec<String>>,
    pub resolve_skill_version: &'a dyn Fn(&str, Option<&str>) -> String,
}

/// `build_corpus` (fold.py:80): ledger rows attributed to *skill* -> corpus
/// items. A row that ran a different skill is simply not attributed here -
/// never a crash, never a counted gap.
///
/// Returns `{"items": [...], "total_rows": int, "attributed": int}`.
pub(crate) fn build_corpus(
    rows: &[Value],
    graph_nodes: &[Value],
    postmortems: &[Value],
    skill: &str,
    since_days: i64,
    now: NaiveDateTime,
    deliveries: Option<&Value>,
    readers: &CorpusReaders<'_>,
) -> Value {
    let skill_id = skill_id_for(skill);
    let cutoff = now - chrono::Duration::days(since_days);
    let windowed: Vec<&Value> = rows
        .iter()
        .filter(|r| r.is_object())
        .filter(|r| {
            parse_ts(r.get("completed").unwrap_or(&Value::Null))
                .is_some_and(|dt| cutoff <= dt && dt <= now)
        })
        .collect();

    let by_id: BTreeMap<String, Value> = graph_nodes
        .iter()
        .filter_map(|n| {
            let id = n.get("id")?;
            if id.is_null() {
                return None;
            }
            Some((id.to_string(), n.clone()))
        })
        .collect();
    let mut fixes: HashMap<String, Vec<Value>> = HashMap::new();
    for n in graph_nodes {
        if let Some(origin) = n.get("caused_by").and_then(Value::as_str) {
            if !origin.is_empty() {
                fixes.entry(origin.to_string()).or_default().push(n.clone());
            }
        }
    }
    // Same w4 gate as scoreboard: without any causal telemetry, node_outcome's
    // a "no fix found" verdict is not distinguishable from "revert data doesn't
    // exist yet", so a shipped node is unjudgeable rather than a fake
    // merged_clean.
    let w4_available = graph_nodes
        .iter()
        .any(|n| n.get("reverted").is_some() || n.get("caused_by").is_some_and(|v| !v.is_null()));

    let pms = PostmortemIndex::new(postmortems);

    let mut items: Vec<Value> = Vec::new();
    for r in &windowed {
        let (skills, method) = extract_skill_runs(r, readers.read_transcript);
        if !skills.iter().any(|s| s == &skill_id) {
            continue;
        }

        let nid = r.get("graph_node_id").cloned().unwrap_or(Value::Null);
        let session_id = first_session_id(r);
        let shipped = row_shipped(r, deliveries);
        let nid_str = nid.as_str().filter(|s| !s.is_empty());
        let judgeable = shipped
            && nid_str.is_some()
            && w4_available
            && nid_str.is_some_and(|id| by_id.contains_key(id));
        let outcome: Option<&'static str> = if judgeable {
            nid_str.map(|id| node_outcome(id, parse_ts(&r["completed"]), &by_id, &fixes))
        } else {
            None
        };
        let version =
            (readers.resolve_skill_version)(&skill_id, r.get("completed").and_then(Value::as_str));
        let kind = nid_str
            .and_then(|id| pms.by_node.get(id))
            .or_else(|| {
                session_id
                    .as_deref()
                    .and_then(|sid| pms.by_session.get(sid))
            })
            .map(String::as_str);
        items.push(json!({
            "session_id": session_id,
            "graph_node_id": nid,
            "plan_path": r.get("plan_path").cloned().unwrap_or(Value::Null),
            "skill_id": skill_id,
            "skill_version": version,
            "method": method,
            "shipped": shipped,
            "termination_reason": r.get("termination_reason").cloned().unwrap_or(Value::Null),
            "judgeable": judgeable,
            "outcome": outcome,
            "attribution_class": classify_postmortem(kind),
        }));
    }

    let attributed = items.len();
    json!({
        "items": items,
        "total_rows": windowed.len(),
        "attributed": attributed,
    })
}

/// `score_blueprint_item` (fold.py:215): score one corpus item on blueprint's
/// dimensions. `shipped_outcome` is omitted from the result for a replay item
/// (caller passes the item without `judgeable`/`outcome` populated meaningfully
/// - Review Amendment A1: replay scores structural dimensions only). `null`
/// for a dimension means "not scorable" (counted as a coverage gap by the
/// caller), never a fabricated verdict.
///
/// `collisions` is the pre-fetched `_surface_collisions(plan_text)` answer
/// (the wave-door I/O stays at the caller): `None` = unscorable.
pub(crate) fn score_blueprint_item(item: &Value, collisions: Option<&[String]>) -> Value {
    let mut result = Map::new();
    result.insert(
        "collision_free".to_string(),
        match collisions {
            None => Value::Null,
            Some(list) if list.is_empty() => json!("pass"),
            Some(_) => json!("fail"),
        },
    );

    if item.get("include_shipped_outcome").and_then(Value::as_bool) != Some(false) {
        let outcome = item.get("outcome").cloned().unwrap_or(Value::Null);
        let shipped = if item.get("judgeable").and_then(Value::as_bool) != Some(true) {
            Value::Null
        } else if outcome == json!("merged_clean") {
            json!("pass")
        } else if outcome == json!("bounced") || outcome == json!("reverted") {
            let attribution = item.get("attribution_class").and_then(Value::as_str);
            if attribution == Some("plan-attributable") {
                json!("fail")
            } else {
                json!("degraded")
            }
        } else {
            Value::Null
        };
        result.insert("shipped_outcome".to_string(), shipped);
    }

    Value::Object(result)
}

/// `_repo_from_url` (fold.py:270): `https://github.com/owner/name/pull/149`
/// -> `owner/name`, else None.
pub(crate) fn repo_from_url(url: Option<&str>) -> Option<String> {
    let url = url?;
    let start = url.find("github.com/")?;
    let rest = &url[start + "github.com/".len()..];
    let end = rest.find("/pull/")?;
    let mut parts = rest[..end].split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// `_resolve_pr_node` (fold.py:278): PR -> graph node, walking back. Branch
/// `feature/<id>` first (cheap), then the reverse lookup by exact pr_url,
/// then (pr_number, repo) - branch name alone re-creates the loss on
/// squash-merges that drop the branch. None -> the PR is unattributable (a
/// counted coverage gap, never a silent drop).
fn resolve_pr_node(
    pr: &Value,
    by_id: &BTreeMap<String, Value>,
    by_pr_url: &HashMap<String, Value>,
    by_pr_num_repo: &HashMap<(String, i64), Value>,
) -> Option<Value> {
    let head = pr.get("headRefName").and_then(Value::as_str).unwrap_or("");
    if let Some(feature_id) = head.strip_prefix("feature/") {
        if let Some(node) = by_id.get(feature_id) {
            return Some(node.clone());
        }
    }
    if let Some(url) = pr.get("url").and_then(Value::as_str) {
        if let Some(node) = by_pr_url.get(url) {
            return Some(node.clone());
        }
    }
    let repo = repo_from_url(pr.get("url").and_then(Value::as_str));
    let number = pr.get("number").and_then(Value::as_i64);
    if let (Some(repo), Some(number)) = (repo, number) {
        if let Some(node) = by_pr_num_repo.get(&(repo, number)) {
            return Some(node.clone());
        }
    }
    None
}

/// `_target_session_signals` (fold.py:300): aggregate the per-PR signals
/// vector across a node's sessions. Structured tier (loop_check / termination
/// events, present) drives the three verdict dimensions; ledger + transcript
/// tiers ride as evidence only. An unmeasured signal is `null`, never `0`
/// (Locked Decision 6): a node with no joined loop_check has `loop_fires`
/// null, distinct from a measured zero.
///
/// `promise` is NOT a standalone event kind - it is a `loop_check` whose
/// `intent` is "promise" (verified against the emitters); likewise the plan's
/// `gate_escape` / `phase_transition` kinds do not exist, so they are
/// honestly absent from the vector rather than faked as 0.
pub(crate) fn target_session_signals(
    session_ids: &BTreeSet<String>,
    events_by_session: &HashMap<String, Vec<Value>>,
    node_rows: &[&Value],
) -> Value {
    let mut promises = 0u64;
    let mut loop_fires = 0u64;
    let mut ci_by_ts: Vec<(Option<NaiveDateTime>, Option<String>)> = Vec::new();
    let mut terminal_reasons: BTreeSet<String> = BTreeSet::new();
    for sid in session_ids {
        for e in events_by_session.get(sid).into_iter().flatten() {
            let kind = e
                .get("kind")
                .or_else(|| e.get("type"))
                .and_then(Value::as_str);
            let data_obj = e
                .get("data")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if kind == Some("loop_check") {
                loop_fires += 1;
                if data_obj.get("intent").and_then(Value::as_str) == Some("promise") {
                    promises += 1;
                }
                let dt = parse_ts(e.get("ts").unwrap_or(&Value::Null))
                    .or_else(|| parse_ts(data_obj.get("ts").unwrap_or(&Value::Null)));
                let ci = data_obj
                    .get("ci")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                ci_by_ts.push((dt, ci));
            } else if kind == Some("termination") {
                if let Some(reason) = data_obj.get("reason").and_then(Value::as_str) {
                    if !reason.is_empty() {
                        terminal_reasons.insert(reason.to_string());
                    }
                }
            }
        }
    }

    // Ledger rows carry the terminal reason too (the sessions the lossy
    // join misses); union both so a PR whose ledger row is missing still
    // recovers a reason from events.
    for r in node_rows {
        if let Some(tr) = r.get("termination_reason").and_then(Value::as_str) {
            if !tr.is_empty() {
                terminal_reasons.insert(tr.to_string());
            }
        }
    }

    // Python sorts on `dt or datetime.max`, so an unparsed ts lands last.
    ci_by_ts.sort_by(|a, b| match (&a.0, &b.0) {
        (Some(x), Some(y)) => x.cmp(y),
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let ci_ordered: Vec<Option<&str>> = ci_by_ts.iter().map(|(_, ci)| ci.as_deref()).collect();
    let ci_reds = if loop_fires > 0 {
        json!(ci_reds_from_fires(&ci_ordered).0)
    } else {
        Value::Null // None (not 0): no joined loop_check != a measured zero
    };

    // Ledger tier (free - already loaded): tokens + duration, nullable so an
    // unrecorded field stays distinct from a measured zero.
    let tokens = sum_opt(
        node_rows
            .iter()
            .map(|r| r.get("tokens_total").unwrap_or(&Value::Null)),
    );
    let duration = sum_opt(
        node_rows
            .iter()
            .map(|r| r.get("duration_minutes").unwrap_or(&Value::Null)),
    );

    // Transcript tier (medium; GC'd -> None): tool-error taxonomy and
    // permission denials are transcript-only, low-tier, and evidence-only;
    // off in v1 (read_transcript=None at the caller), so `turns` /
    // `toolcalls` are null here, never 0.
    json!({
        "promises": if loop_fires > 0 { json!(promises) } else { Value::Null },
        "loop_fires": if loop_fires > 0 { json!(loop_fires) } else { Value::Null },
        "ci_reds": ci_reds,
        "terminal_reasons": terminal_reasons.into_iter().collect::<Vec<_>>(),
        "tokens_total": tokens.map(|v| json!(v)).unwrap_or(Value::Null),
        "duration_minutes": duration.map(|v| json!(v)).unwrap_or(Value::Null),
        "turns": Value::Null,
        "toolcalls": Value::Null,
    })
}

/// `build_target_corpus` (fold.py:393): PR-anchored corpus for the target
/// skill. Pure; the caller pre-fetches `prs` (gh pr list merged+closed) and
/// `events_by_session` (loop_check + termination grouped by
/// `data.session_id`).
///
/// Returns `{"items", "unattributed", "no_pr", "coverage"}`. Invariant
/// (AC1-HP): `items` and `unattributed` strictly partition `prs` - asserted
/// here, so a PR resolving to neither is a partition bug, not a silent drop.
pub(crate) fn build_target_corpus(
    prs: &[Value],
    graph_nodes: &[Value],
    rows: &[Value],
    events_by_session: &HashMap<String, Vec<Value>>,
    since_days: i64,
    now: NaiveDateTime,
    postmortems: &[Value],
) -> Value {
    let cutoff = now - chrono::Duration::days(since_days);

    let by_id: BTreeMap<String, Value> = graph_nodes
        .iter()
        .filter_map(|n| {
            let id = n.get("id")?.as_str()?.to_string();
            if id.is_empty() {
                return None;
            }
            Some((id, n.clone()))
        })
        .collect();
    let mut fixes: HashMap<String, Vec<Value>> = HashMap::new();
    for n in graph_nodes {
        if let Some(origin) = n.get("caused_by").and_then(Value::as_str) {
            if !origin.is_empty() {
                fixes.entry(origin.to_string()).or_default().push(n.clone());
            }
        }
    }
    let w4_available = graph_nodes
        .iter()
        .any(|n| n.get("reverted").is_some() || n.get("caused_by").is_some_and(|v| !v.is_null()));

    let mut by_pr_url: HashMap<String, Value> = HashMap::new();
    let mut by_pr_num_repo: HashMap<(String, i64), Value> = HashMap::new();
    for n in graph_nodes {
        let url = n
            .get("pr_url")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty());
        let repo = repo_from_url(url);
        if let Some(url) = url {
            by_pr_url.insert(url.to_string(), n.clone());
        }
        if let (Some(repo), Some(num)) = (&repo, n.get("pr_number").and_then(Value::as_i64)) {
            by_pr_num_repo.insert((repo.clone(), num), n.clone());
        }
        // additional_prs: a node's secondary PRs (34 nodes carry these),
        // numbered in the node's own repo. Without indexing them a
        // non-primary PR resolves to no node and is wrongly counted
        // unattributed. entry-or-insert so a node's PRIMARY (pr_number)
        // claim always wins a collision over a secondary.
        if let Some(repo) = &repo {
            if let Some(ap_list) = n.get("additional_prs").and_then(Value::as_array) {
                for ap in ap_list {
                    if let Some(apn) = ap.get("number").and_then(Value::as_i64) {
                        by_pr_num_repo
                            .entry((repo.clone(), apn))
                            .or_insert(n.clone());
                    }
                }
            }
        }
    }

    let pms = PostmortemIndex::new(postmortems);

    // Index ledger rows by node id (for node -> sessions + terminal reason).
    let mut rows_by_node: HashMap<String, Vec<Value>> = HashMap::new();
    for r in rows.iter().filter(|r| r.is_object()) {
        if let Some(nid) = r
            .get("graph_node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            rows_by_node
                .entry(nid.to_string())
                .or_default()
                .push(r.clone());
        }
    }

    let mut items: Vec<Value> = Vec::new();
    let mut unattributed: Vec<Value> = Vec::new();
    let mut scored_session_ids: BTreeSet<String> = BTreeSet::new();

    for pr in prs.iter().filter(|p| p.is_object()) {
        let node = resolve_pr_node(pr, &by_id, &by_pr_url, &by_pr_num_repo);
        let ship_ts = parse_ts(pr.get("mergedAt").unwrap_or(&Value::Null))
            .or_else(|| parse_ts(pr.get("closedAt").unwrap_or(&Value::Null)));
        let merged = pr.get("mergedAt").is_some_and(|v| !v.is_null());
        let base = json!({
            "pr_number": pr.get("number").cloned().unwrap_or(Value::Null),
            "pr_url": pr.get("url").cloned().unwrap_or(Value::Null),
            "repo": repo_from_url(pr.get("url").and_then(Value::as_str)),
            "merged": merged,
        });
        let Some(node) = node else {
            let mut o = base.as_object().cloned().unwrap_or_default();
            o.insert("graph_node_id".to_string(), Value::Null);
            unattributed.push(Value::Object(o));
            continue;
        };

        let Some(nid) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        let empty: Vec<Value> = Vec::new();
        let node_rows_owned = rows_by_node.get(nid).unwrap_or(&empty);
        let node_rows: Vec<&Value> = node_rows_owned.iter().collect();
        let mut session_ids: BTreeSet<String> = BTreeSet::new();
        if let Some(sessions) = node.get("sessions").and_then(Value::as_array) {
            for s in sessions {
                // phase stamps: dicts, or legacy bare strings
                if let Some(sid) = s.get("session_id").and_then(Value::as_str) {
                    if !sid.is_empty() {
                        session_ids.insert(sid.to_string());
                    }
                } else if let Some(sid) = s.as_str().filter(|s| !s.is_empty()) {
                    session_ids.insert(sid.to_string());
                }
            }
        }
        for r in &node_rows {
            session_ids.extend(row_session_ids(r));
        }
        scored_session_ids.extend(session_ids.iter().cloned());

        let signals = target_session_signals(&session_ids, events_by_session, &node_rows);

        // shipped_outcome label: ONLY from node_outcome over w4 telemetry,
        // and only for a MERGED PR. A closed-unmerged PR (or missing
        // telemetry) is unjudgeable - the PR's own merge state is never
        // promoted to a verdict (AC2-ERR). The merge is the anchor; the
        // label is telemetry.
        let judgeable = merged && w4_available && by_id.contains_key(nid);
        let outcome_value = if judgeable {
            json!(node_outcome(nid, ship_ts, &by_id, &fixes))
        } else {
            Value::Null
        };
        let terminal_reasons = signals
            .get("terminal_reasons")
            .cloned()
            .unwrap_or(json!([]));
        let kind = pms.by_node.get(nid).map(String::as_str);
        let mut item = base.as_object().cloned().unwrap_or_default();
        item.insert("graph_node_id".to_string(), json!(nid));
        item.insert(
            "session_ids".to_string(),
            json!(session_ids.into_iter().collect::<Vec<_>>()),
        );
        item.insert("signals".to_string(), signals);
        item.insert("terminal_reasons".to_string(), terminal_reasons);
        item.insert("judgeable".to_string(), json!(judgeable));
        item.insert("outcome".to_string(), outcome_value);
        item.insert(
            "attribution_class".to_string(),
            json!(classify_postmortem(kind)),
        );
        items.push(Value::Object(item));
    }

    assert_eq!(
        items.len() + unattributed.len(),
        prs.len(),
        "PR partition violation: {} + {} != {}",
        items.len(),
        unattributed.len(),
        prs.len()
    );

    // no-PR class (survivorship correction, US3): window ledger rows that hit
    // an abandoned terminal and whose sessions do not belong to any scored
    // PR - the churny / abandoned / crash-terminated runs a green-only stat
    // silently drops. A shipped row that does not join a scored PR is
    // instead a window/truncation coverage gap (not counted here).
    let mut no_pr: Vec<Value> = Vec::new();
    for r in rows.iter().filter(|r| r.is_object()) {
        let Some(reason) = r.get("termination_reason").and_then(Value::as_str) else {
            continue;
        };
        if !is_no_pr_terminal(reason) {
            continue;
        }
        let Some(dt) = parse_ts(r.get("completed").unwrap_or(&Value::Null)) else {
            continue;
        };
        if !(cutoff <= dt && dt <= now) {
            continue;
        }
        let sids = row_session_ids(r);
        if sids.iter().any(|s| scored_session_ids.contains(s)) {
            continue;
        }
        no_pr.push(json!({
            "session_ids": sids.into_iter().collect::<Vec<_>>(),
            "termination_reason": reason,
            "graph_node_id": r.get("graph_node_id").cloned().unwrap_or(Value::Null),
        }));
    }

    let mut stop_cause: BTreeMap<String, u64> = BTreeMap::new();
    for e in &no_pr {
        let reason = e
            .get("termination_reason")
            .and_then(Value::as_str)
            .unwrap_or("");
        *stop_cause.entry(reason.to_string()).or_insert(0) += 1;
    }

    let coverage = json!({
        "prs_total": prs.len(),
        "attributed": items.len(),
        "unattributed_pr": unattributed.len(),
        "no_pr_attempts": no_pr.len(),
        "no_pr_stop_cause": stop_cause,
    });
    json!({
        "items": items,
        "unattributed": unattributed,
        "no_pr": no_pr,
        "coverage": coverage,
    })
}

/// `score_target_item` (fold.py:559): score one PR-anchored corpus item on
/// the three verdict dimensions. `null` for a dimension means "not scorable"
/// (a coverage gap), never a fabricated verdict. Only these three emit
/// verdicts; the rest of the signals vector rides as evidence/distribution
/// (Locked Decisions 6, 8). No process dimension is ever back-filled from
/// the outcome (AC2-EDGE): each judges only its own tier-1 signal.
pub(crate) fn score_target_item(item: &Value) -> Value {
    let mut result = Map::new();
    let signals = item.get("signals").cloned().unwrap_or(json!({}));

    // shipped_outcome (REUSED classifier): merged_clean -> pass; bounced/
    // reverted -> fail if plan-attributable else degraded
    // (attribution-dependent middle state); unjudgeable -> None.
    let shipped = if item.get("judgeable").and_then(Value::as_bool) != Some(true) {
        Value::Null
    } else {
        match item.get("outcome").and_then(Value::as_str) {
            Some("merged_clean") => json!("pass"),
            Some("bounced") | Some("reverted") => {
                let attribution = item.get("attribution_class").and_then(Value::as_str);
                if attribution == Some("plan-attributable") {
                    json!("fail")
                } else {
                    json!("degraded")
                }
            }
            _ => Value::Null,
        }
    };
    result.insert("shipped_outcome".to_string(), shipped);

    // first_try_green: 0 ci-red episodes -> pass, >=1 -> fail, no loop_check
    // joined (or unparsed ci drift -> ci_reds None) -> None. Hard structured
    // signal.
    let loop_fires = signals
        .get("loop_fires")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let ci_reds = signals.get("ci_reds");
    let first_try = if loop_fires > 0 && ci_reds.is_some_and(|v| !v.is_null()) {
        if ci_reds.and_then(Value::as_u64) == Some(0) {
            json!("pass")
        } else {
            json!("fail")
        }
    } else {
        Value::Null
    };
    result.insert("first_try_green".to_string(), first_try);

    // converged: a ship reason -> pass; a wedge reason (and no ship) ->
    // fail; no terminal reason -> None. NEVER inferred from the PR being
    // merged.
    let reasons: Vec<&str> = item
        .get("terminal_reasons")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let converged = if reasons.iter().any(|r| is_shipped_reason(Some(r))) {
        json!("pass")
    } else if reasons.iter().any(|r| WEDGE_REASONS.contains(r)) {
        json!("fail")
    } else {
        Value::Null
    };
    result.insert("converged".to_string(), converged);

    Value::Object(result)
}

/// `build_run_summary` (fold.py:608): aggregate a run's (dimension, verdict)
/// findings into the `skill_eval_run_complete` payload shape, or an
/// `insufficient` state. `findings` must already exclude null (unscorable)
/// entries - each is one real, dimension-scoped verdict. A replay batch
/// passes `require_min=false`: a targeted before/after on specific items is
/// not a trend, so it reports its true, small corpus_size rather than a
/// padded one.
pub(crate) fn build_run_summary(
    run_id: &str,
    skill_id: &str,
    skill_version: &str,
    findings: &[(String, String)],
    corpus_size: usize,
    scored_count: usize,
    skill_ref: Option<&str>,
    require_min: bool,
) -> Value {
    if require_min && corpus_size < MIN_ATTRIBUTABLE {
        return json!({"state": "insufficient", "need": MIN_ATTRIBUTABLE, "n": corpus_size});
    }

    let mut pass_count = 0u64;
    let mut degraded_count = 0u64;
    let mut fail_count = 0u64;
    let mut fail_by_dim: BTreeMap<String, u64> = BTreeMap::new();
    for (d, v) in findings {
        match v.as_str() {
            "pass" => pass_count += 1,
            "degraded" => degraded_count += 1,
            "fail" => {
                fail_count += 1;
                *fail_by_dim.entry(d.clone()).or_insert(0) += 1;
            }
            _ => {}
        }
    }
    // Python sorts on (-fail_count, dimension): count descending, ties by
    // dimension ascending.
    let mut ranking: Vec<(&String, u64)> = fail_by_dim.iter().map(|(d, n)| (d, *n)).collect();
    ranking.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let mut out = Map::new();
    out.insert("state".to_string(), json!("ok"));
    out.insert("run_id".to_string(), json!(run_id));
    out.insert("skill_id".to_string(), json!(skill_id));
    out.insert("skill_version".to_string(), json!(skill_version));
    out.insert("corpus_size".to_string(), json!(corpus_size));
    out.insert("pass_count".to_string(), json!(pass_count));
    out.insert("degraded_count".to_string(), json!(degraded_count));
    out.insert("fail_count".to_string(), json!(fail_count));
    out.insert(
        "coverage_pct".to_string(),
        json!(pct(scored_count, corpus_size)),
    );
    out.insert(
        "failure_ranking".to_string(),
        json!(ranking
            .into_iter()
            .map(|(d, n)| json!({"dimension": d, "fail_count": n}))
            .collect::<Vec<_>>()),
    );
    if let Some(skill_ref) = skill_ref {
        out.insert("skill_ref".to_string(), json!(skill_ref));
    }
    Value::Object(out)
}

/// `_surface_collisions`' back half (fold.py:206): fold the wave door's
/// answer into the verdict. `None` for unparseable plans (a U/W line): a
/// coverage gap, never a fail; `[]` (clean) -> pass; shared-surface lines ->
/// their rendered text.
pub(crate) fn collisions_from_check(check: Option<&[String]>) -> Option<Vec<String>> {
    let lines = check?;
    if lines
        .iter()
        .any(|l| l.starts_with("U\t") || l.starts_with("W\t"))
    {
        return None;
    }
    Some(
        lines
            .iter()
            .filter(|l| {
                l.starts_with("E\tparallel tasks share surface")
                    || l.starts_with("X\tparallel tasks share surface")
            })
            .map(|l| l.splitn(2, '\t').nth(1).unwrap_or("").to_string())
            .collect(),
    )
}

/// `_surface_collisions`' front half (fold.py:178): does this plan text reach
/// the wave door at all? `None` = unparseable / no frontmatter / strategy
/// malformed (a coverage gap); `Some(true)` = parallel waves present (run
/// the door); `Some(false)` = no waves and at most one task (empty collision
/// set -> pass).
pub(crate) fn plan_reaches_wave_door(plan_text: &str) -> Option<bool> {
    let parsed = crate::plan_doc::codec::parse_frontmatter(plan_text).ok()?;
    if parsed.block.is_empty() {
        return None; // `if not doc.frontmatter` - a PR diff or bare text
    }
    let strategy_text = section_body(plan_text, "Execution Strategy").unwrap_or_default();
    let strategy = parse_execution_strategy(&strategy_text).ok()?;
    let waves = strategy.get("waves").cloned().unwrap_or(json!([]));
    if waves.as_array().is_none_or(|w| w.is_empty()) {
        let tasks = strategy.get("tasks").cloned().unwrap_or(json!([]));
        let Some(tasks) = tasks.as_array() else {
            return None;
        };
        return Some(tasks.len() <= 1);
    }
    Some(true)
}

/// `plan._doc.has_section` / `get_section`: a `## <name>` heading at the
/// start of a line, OUTSIDE fenced code blocks, bounds a section (deeper
/// headings are body). `section_body` returns its trimmed text.
pub(crate) fn section_body(text: &str, name: &str) -> Option<String> {
    let mut current: Option<(String, Vec<String>)> = None;
    let mut in_fence = false;
    let mut fence_marker = String::new();
    for raw_line in text.split('\n') {
        let stripped = raw_line.trim();
        if in_fence {
            if stripped.starts_with(fence_marker.as_str()) {
                in_fence = false;
            }
            if let Some((_, lines)) = current.as_mut() {
                lines.push(raw_line.to_string());
            }
            continue;
        }
        if stripped.starts_with("```") || stripped.starts_with("~~~") {
            fence_marker = stripped
                .chars()
                .take_while(|c| *c == '`' || *c == '~')
                .collect();
            in_fence = true;
            if let Some((_, lines)) = current.as_mut() {
                lines.push(raw_line.to_string());
            }
            continue;
        }
        if let Some(heading) = stripped.strip_prefix("## ").map(str::trim) {
            if let Some((done_name, lines)) = current.take() {
                if done_name == name {
                    return Some(lines.join("\n").trim().to_string());
                }
            }
            current = Some((heading.to_string(), Vec::new()));
            continue;
        }
        if let Some((_, lines)) = current.as_mut() {
            lines.push(raw_line.to_string());
        }
    }
    let (done_name, lines) = current?;
    (done_name == name).then(|| lines.join("\n").trim().to_string())
}

/// Whether the plan carries the `## Five questions` section
/// (`load_plan_text(text).has_section("Five questions")` at the judge gate).
pub(crate) fn has_five_questions(plan_text: &str) -> bool {
    section_body(plan_text, "Five questions").is_some()
}

/// `plan.brief.parse_execution_strategy`, narrowed to the shape the
/// collision check reads. Errors mirror the Python raises (a malformed block
/// is unscorable, not a crash): the fence holds a YAML mapping (a null parse
/// normalizes to empty tasks and waves), `tasks` is a list of mappings with
/// list `surface`s, `waves` is a list.
fn parse_execution_strategy(yaml_text: &str) -> Result<Value, ()> {
    let raw_yaml = fenced_yaml_block(yaml_text).unwrap_or_else(|| yaml_text.to_string());
    let parsed: Value = if raw_yaml.trim().is_empty() {
        Value::Null
    } else {
        serde_yaml_ng::from_str(&raw_yaml).map_err(|_| ())?
    };
    if parsed.is_null() {
        return Ok(json!({"tasks": [], "waves": []}));
    }
    let Some(map) = parsed.as_object() else {
        return Err(());
    };
    let tasks = map.get("tasks").cloned().unwrap_or(json!([]));
    for t in tasks.as_array().ok_or(())? {
        let obj = t.as_object().ok_or(())?;
        if obj
            .get("surface")
            .cloned()
            .unwrap_or(json!([]))
            .as_array()
            .is_none()
        {
            return Err(());
        }
    }
    let waves = map.get("waves").cloned().unwrap_or(json!([]));
    if !waves.is_array() {
        return Err(());
    }
    Ok(parsed)
}

/// The first ```yaml fenced block's content: `plan.brief`'s fence regex
/// (` ```ya?ml\s*\n(.*?)``` `, DOTALL, IGNORECASE) as a line scan. An
/// unterminated fence never matches the regex, so the whole text falls
/// through as bare YAML - mirror that by reporting no block.
fn fenced_yaml_block(text: &str) -> Option<String> {
    let mut in_block = false;
    let mut collected: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let trimmed = line.trim_start();
        if !in_block {
            let lower = trimmed.to_ascii_lowercase();
            if lower.starts_with("```yaml") || lower.starts_with("```yml") {
                in_block = true;
                collected.clear();
            }
        } else if trimmed.starts_with("```") {
            return Some(collected.join("\n"));
        } else {
            collected.push(line);
        }
    }
    None
}

/// Best-effort transcript candidate lines for one session id
/// (`cost/_session_cost.find_transcript` + read): resolve `<sid>.jsonl`
/// through [`build_transcript_index`]'s memo, read it, and keep only
/// [`SKILL_USE_MARKER`] lines (the sole consumer is
/// [`skills_from_transcript`]). None when the id is not a UUID, the index
/// has no entry, or the read fails (GC'd / per-machine).
pub(crate) fn default_read_transcript(session_id: &str) -> Option<Vec<String>> {
    if !is_uuid(session_id) {
        return None;
    }
    let base = home_dir()?.join(".claude").join("projects");
    if !base.is_dir() {
        return None;
    }
    static INDEX: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
    let path = match INDEX.get() {
        Some(index) => index.get(session_id),
        None => {
            let built = build_transcript_index(&base);
            if built.is_empty() {
                // A failed or empty listing is never pinned: pinning one
                // would answer every later lookup in the run with None, so
                // this call pays the walk again instead.
                return None;
            }
            let _ = INDEX.set(built);
            INDEX.get().and_then(|index| index.get(session_id))
        }
    }?;
    std::fs::read_to_string(path).ok().map(|text| {
        text.lines()
            .filter(|line| line.contains(SKILL_USE_MARKER))
            .map(str::to_string)
            .collect()
    })
}

/// Session id -> transcript path, built in one pass over the projects dir.
/// The sweep resolves hundreds of session ids; a read_dir walk per id is
/// O(ids * project dirs) stats on a store that grows into thousands of
/// project dirs, and that walk alone can outlive the sweep's stage bound.
fn build_transcript_index(base: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let Ok(projects) = std::fs::read_dir(base) else {
        return map;
    };
    for project in projects.flatten() {
        let Ok(sessions) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in sessions.flatten() {
            let path = file.path();
            let is_jsonl = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "jsonl");
            if !is_jsonl {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                map.insert(stem.to_string(), path);
            }
        }
    }
    map
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from)
}

/// `_default_skill_version` (scoreboard/fold.py:677): best-effort git hash
/// of the skill's SKILL.md at (or before) ts_raw. No git history / no such
/// file / any subprocess failure -> "unknown", never an error.
pub(crate) fn default_skill_version(
    skill_id: &str,
    ts_raw: Option<&str>,
    repo_root: &Path,
) -> String {
    let until = ts_raw
        .and_then(|ts| parse_ts(&json!(ts)))
        .unwrap_or_else(|| Local::now().naive_local());
    pick_skill_version(&skill_version_history(skill_id, repo_root), until)
}

/// [`default_skill_version`] with the git history read once per repo root
/// and skill. The sweep resolves the same skill id for every corpus item,
/// and each uncached read is a `git log --follow` subprocess, so per-item
/// resolution is a second stage-bound cost on a large corpus.
pub(crate) fn cached_skill_version(
    skill_id: &str,
    ts_raw: Option<&str>,
    repo_root: &Path,
) -> String {
    static HISTORY: OnceLock<Mutex<HashMap<String, Vec<(NaiveDateTime, String)>>>> =
        OnceLock::new();
    let cache = HISTORY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = format!("{}\u{1}{skill_id}", repo_root.display());
    let history = cache
        .entry(key)
        .or_insert_with(|| skill_version_history(skill_id, repo_root));
    let until = ts_raw
        .and_then(|ts| parse_ts(&json!(ts)))
        .unwrap_or_else(|| Local::now().naive_local());
    pick_skill_version(history, until)
}

/// The sorted (commit time, hash) history of the skill's SKILL.md; empty
/// when the file is absent or git fails, which [`pick_skill_version`] reads
/// as the "unknown" contract.
pub(crate) fn skill_version_history(
    skill_id: &str,
    repo_root: &Path,
) -> Vec<(NaiveDateTime, String)> {
    /// `_PHASE_TO_SKILL_DIR` (scoreboard/fold.py:644): the ledger phase and
    /// skill directory are separate vocabularies; old phase rows point at
    /// the renamed skill until the compatibility shim retires.
    fn phase_to_skill_dir(name: &str) -> &str {
        match name {
            "do" | "execute" => "execute",
            other => other,
        }
    }
    let name = phase_to_skill_dir(skill_id.split(':').nth(1).unwrap_or(skill_id));
    let rel = format!("skills/{name}/SKILL.md");
    if !repo_root.join(&rel).exists() {
        return Vec::new();
    }
    let Ok(out) = std::process::Command::new("git")
        .args(["log", "--follow", "--format=%h %aI", "--", &rel])
        .current_dir(repo_root)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut history: Vec<(NaiveDateTime, String)> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((h, iso)) = line.split_once(' ') else {
            continue;
        };
        if let Some(dt) = parse_ts(&json!(iso)) {
            history.push((dt, h.to_string()));
        }
    }
    history.sort_by(|a, b| a.0.cmp(&b.0));
    history
}

/// The newest hash at or before `until`; "unknown" for an empty history.
pub(crate) fn pick_skill_version(
    history: &[(NaiveDateTime, String)],
    until: NaiveDateTime,
) -> String {
    let mut best = "unknown".to_string();
    for (dt, h) in history {
        if *dt > until {
            break;
        }
        best = h.clone();
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The index maps every `.jsonl` under any project dir, by stem, and
    /// skips everything else.
    #[test]
    fn transcript_index_maps_sessions_across_project_dirs() {
        let base = std::env::temp_dir().join(format!("fno-fold-idx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let p1 = base.join("proj-one");
        let p2 = base.join("proj-two");
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::create_dir_all(&p2).unwrap();
        std::fs::write(p1.join("11111111-1111-4111-8111-111111111111.jsonl"), "{}").unwrap();
        std::fs::write(p2.join("22222222-2222-4222-8222-222222222222.jsonl"), "{}").unwrap();
        std::fs::write(p2.join("notes.txt"), "not a transcript").unwrap();

        let index = build_transcript_index(&base);
        assert!(index.contains_key("11111111-1111-4111-8111-111111111111"));
        assert!(index.contains_key("22222222-2222-4222-8222-222222222222"));
        assert_eq!(index.len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The candidate-line guard changes the parse input, not the verdicts:
    /// a real Skill tool_use is found, prose that merely names Skill is not
    /// a run, and a malformed candidate line is skipped.
    #[test]
    fn skill_use_extraction_unchanged_by_candidate_guard() {
        let use_line = r#"{"message":{"content":[{"type":"tool_use","name":"Skill","input":{"skill":"fno:blueprint"}}]}}"#;
        let prose_line =
            r#"{"message":{"content":[{"type":"text","text":"the Skill tool gates delivery"}]}}"#;
        let found = skills_from_transcript(&[use_line, prose_line]);
        assert_eq!(found, vec!["fno:blueprint".to_string()]);
        assert!(skills_from_transcript(&["not json at all but says Skill"]).is_empty());
    }

    /// The pick reads the newest commit at or before `until`; an empty
    /// history is the "unknown" contract, never a fabricated hash.
    #[test]
    fn skill_version_pick_reads_newest_commit_at_or_before_until() {
        let history = vec![
            (
                "2026-01-01T00:00:00".parse::<NaiveDateTime>().unwrap(),
                "aaa1111".to_string(),
            ),
            (
                "2026-02-01T00:00:00".parse::<NaiveDateTime>().unwrap(),
                "bbb2222".to_string(),
            ),
        ];
        assert_eq!(
            pick_skill_version(
                &history,
                "2026-01-15T00:00:00".parse::<NaiveDateTime>().unwrap()
            ),
            "aaa1111"
        );
        assert_eq!(
            pick_skill_version(
                &history,
                "2026-03-01T00:00:00".parse::<NaiveDateTime>().unwrap()
            ),
            "bbb2222"
        );
        assert_eq!(
            pick_skill_version(
                &history,
                "2025-06-01T00:00:00".parse::<NaiveDateTime>().unwrap()
            ),
            "unknown"
        );
        assert_eq!(
            pick_skill_version(&[], "2026-03-01T00:00:00".parse::<NaiveDateTime>().unwrap()),
            "unknown"
        );
    }

    /// The cache answers identically to the uncached read and never leaks
    /// one skill's history into another skill's answer.
    #[test]
    fn cached_skill_version_matches_uncached_per_skill() {
        let repo = std::env::temp_dir().join(format!("fno-fold-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join("skills/blueprint")).unwrap();
        std::fs::create_dir_all(repo.join("skills/review")).unwrap();
        std::fs::write(repo.join("skills/blueprint/SKILL.md"), "b\n").unwrap();
        std::fs::write(repo.join("skills/review/SKILL.md"), "r\n").unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "seed"]);

        let bp_uncached = default_skill_version("fno:blueprint", None, &repo);
        let bp_cached = cached_skill_version("fno:blueprint", None, &repo);
        let rv_cached = cached_skill_version("fno:review", None, &repo);
        let rv_uncached = default_skill_version("fno:review", None, &repo);
        assert_eq!(
            bp_cached, bp_uncached,
            "cached answer diverged from the uncached read"
        );
        assert_eq!(
            rv_cached, rv_uncached,
            "one skill's cached history leaked into the other"
        );
        assert_ne!(bp_cached, rv_cached, "distinct skills resolved to one hash");
        let _ = std::fs::remove_dir_all(&repo);
    }
}
