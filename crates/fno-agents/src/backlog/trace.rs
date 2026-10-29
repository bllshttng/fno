//! `fno backlog get <id> --trace`: one node's history as JSON.
//!
//! The fold writes nothing. Phases come from the graph `sessions` rows, the
//! review span from review_invocation and review_attestation rows whose
//! branch names the node, and tasks from the plan with the task_started and
//! task_done events the commit hook lands. Error and mess-up counts read
//! the session transcripts and the node's event rows. A value no source
//! answered reads null with a reason beside it, never 0.

use crate::wave::{plan_tasks, PlanTask};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

const PHASES: [&str; 5] = ["think", "blueprint", "execute", "review", "ship"];

/// A transcript over this size is not read: the trace is an on-demand read
/// and one runaway session must not stall it.
const TRANSCRIPT_CAP_BYTES: u64 = 64 * 1024 * 1024;

/// The per-session counts; reopens and retracted decisions are node-level
/// and appear in the totals only.
const SESSION_KINDS: [&str; 5] = [
    "tool_errors",
    "refusals",
    "retractions",
    "user_corrections",
    "interruptions",
];
/// The kinds a transcript timestamps, so a window can count them.
const TIMED_KINDS: [&str; 2] = ["tool_errors", "refusals"];

/// One session transcript's reading. `marks` are the timestamped tool
/// errors and refusals a window can count.
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionScan {
    pub(crate) ai_turns: Option<u64>,
    pub(crate) tool_calls: Option<u64>,
    pub(crate) counts: BTreeMap<&'static str, Option<u64>>,
    pub(crate) marks: Vec<(i64, &'static str)>,
    /// Why a null count is null.
    pub(crate) reason: Option<String>,
    /// First and last row timestamps of the transcript.
    pub(crate) span: Option<(i64, i64)>,
}

/// Everything the fold reads, gathered first so the fold itself is pure.
pub(crate) struct Inputs {
    /// The node's own event rows (task boundaries first).
    pub(crate) events: Result<Vec<Value>, String>,
    /// The review rows whose branch names the node.
    pub(crate) reviews: Vec<Value>,
    pub(crate) cost_usd: Option<f64>,
    pub(crate) tasks: Result<Vec<PlanTask>, String>,
    pub(crate) scans: HashMap<String, SessionScan>,
}

impl Default for Inputs {
    fn default() -> Self {
        Inputs {
            events: Ok(Vec::new()),
            reviews: Vec::new(),
            cost_usd: None,
            tasks: Ok(Vec::new()),
            scans: HashMap::new(),
        }
    }
}

fn ms_of(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The task id an event carries, at envelope top level or in `data`.
fn task_of(row: &Value) -> Option<&str> {
    str_of(row, "task").or_else(|| row.get("data").and_then(|d| str_of(d, "task")))
}

/// True when `branch` names `node` as a whole token (`feature/x-aaaa-...`).
pub(crate) fn branch_names(branch: &str, node: &str) -> bool {
    branch.match_indices(node).any(|(at, _)| {
        let before = branch[..at].chars().next_back();
        let after = branch[at + node.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric())
            && !after.is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

/// Add `n` to `counts[kind]`, keeping a null null.
fn add(counts: &mut Map<String, Value>, kind: &str, n: Option<u64>) {
    let next = match (counts.get(kind), n) {
        (Some(Value::Null), _) | (_, None) => Value::Null,
        (Some(v), Some(n)) => json!(v.as_u64().unwrap_or(0) + n),
        (None, Some(n)) => json!(n),
    };
    counts.insert(kind.into(), next);
}

fn errors_value(counts: Map<String, Value>, reasons: Map<String, Value>) -> Value {
    let mut out = counts;
    if !reasons.is_empty() {
        out.insert("unread".into(), Value::Object(reasons));
    }
    Value::Object(out)
}

/// Merge one errors object into running totals, carrying null and reasons.
fn merge(totals: &mut Map<String, Value>, reasons: &mut Map<String, Value>, errors: &Value) {
    let Some(obj) = errors.as_object() else {
        return;
    };
    for (k, v) in obj {
        if k == "unread" {
            for (rk, rv) in v.as_object().into_iter().flatten() {
                reasons.entry(rk.clone()).or_insert_with(|| rv.clone());
            }
            continue;
        }
        add(totals, k, v.as_u64());
    }
}

/// How many of the node's events have `event_type` (with `origin`, when
/// named); null when the store was not read.
fn event_count(inp: &Inputs, event_type: &str, origin: Option<&str>) -> Option<u64> {
    let rows = inp.events.as_ref().ok()?;
    let n = rows
        .iter()
        .filter(|r| str_of(r, "type") == Some(event_type))
        .filter(|r| origin.is_none() || r.pointer("/data/origin").and_then(Value::as_str) == origin)
        .count();
    Some(n as u64)
}

/// The trace for one node from gathered inputs.
pub(crate) fn fold(node: &Value, inp: &Inputs) -> Value {
    let id = str_of(node, "id").unwrap_or_default();
    let sessions: Vec<&Value> = node
        .get("sessions")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let mut phases: Vec<Value> = PHASES
        .iter()
        .map(|phase| phase_row(phase, &sessions, inp))
        .collect();
    // Red CI lands on ship.
    let ci_red = event_count(inp, "friction", Some("ci_failure"));
    let ship = phases[4]["errors"]
        .as_object_mut()
        .expect("errors is an object");
    ship.insert("ci_red".into(), json!(ci_red));
    let mut totals = Map::new();
    let mut reasons = Map::new();
    if ci_red.is_none() {
        reasons.insert("ci_red".into(), json!("event store not read"));
    }
    for p in &phases {
        merge(&mut totals, &mut reasons, &p["errors"]);
    }
    for (kind, event_type) in [
        ("reopens", "backlog_reopened"),
        ("decision_retractions", "decision_retracted"),
    ] {
        let n = event_count(inp, event_type, None);
        add(&mut totals, kind, n);
        if n.is_none() {
            reasons.insert(kind.into(), json!("event store not read"));
        }
    }

    let started: Vec<i64> = phases
        .iter()
        .filter_map(|p| p["started_at"].as_str().and_then(ms_of))
        .collect();
    // A phase still open leaves the node open: no end, no duration.
    let ended: Vec<i64> = if phases.iter().any(|p| p["open"] == true) {
        Vec::new()
    } else {
        phases
            .iter()
            .filter_map(|p| p["ended_at"].as_str().and_then(ms_of))
            .collect()
    };
    let mut header = json!({
        "started_at": started.iter().min().map(|m| stamp(*m)),
        "ended_at": ended.iter().max().map(|m| stamp(*m)),
        "duration_s": started.iter().min().zip(ended.iter().max()).map(|(s, e)| (e - s).max(0) / 1000),
        "cost_usd": inp.cost_usd,
        "sessions": sessions.len(),
        "ai_turns": sum_scans(&sessions, inp, |s| s.ai_turns),
        "tool_calls": sum_scans(&sessions, inp, |s| s.tool_calls),
    });
    if inp.cost_usd.is_none() {
        header["cost_reason"] = json!("no cost recorded");
    }
    let (tasks, tasks_reason) = task_rows(inp);
    json!({
        "node": id,
        "title": str_of(node, "title"),
        "status": str_of(node, "status"),
        "header": header,
        "phases": phases,
        "tasks": tasks,
        "tasks_reason": tasks_reason,
        "errors": errors_value(totals, reasons),
    })
}

fn sum_scans(sessions: &[&Value], inp: &Inputs, f: fn(&SessionScan) -> Option<u64>) -> Option<u64> {
    sessions.iter().try_fold(0u64, |acc, s| {
        let scan = inp.scans.get(str_of(s, "session_id").unwrap_or_default())?;
        Some(acc + f(scan)?)
    })
}

/// One session's counts inside its phase window. A transcript that lies
/// inside the window answers whole. One that spans other work (a lead
/// session that ran a blueprint) counts only its timestamped marks in the
/// window, and its untimed kinds read null.
fn session_counts(
    scan: &SessionScan,
    window: (i64, i64),
) -> Vec<(&'static str, Option<u64>, Option<String>)> {
    let inside = scan
        .span
        .is_some_and(|(first, last)| first >= window.0 && last <= window.1);
    SESSION_KINDS
        .iter()
        .map(|kind| {
            let whole = scan.counts.get(kind).copied().flatten();
            if whole.is_none() {
                let reason = scan.reason.clone().unwrap_or_else(|| "not read".into());
                return (*kind, None, Some(reason));
            }
            if inside {
                return (*kind, whole, None);
            }
            if TIMED_KINDS.contains(kind) {
                let n = scan
                    .marks
                    .iter()
                    .filter(|(at, k)| k == kind && *at >= window.0 && *at <= window.1)
                    .count() as u64;
                return (*kind, Some(n), None);
            }
            (*kind, None, Some("session spans other work".into()))
        })
        .collect()
}

/// One task's recorded span and outcome from its events.
type Span = (Option<i64>, Option<i64>, Option<String>);

fn task_span(events: &[Value], task: &str) -> Span {
    let of = |t: &str| -> Vec<&Value> {
        events
            .iter()
            .filter(|r| str_of(r, "type") == Some(t) && task_of(r) == Some(task))
            .collect()
    };
    let ts = |r: &&Value| str_of(r, "ts").and_then(ms_of);
    let outcome = of("task_done")
        .last()
        .and_then(|r| str_of(r, "outcome"))
        .map(str::to_string)
        .or_else(|| of("blocked").last().map(|_| "BLOCKED".to_string()));
    (
        of("task_started").iter().filter_map(ts).min(),
        of("task_done").iter().filter_map(ts).max(),
        outcome,
    )
}

/// The plan's tasks in plan order with their recorded spans, plus the
/// reason the list is empty.
fn task_rows(inp: &Inputs) -> (Vec<Value>, Value) {
    let plan = match &inp.tasks {
        Ok(t) => t,
        Err(reason) => return (Vec::new(), json!(reason)),
    };
    let events: &[Value] = inp.events.as_deref().unwrap_or(&[]);
    let rows = plan
        .iter()
        .map(|task| {
            let (start, done, outcome) = task_span(events, &task.id);
            let mut row = json!({
                "id": task.id,
                "title": task.title,
                "started_at": start.map(stamp),
                "ended_at": done.map(stamp),
                "outcome": outcome,
            });
            if start.is_none() && done.is_none() {
                row["reason"] = json!(match &inp.events {
                    Ok(_) => "not recorded".to_string(),
                    Err(e) => format!("event store not read: {e}"),
                });
            }
            row
        })
        .collect();
    (rows, Value::Null)
}

/// One phase: its span over its session rows and its sessions. Review adds
/// the span of the node's review rows.
fn phase_row(phase: &str, sessions: &[&Value], inp: &Inputs) -> Value {
    let rows: Vec<&&Value> = sessions
        .iter()
        .filter(|s| str_of(s, "phase") == Some(phase))
        .collect();
    let mut starts: Vec<i64> = rows
        .iter()
        .filter_map(|s| str_of(s, "started_at").and_then(ms_of))
        .collect();
    let mut ends: Vec<i64> = rows
        .iter()
        .filter_map(|s| str_of(s, "ended_at").and_then(ms_of))
        .collect();
    let open = rows.iter().any(|s| str_of(s, "ended_at").is_none());
    let mut counts = Map::new();
    let mut reasons = Map::new();
    for s in &rows {
        let window = (
            str_of(s, "started_at").and_then(ms_of).unwrap_or(i64::MIN),
            str_of(s, "ended_at").and_then(ms_of).unwrap_or(i64::MAX),
        );
        let missing;
        let found = inp.scans.get(str_of(s, "session_id").unwrap_or_default());
        let scan = match found {
            Some(scan) => scan,
            None => {
                missing = SessionScan {
                    reason: Some("not on this host".into()),
                    ..Default::default()
                };
                &missing
            }
        };
        for (kind, n, reason) in session_counts(scan, window) {
            add(&mut counts, kind, n);
            if let Some(reason) = reason {
                reasons.insert(kind.into(), json!(reason));
            }
        }
    }
    let listed: Vec<Value> = rows
        .iter()
        .map(|s| {
            json!({
                "session_id": str_of(s, "session_id"),
                "harness": str_of(s, "harness"),
                "model": s.pointer("/observed_model/model").or_else(|| s.get("observed_model").filter(|m| m.is_string())),
                "started_at": str_of(s, "started_at"),
                "ended_at": str_of(s, "ended_at"),
            })
        })
        .collect();
    let derived = phase == "review" && !inp.reviews.is_empty();
    if derived {
        let at = |t: &str| {
            inp.reviews
                .iter()
                .filter(|r| str_of(r, "type") == Some(t))
                .filter_map(|r| str_of(r, "ts").and_then(ms_of))
                .collect::<Vec<i64>>()
        };
        starts.extend(at("review_invocation"));
        let attested = at("review_attestation");
        ends.extend(attested.iter().copied());
        if starts.is_empty() {
            starts.extend(attested.iter().min());
        }
        let (mut findings, mut fails) = (0u64, 0u64);
        for a in inp
            .reviews
            .iter()
            .filter(|r| str_of(r, "type") == Some("review_attestation"))
        {
            findings += a
                .pointer("/data/findings_blocking")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            fails += u64::from(a.pointer("/data/verdict").and_then(Value::as_str) == Some("fail"));
        }
        add(&mut counts, "review_findings", Some(findings));
        add(&mut counts, "review_fails", Some(fails));
    }
    let mut row = json!({
        "phase": phase,
        "started_at": starts.iter().min().map(|m| stamp(*m)),
        "ended_at": if open && !derived { None } else { ends.iter().max().map(|m| stamp(*m)) },
        "sessions": listed,
        "errors": errors_value(counts, reasons),
    });
    if row["started_at"].is_null() {
        row["start_reason"] = json!(if rows.is_empty() && !derived {
            "no session recorded"
        } else {
            "no start recorded"
        });
    }
    if open && !derived {
        row["open"] = json!(true);
    }
    if derived {
        row["derived_from"] = json!("review events");
    }
    row
}

/// Read one claude or codex transcript into counts and timestamped marks.
pub(crate) fn scan_transcript(harness: &str, path: Option<PathBuf>) -> SessionScan {
    let unread = |reason: String| SessionScan {
        counts: SESSION_KINDS.iter().map(|k| (*k, None)).collect(),
        reason: Some(reason),
        ..Default::default()
    };
    let Some(path) = path else {
        return unread(match harness {
            "claude" | "codex" => "not on this host".to_string(),
            other => format!("no transcript reader for {other}"),
        });
    };
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > TRANSCRIPT_CAP_BYTES) {
        return unread("transcript over 64 MB".into());
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => return unread(format!("transcript unreadable: {e}")),
    };
    if harness == "codex" {
        // A codex rollout carries no is_error flag and no compaction windows:
        // only its refusals are measured.
        let rate = crate::refusal_rate::rate_from_text("codex", &raw, usize::MAX, false).ok();
        let mut scan = unread("codex rollout carries no such count".into());
        scan.counts.insert(
            "refusals",
            rate.as_ref().and_then(|r| r["refused"].as_u64()),
        );
        scan.tool_calls = rate.as_ref().and_then(|r| r["total"].as_u64());
        return scan;
    }
    scan_claude(&raw)
}

fn scan_claude(raw: &str) -> SessionScan {
    let mut scan = SessionScan::default();
    let (mut turns, mut calls, mut errors, mut refusals) = (0u64, 0u64, 0u64, 0u64);
    let mut seen = std::collections::HashSet::new();
    for line in raw.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = str_of(&row, "timestamp").and_then(ms_of);
        if let Some(t) = at {
            scan.span = Some(scan.span.map_or((t, t), |(a, b)| (a.min(t), b.max(t))));
        }
        let blocks = row
            .pointer("/message/content")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let of = |kind: &'static str| {
            blocks
                .iter()
                .filter(move |b| str_of(b, "type") == Some(kind))
        };
        match str_of(&row, "type") {
            Some("assistant") => {
                let key = row
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .unwrap_or(line);
                turns += u64::from(seen.insert(key.to_string()));
                calls += of("tool_use").count() as u64;
            }
            Some("user") => {
                for b in of("tool_result") {
                    let is_error = b.get("is_error").and_then(Value::as_bool);
                    let text = match b.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Array(parts)) => {
                            let texts: Vec<&str> =
                                parts.iter().filter_map(|p| str_of(p, "text")).collect();
                            texts.join("\n")
                        }
                        _ => String::new(),
                    };
                    if is_error == Some(true) {
                        errors += 1;
                        scan.marks.extend(at.map(|t| (t, "tool_errors")));
                    }
                    if crate::refusal_rate::is_refused(&text, is_error) {
                        refusals += 1;
                        scan.marks.extend(at.map(|t| (t, "refusals")));
                    }
                }
            }
            _ => {}
        }
    }
    let corrections = crate::correction_meter::correction_totals(raw);
    scan.ai_turns = Some(turns);
    scan.tool_calls = Some(calls);
    scan.counts.insert("tool_errors", Some(errors));
    scan.counts.insert("refusals", Some(refusals));
    for k in ["retractions", "user_corrections", "interruptions"] {
        scan.counts.insert(k, corrections[k].as_u64());
    }
    scan
}

/// The node's own project store, from its `project` root in the work map,
/// else this checkout's store.
fn store_of(node: &Value) -> std::path::PathBuf {
    str_of(node, "project")
        .and_then(super::settings::project_root)
        .map(|root| crate::paths::events_path(std::path::Path::new(&root)))
        .unwrap_or_else(crate::paths::project_events_path)
}

/// Shared reads for one trace call: the store, the ledger and each session
/// transcript, read once even when an epic folds many children.
pub(crate) struct Shared {
    events_path: PathBuf,
    store_ok: Result<(), String>,
    reviews: Vec<Value>,
    ledger: super::scorecard::LedgerIndex,
    transcripts: crate::context_run::SessionTranscripts,
    /// One scan per session: an epic's children often share a lead session.
    scanned: std::cell::RefCell<HashMap<String, SessionScan>>,
}

impl Shared {
    pub(crate) fn load(root: &Value) -> Shared {
        let events_path = store_of(root);
        let store_ok = crate::event_store::import_all(&events_path).map(|_| ());
        let reviews = crate::event_store::query_events(
            &events_path,
            &crate::event_store::EventQuery::of_types(&["review_invocation", "review_attestation"]),
        )
        .map(|rows| {
            rows.iter()
                .filter_map(|r| serde_json::from_str(&r.line).ok())
                .collect()
        })
        .unwrap_or_default();
        Shared {
            events_path,
            store_ok,
            reviews,
            ledger: super::scorecard::LedgerIndex::load(),
            transcripts: crate::context_run::SessionTranscripts::default(),
            scanned: Default::default(),
        }
    }

    /// The inputs for one node.
    pub(crate) fn gather(&self, node: &Value) -> Inputs {
        let id = str_of(node, "id").unwrap_or_default();
        let events = self.store_ok.clone().and_then(|_| {
            crate::event_store::query_events(
                &self.events_path,
                &crate::event_store::EventQuery {
                    node_id: Some(id.to_string()),
                    ..Default::default()
                },
            )
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| serde_json::from_str(&r.line).ok())
                    .collect()
            })
        });
        let reviews = self
            .reviews
            .iter()
            .filter(|r| {
                r.pointer("/data/branch")
                    .and_then(Value::as_str)
                    .is_some_and(|b| branch_names(b, id))
            })
            .cloned()
            .collect();
        let tasks = match str_of(node, "plan_path") {
            Some(p) => plan_tasks(Path::new(p)),
            None => Err("no plan".to_string()),
        };
        let mut scans = HashMap::new();
        for s in node
            .get("sessions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(sid), harness) =
                (str_of(s, "session_id"), str_of(s, "harness").unwrap_or(""))
            else {
                continue;
            };
            let scan = self
                .scanned
                .borrow_mut()
                .entry(sid.to_string())
                .or_insert_with(|| scan_transcript(harness, self.transcripts.find(sid, harness)))
                .clone();
            scans.insert(sid.to_string(), scan);
        }
        Inputs {
            events,
            reviews,
            cost_usd: super::scorecard::node_cost_usd(node, id, &self.ledger),
            tasks,
            scans,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(errors: u64, corrections: u64, marks: Vec<(i64, &'static str)>) -> SessionScan {
        let mut s = SessionScan::default();
        for k in SESSION_KINDS {
            s.counts.insert(k, Some(0));
        }
        s.counts.insert("tool_errors", Some(errors));
        s.counts.insert("user_corrections", Some(corrections));
        s.span = marks.first().map(|(t, _)| (*t, *t));
        s.marks = marks;
        s
    }

    fn session(phase: &str, sid: &str, start: Option<&str>, end: Option<&str>) -> Value {
        json!({"phase": phase, "harness": "claude", "session_id": sid,
               "started_at": start, "ended_at": end})
    }

    #[test]
    fn phase_spans_fold_and_an_open_phase_leaves_the_node_open() {
        let node = json!({"id": "x-1", "sessions": [
            session("think", "a", Some("2026-10-01T00:00:00Z"), Some("2026-10-01T01:00:00Z")),
            session("blueprint", "b", None, Some("2026-10-02T01:00:00Z")),
            session("execute", "c", Some("2026-10-03T00:00:00Z"), None),
        ]});
        let review = json!({"type": "review_attestation", "ts": "2026-10-03T02:00:00Z"});
        let inp = Inputs {
            reviews: vec![review],
            ..Default::default()
        };
        let t = fold(&node, &inp);
        assert_eq!(t["phases"][0]["started_at"], "2026-10-01T00:00:00Z");
        assert_eq!(t["phases"][1]["start_reason"], "no start recorded");
        assert_eq!(t["phases"][2]["open"], true);
        assert_eq!(t["phases"][3]["started_at"], "2026-10-03T02:00:00Z");
        assert_eq!(t["phases"][4]["start_reason"], "no session recorded");
        assert!(t["header"]["ended_at"].is_null());
        assert_eq!(t["header"]["cost_reason"], "no cost recorded");
        assert!(branch_names("feature/x-aaaa-trace", "x-aaaa"));
        assert!(!branch_names("feature/x-aaaa0", "x-aaaa"));
    }

    #[test]
    fn counts_fold_per_phase_and_an_unread_transcript_reads_null() {
        let node = json!({"id": "x-1", "sessions": [
            session("execute", "c", Some("2026-10-03T00:00:00Z"), Some("2026-10-03T05:00:00Z")),
            session("ship", "gone", Some("2026-10-03T05:00:00Z"), Some("2026-10-03T06:00:00Z")),
        ]});
        let at = ms_of("2026-10-03T00:30:00Z").unwrap();
        let mut scans = HashMap::new();
        scans.insert(
            "c".to_string(),
            scan(2, 1, vec![(at, "tool_errors"), (at, "tool_errors")]),
        );
        let red = json!({"type": "friction", "data": {"origin": "ci_failure"}});
        let inp = Inputs {
            events: Ok(vec![red]),
            scans,
            ..Default::default()
        };
        let t = fold(&node, &inp);
        assert_eq!(t["phases"][2]["errors"]["tool_errors"], 2);
        assert_eq!(t["phases"][2]["errors"]["user_corrections"], 1);
        assert_eq!(t["phases"][4]["errors"]["ci_red"], 1);
        // The ship session's transcript is absent: null with a reason.
        assert!(t["phases"][4]["errors"]["tool_errors"].is_null());
        assert_eq!(t["errors"]["unread"]["tool_errors"], "not on this host");
        assert!(t["errors"]["tool_errors"].is_null());
        assert_eq!(t["errors"]["ci_red"], 1);
        assert_eq!(t["errors"]["reopens"], 0);
    }

    #[test]
    fn plan_tasks_read_in_plan_order_with_their_spans() {
        let ev = |t: &str, task: &str, ts: &str| json!({"type": t, "task": task, "ts": ts});
        let tasks = ["2.1", "1.1", "1.2"]
            .iter()
            .map(|id| PlanTask {
                id: id.to_string(),
                title: format!("t{id}"),
            })
            .collect();
        let inp = Inputs {
            events: Ok(vec![
                ev("task_started", "1.1", "2026-10-03T00:00:00Z"),
                ev("task_done", "1.1", "2026-10-03T01:00:00Z"),
            ]),
            tasks: Ok(tasks),
            ..Default::default()
        };
        let t = fold(&json!({"id": "x-1"}), &inp);
        let ids: Vec<&str> = t["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["2.1", "1.1", "1.2"]);
        assert_eq!(t["tasks"][1]["ended_at"], "2026-10-03T01:00:00Z");
        assert_eq!(t["tasks"][1]["outcome"], Value::Null);
        assert_eq!(t["tasks"][2]["reason"], "not recorded");

        let bare = Inputs {
            tasks: Err("no plan".into()),
            ..Default::default()
        };
        let t = fold(&json!({"id": "x-2"}), &bare);
        assert_eq!(
            (t["tasks"].clone(), t["tasks_reason"].clone()),
            (json!([]), json!("no plan"))
        );
    }
}
