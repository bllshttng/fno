//! The pure outage fold over explicit records, prior durable state and injected
//! time, plus the journal it persists through.

use super::{
    journal_path, py_number, py_str, py_truthy, OutageEvidence, OutagePolicy, MAX_PANE_SNAPSHOTS,
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

const JOURNAL_VERSION: i64 = 1;
const MAX_EVIDENCE: usize = 512;
const JOURNAL_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

fn refusal(record: &OutageEvidence, reason: &str) -> Value {
    json!({
        "row_id": record.row_id,
        "source": record.source,
        "fingerprint": record.fingerprint(),
        "reason": reason,
    })
}

fn validate(record: &OutageEvidence, now_s: f64, policy: &OutagePolicy) -> Option<&'static str> {
    if record.source != "transcript" && record.source != "pane" {
        return Some("unknown_source");
    }
    if record.row_id.is_empty() || record.harness.is_empty() {
        return Some("unknown_record_identity");
    }
    if record.provider.as_deref().unwrap_or("").is_empty()
        || record.account.as_deref().unwrap_or("").is_empty()
    {
        return Some("unknown_route_identity");
    }
    if record.role != "assistant" {
        return Some("not_assistant_api_record");
    }
    if record.source == "pane" {
        if record.pane_id.as_deref().unwrap_or("").is_empty() {
            return Some("unknown_pane_identity");
        }
        let Some(snapshot_at) = record.snapshot_at.filter(|_| record.persisted) else {
            return Some("pane_not_persisted");
        };
        if snapshot_at > now_s || now_s - snapshot_at > policy.pane_freshness_s {
            return Some("pane_snapshot_stale");
        }
    }
    let age = now_s - record.observed_at;
    // Same rule as the collector: a live reset epoch admits the record however
    // old the line is; otherwise the refusal is named.
    if (age < 0.0 || age > policy.evidence_freshness_s)
        && !(age > 0.0 && record.reset_at.is_some_and(|reset| now_s < reset))
    {
        return Some("evidence_stale");
    }
    if record.raw_kind == "content" && record.raw_status.is_none() {
        return None;
    }
    if record.raw_kind != "api_error" || record.raw_status.is_none() {
        return Some("not_assistant_api_record");
    }
    None
}

struct Vote<'a> {
    record: &'a OutageEvidence,
    kind: &'static str,
    at: f64,
    reset_at: Option<f64>,
}

/// One row's state per session and the votes it casts. `sessions` keeps first
/// sight of the row, because accepted records arrive in observation order.
fn session_summaries<'a>(
    records: &'a [OutageEvidence],
    policy: &OutagePolicy,
) -> (Map<String, Value>, Vec<Vote<'a>>) {
    let mut order: Vec<&str> = Vec::new();
    let mut by_row: HashMap<&str, Vec<&'a OutageEvidence>> = HashMap::new();
    for record in records {
        if !by_row.contains_key(record.row_id.as_str()) {
            order.push(record.row_id.as_str());
        }
        by_row
            .entry(record.row_id.as_str())
            .or_default()
            .push(record);
    }
    let mut sessions: Map<String, Value> = Map::new();
    let mut votes: Vec<Vote<'a>> = Vec::new();
    for row_id in order {
        let mut row_records: Vec<&'a OutageEvidence> = by_row.remove(row_id).unwrap_or_default();
        row_records.sort_by(|a, b| a.observed_at.total_cmp(&b.observed_at));
        let mut consecutive: Vec<&'a OutageEvidence> = Vec::new();
        let mut terminal: Option<&'a OutageEvidence> = None;
        for record in row_records {
            if record.raw_kind == "content" {
                consecutive.clear();
                continue;
            }
            if record.raw_status == Some(529) {
                consecutive.push(record);
                continue;
            }
            consecutive.clear();
            if record.raw_status == Some(429)
                && record.content.to_lowercase().contains("fair usage policy")
            {
                terminal = Some(record);
            }
        }
        if let Some(terminal) = terminal {
            sessions.insert(
                row_id.to_string(),
                json!({
                    "state": "terminal",
                    "kind": "fair_usage_policy",
                    "consecutive": 1,
                    "reset_at": terminal.reset_at,
                    "manual_restoration": terminal.reset_at.is_none(),
                }),
            );
            votes.push(Vote {
                record: terminal,
                kind: "fair_usage_policy",
                at: terminal.observed_at,
                reset_at: terminal.reset_at,
            });
            continue;
        }
        let (Some(first), Some(last)) = (consecutive.first(), consecutive.last()) else {
            continue;
        };
        let span = last.observed_at - first.observed_at;
        let persistent =
            consecutive.len() as i64 >= policy.overload_count && span >= policy.overload_span_s;
        sessions.insert(
            row_id.to_string(),
            json!({
                "state": if persistent { "session_persistent" } else { "retrying" },
                "kind": "overloaded_529",
                "consecutive": consecutive.len(),
                "reset_at": Value::Null,
                "manual_restoration": false,
            }),
        );
        if persistent {
            votes.push(Vote {
                record: *last,
                kind: "overloaded_529",
                at: last.observed_at,
                reset_at: None,
            });
        }
    }
    (sessions, votes)
}

type BreakerKey = (String, String, String);

fn prior_key(item: &Value) -> BreakerKey {
    let field = |name: &str| py_str(item.get(name).unwrap_or(&Value::Null));
    (field("provider"), field("account"), field("kind"))
}

fn breakers(votes: &[Vote<'_>], prior: &[Value], now_s: f64, policy: &OutagePolicy) -> Vec<Value> {
    let mut prior_by_key: HashMap<BreakerKey, &Value> = HashMap::new();
    for item in prior {
        let present = ["provider", "account", "kind"]
            .iter()
            .all(|name| item.get(*name).is_some_and(py_truthy));
        if present {
            prior_by_key.insert(prior_key(item), item);
        }
    }
    let mut grouped: BTreeMap<BreakerKey, Vec<&Vote<'_>>> = BTreeMap::new();
    for vote in votes {
        let key = (
            vote.record.provider.clone().unwrap_or_default(),
            vote.record.account.clone().unwrap_or_default(),
            vote.kind.to_string(),
        );
        grouped.entry(key).or_default().push(vote);
    }
    let mut out: Vec<Value> = Vec::new();
    let mut current_keys: HashSet<BreakerKey> = HashSet::new();
    for (key, got) in grouped {
        let mut order: Vec<&str> = Vec::new();
        let mut newest_by_row: HashMap<&str, &Vote<'_>> = HashMap::new();
        for vote in got {
            let row_id = vote.record.row_id.as_str();
            match newest_by_row.get(row_id) {
                None => {
                    order.push(row_id);
                    newest_by_row.insert(row_id, vote);
                }
                Some(seen) if vote.at > seen.at => {
                    newest_by_row.insert(row_id, vote);
                }
                Some(_) => {}
            }
        }
        let mut distinct: Vec<&Vote<'_>> = order.iter().map(|row| newest_by_row[row]).collect();
        distinct.sort_by(|a, b| a.at.total_cmp(&b.at));
        let fair_usage = key.2 == "fair_usage_policy";
        let window_s = if fair_usage {
            policy.fup_window_s
        } else {
            policy.overload_cross_session_window_s
        };
        if (distinct.len() as i64) < policy.quorum {
            continue;
        }
        // Votes sharing a still-future reset ARE one event: rows noticing the
        // same live cap hours apart still describe one outage, so the spread
        // window (the proxy for votes that cannot) yields.
        let one_live_cap = fair_usage
            && distinct
                .iter()
                .all(|vote| vote.reset_at.is_some_and(|reset| now_s < reset));
        let (Some(first), Some(last)) = (distinct.first(), distinct.last()) else {
            continue;
        };
        if !one_live_cap && last.at - first.at > window_s {
            continue;
        }
        let previous: Option<&Value> = prior_by_key.get(&key).copied();
        let epoch: Value = previous
            .and_then(|item| item.get("outage_epoch"))
            .filter(|value| py_number(value).is_some())
            .cloned()
            .unwrap_or_else(|| json!(first.at));
        let opened_at: Value = previous
            .and_then(|item| item.get("opened_at"))
            .cloned()
            .unwrap_or_else(|| json!(now_s));
        let reset_at: Option<f64> = distinct
            .iter()
            .filter_map(|vote| vote.reset_at)
            .fold(None, |best: Option<f64>, reset| {
                Some(best.map_or(reset, |seen| seen.max(reset)))
            });
        let mut row_ids: Vec<&str> = distinct.iter().map(|v| v.record.row_id.as_str()).collect();
        row_ids.sort_unstable();
        let mut fingerprints: Vec<&str> = distinct.iter().map(|v| v.record.fingerprint()).collect();
        fingerprints.sort_unstable();
        out.push(json!({
            "provider": key.0,
            "account": key.1,
            "kind": key.2,
            "outage_epoch": epoch,
            "opened_at": opened_at,
            "row_ids": row_ids,
            "fingerprints": fingerprints,
            "reset_at": reset_at,
            "manual_restoration": fair_usage && reset_at.is_none(),
            "basis": format!(
                "positive quorum={} across {} distinct rows",
                policy.quorum,
                distinct.len()
            ),
        }));
        current_keys.insert(key);
    }
    for item in prior {
        if current_keys.contains(&prior_key(item)) {
            continue;
        }
        let reset_live = item
            .get("reset_at")
            .and_then(py_number)
            .is_some_and(|reset| now_s <= reset + policy.reset_grace_s);
        if item.get("manual_restoration") == Some(&Value::Bool(true)) || reset_live {
            out.push(item.clone());
        }
    }
    let sort_field = |item: &Value, name: &str| -> String {
        item.get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    out.sort_by_key(|item| {
        (
            sort_field(item, "provider"),
            sort_field(item, "account"),
            sort_field(item, "kind"),
        )
    });
    out
}

/// The anchor a kept pane snapshot ages from.
fn snapshot_anchor(item: &Value) -> Option<f64> {
    item.get("snapshot_at")
        .and_then(py_number)
        .or_else(|| item.get("observed_at").and_then(py_number))
}

fn object_items(prior: Option<&Value>, key: &str) -> Vec<Value> {
    prior
        .and_then(|state| state.get(key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.is_object())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Pure fold over explicit records, prior durable state and injected time.
/// Returns `(report, next_state)`.
pub fn fold_provider_outages(
    records: &[OutageEvidence],
    prior_state: Option<&Value>,
    now_s: f64,
    policy: &OutagePolicy,
) -> (Value, Value) {
    let prior_evidence = object_items(prior_state, "evidence");
    let prior_breakers = object_items(prior_state, "breakers");
    let mut known: HashMap<String, OutageEvidence> = HashMap::new();
    for item in &prior_evidence {
        if let Some(record) = OutageEvidence::from_json(item) {
            known.insert(record.content_fingerprint.clone(), record);
        }
    }
    let mut pane_snapshots: BTreeMap<String, Value> = BTreeMap::new();
    for item in object_items(prior_state, "pane_snapshots") {
        let key = py_str(item.get("fingerprint").unwrap_or(&Value::Null));
        pane_snapshots.insert(key, item);
    }
    let mut refusals: Vec<Value> = Vec::new();
    for record in records {
        if let Some(reason) = validate(record, now_s, policy) {
            refusals.push(refusal(record, reason));
            continue;
        }
        // Fresh wins on a repeated fingerprint: only reset_at can differ, and a
        // resolution made after reset_timezone was set must upgrade the record
        // the journal carried, not stay shadowed by its None.
        known.insert(record.content_fingerprint.clone(), record.clone());
        if record.source == "pane" {
            pane_snapshots.insert(
                record.content_fingerprint.clone(),
                json!({
                    "fingerprint": record.fingerprint(),
                    "pane_id": record.pane_id,
                    "observed_at": record.observed_at,
                    "snapshot_at": record.snapshot_at,
                    "raw_status": record.raw_status,
                    "raw_kind": record.raw_kind,
                    "content": record.content,
                }),
            );
        }
    }
    let mut accepted: Vec<OutageEvidence> = known
        .into_values()
        .filter(|record| validate(record, now_s, policy).is_none())
        .collect();
    accepted.sort_by(|a, b| {
        a.observed_at
            .total_cmp(&b.observed_at)
            .then_with(|| a.content_fingerprint.cmp(&b.content_fingerprint))
    });
    if accepted.len() > MAX_EVIDENCE {
        accepted.drain(..accepted.len() - MAX_EVIDENCE);
    }
    let (sessions, votes) = session_summaries(&accepted, policy);
    let open = breakers(&votes, &prior_breakers, now_s, policy);
    let state_count = |state: &str| -> usize {
        sessions
            .values()
            .filter(|item| item.get("state").and_then(Value::as_str) == Some(state))
            .count()
    };
    let mut counts: Map<String, Value> = Map::new();
    if !accepted.is_empty() {
        counts.insert("accepted".to_string(), json!(accepted.len()));
    }
    if !refusals.is_empty() {
        counts.insert("refused".to_string(), json!(refusals.len()));
    }
    for key in ["terminal", "retrying", "session_persistent"] {
        let count = state_count(key);
        if count > 0 {
            counts.insert(key.to_string(), json!(count));
        }
    }
    if !open.is_empty() {
        counts.insert("open".to_string(), json!(open.len()));
    }
    // A pane snapshot older than the freshness window can never vote or
    // revalidate again, so carrying it (up to 64KB of screen each) only grows
    // the journal this fold rewrites every tick. Keep the fresh set, newest
    // first, bounded to the same cap as the on-disk snapshot dir.
    let mut fresh: Vec<(f64, String, Value)> = pane_snapshots
        .into_values()
        .filter_map(|item| {
            let anchor = snapshot_anchor(&item)?;
            let age = now_s - anchor;
            if age < 0.0 || age > policy.pane_freshness_s {
                return None;
            }
            let fingerprint = item
                .get("fingerprint")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((anchor, fingerprint, item))
        })
        .collect();
    fresh.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    fresh.truncate(MAX_PANE_SNAPSHOTS);
    fresh.sort_by(|a, b| a.1.cmp(&b.1));
    let kept_snapshots: Vec<Value> = fresh.into_iter().map(|(_, _, item)| item).collect();
    let mut fingerprints: Vec<&str> = accepted.iter().map(|r| r.fingerprint()).collect();
    fingerprints.sort_unstable();
    let evidence: Vec<Value> = accepted.iter().map(OutageEvidence::to_json).collect();
    let report = json!({
        "instrument": "measured",
        "breakers": open,
        "counts": counts,
        "refusals": refusals,
        "sessions": sessions,
    });
    let next_state = json!({
        "version": JOURNAL_VERSION,
        "fingerprints": fingerprints,
        "evidence": evidence,
        "breakers": open,
        "pane_snapshots": kept_snapshots,
    });
    (report, next_state)
}

fn unknown_journal(path: &Path) -> Value {
    json!({
        "instrument": "unknown",
        "breakers": [],
        "counts": {"journal_unreadable": 1},
        "refusals": [{"reason": "journal_unreadable", "path": path.display().to_string()}],
    })
}

/// The journal's state. `Err` when it exists and is damaged: a damaged journal
/// reads as an unknown instrument, never as a clean slate.
fn read_journal(path: &Path) -> Result<Value, ()> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path).map_err(|_| ())?;
    let value: Value = serde_json::from_str(&text).map_err(|_| ())?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(())
    }
}

fn write_journal(path: &Path, state: &Value) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".provider-outages-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    let sorted = crate::backlog::decisions_cli::sort_keys(state);
    tmp.write_all(crate::roles::cli::python_json(&sorted).as_bytes())?;
    tmp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// `<journal>.lock`, the flock every journal writer serializes on, held until
/// the returned file drops. None when another writer held it past the bound.
fn lock_journal(path: &Path) -> Option<std::fs::File> {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .ok()?;
    let deadline = Instant::now() + JOURNAL_LOCK_TIMEOUT;
    loop {
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(file);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Fold and atomically persist deduplication and breaker epochs. Returns the
/// report; an unreadable or unlockable journal answers the unknown report.
pub fn measure_and_persist(
    records: &[OutageEvidence],
    now_s: f64,
    path: Option<&Path>,
    policy: &OutagePolicy,
) -> Value {
    let default_path = journal_path();
    let target: &Path = path.unwrap_or(default_path.as_path());
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        if std::fs::create_dir_all(parent).is_err() {
            return unknown_journal(target);
        }
    }
    let Some(_guard) = lock_journal(target) else {
        return unknown_journal(target);
    };
    let Ok(prior) = read_journal(target) else {
        return unknown_journal(target);
    };
    let (report, state) = fold_provider_outages(records, Some(&prior), now_s, policy);
    if write_journal(target, &state).is_err() {
        return unknown_journal(target);
    }
    report
}
