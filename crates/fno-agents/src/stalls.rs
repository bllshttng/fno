//! `stalls`: why work stalls, one cause table. The native fold behind the
//! Python `fno doctor stalls` shell.
//!
//! Same split as `intel` and `evals-macro`: Python resolves the journal
//! defaults and shells HERE; the binary owns the fold, so the file-budget
//! Python-tree ratchet holds. Daemon-free read, not a routable `fno agents`
//! verb.
//!
//! The report closes the four attribution gaps the 2026-09-29 hand analysis
//! named: a timeout row now carries what it waited on, a NoProgress stop
//! carries its PR/CI/red-check state (legacy rows are joined from the
//! session's previous loop_check row, the join the analysis did by hand),
//! codex sandbox refusals ride `sandbox_denial` events instead of living
//! only in transcripts, and every cause counts DISTINCT sessions beside its
//! row count - one stalled session re-firing twenty stops must not read as
//! twenty problems. `unattributed` is its own row and never folds away: a
//! row with no cause evidence stays visible as an unknown, not a zero.

use crate::client_verbs::parse_iso8601 as parse_loose_ts;
use crate::evals_macro::canonical_json;
use crate::evals_macro::parse_since;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

const EXIT_USAGE: i32 = 2;

/// The event types the fold reads; everything else is skipped at parse so a
/// huge journal costs its interesting rows, not its size.
const TYPES: &[&str] = &[
    "loop_check",
    "loop_check_gh_error",
    "loop_check_graphql_standdown",
    "termination",
    "sandbox_denial",
];

const USAGE: &str = "stalls --events <journal.jsonl> [--events ...] \
[--since 7d|ISO] [--until ISO] [--json]";

/// One cause row of the weekly table.
#[derive(Default)]
struct CauseRow {
    rows: usize,
    sessions: HashSet<String>,
    nodes: HashSet<String>,
    /// YYYY-MM-DD of the first and last row in the window.
    first_day: Option<String>,
    last_day: Option<String>,
    /// Per-cause detail split, cause-specific (waited_on for gh_timeout,
    /// class for codex_sandbox, check name for noprogress_ci_red).
    detail: BTreeMap<String, usize>,
}

/// The fixed cause registry, in print order. The last two are the honest
/// leftovers the report must never fold away.
const CAUSES: &[&str] = &[
    "gh_secondary_limit",
    "gh_budget_refusal",
    "gh_timeout",
    "graphql_standdown",
    "codex_sandbox",
    "claude_classifier",
    "noprogress_ci_red",
    "noprogress_other",
    "unattributed",
];

/// One journal row projected into the fold's join shape.
struct Row {
    ts: Option<chrono::DateTime<chrono::Utc>>,
    day: Option<String>,
    event_type: Option<String>,
    session: Option<String>,
    node: Option<String>,
    data: Value,
}

impl Row {
    fn from_event(event: &Value) -> Row {
        let data = event
            .get("data")
            .filter(|d| d.is_object())
            .cloned()
            .unwrap_or(Value::Null);
        let raw_ts = event
            .get("ts")
            .filter(|v| !v.is_null())
            .or_else(|| event.get("timestamp"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let ts = raw_ts.as_deref().and_then(parse_loose_ts);
        let day = ts.map(|t| t.format("%Y-%m-%d").to_string());
        Row {
            ts,
            day,
            event_type: event
                .get("type")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            session: data
                .get("session_id")
                .and_then(|v| v.as_str())
                .or_else(|| event.get("session_id").and_then(|v| v.as_str()))
                .map(str::to_string),
            node: ["node_id", "graph_node_id"]
                .iter()
                .find_map(|k| {
                    event
                        .get(k)
                        .or_else(|| data.get(k))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .or_else(|| {
                    data.get("key")
                        .and_then(|v| v.as_str())
                        .filter(|k| k.starts_with("node:"))
                        .map(|k| k["node:".len()..].to_string())
                }),
            data,
        }
    }
}

/// The gh_error cause one row proves, from its own fields only. Ordering
/// matters: the budget gate's refusal text wins over a GitHub wording that
/// a local refusal may quote, and a timeout is its own bucket with the
/// waited_on evidence as detail.
fn gh_error_cause(data: &Value) -> (&'static str, Option<String>) {
    let outcome = data.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
    let stderr = data
        .get("stderr_tail")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if outcome == "budget_refused" || stderr.contains("gh budget:") {
        return ("gh_budget_refusal", None);
    }
    if data.get("rate_limit_class").and_then(|v| v.as_str()) == Some("secondary")
        || (stderr.contains("HTTP 403")
            || stderr.contains("HTTP 429")
            || stderr.contains("secondary rate limit")
            || stderr.contains("rate limit"))
    {
        return ("gh_secondary_limit", None);
    }
    if outcome == "timeout" {
        let waited = data
            .get("waited_on")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return ("gh_timeout", Some(waited.to_string()));
    }
    ("unattributed", None)
}

/// The folded table over the window's rows: every cause of the registry,
/// in print order, plus the NoProgress totals the red-cause row splits.
struct Fold {
    table: BTreeMap<&'static str, CauseRow>,
    noprogress_total: usize,
    noprogress_joined: usize,
}

/// Build the fold. `legacy_join` walks each session's loop_check history
/// for the NoProgress rows that predate the structured fields - the join
/// the 2026-09-29 hand analysis did by hand.
fn fold_rows(rows: &[Row]) -> Fold {
    // session -> ordered (ts, ci) history for the legacy join.
    let mut ci_history: BTreeMap<&str, Vec<(Option<chrono::DateTime<chrono::Utc>>, &str)>> =
        BTreeMap::new();
    for row in rows {
        if row.event_type.as_deref() == Some("loop_check") {
            if let (Some(session), Some(ci)) = (
                row.session.as_deref(),
                row.data.get("ci").and_then(|v| v.as_str()),
            ) {
                ci_history.entry(session).or_default().push((row.ts, ci));
            }
        }
    }
    let mut fold = Fold {
        table: CAUSES.iter().map(|c| (*c, CauseRow::default())).collect(),
        noprogress_total: 0,
        noprogress_joined: 0,
    };
    for row in rows {
        let Some(event_type) = row.event_type.as_deref() else {
            continue;
        };
        let (cause, detail): (&str, Option<String>) = match event_type {
            "loop_check_gh_error" => gh_error_cause(&row.data),
            "loop_check_graphql_standdown" => ("graphql_standdown", None),
            "sandbox_denial" => {
                let harness = row
                    .data
                    .get("harness")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let class = row.data.get("class").and_then(|v| v.as_str()).unwrap_or("");
                let label = format!("{harness}:{class}");
                (
                    match harness {
                        "codex" => "codex_sandbox",
                        "claude" => "claude_classifier",
                        _ => "unattributed",
                    },
                    Some(label),
                )
            }
            "termination" => {
                if row.data.get("reason").and_then(|v| v.as_str()) != Some("NoProgress") {
                    continue;
                }
                fold.noprogress_total += 1;
                match no_progress_ci(&row.data, row.ts, &ci_history, row.session.as_deref()) {
                    CiEvidence::Red { check } => ("noprogress_ci_red", check),
                    CiEvidence::JoinedRed { check } => {
                        fold.noprogress_joined += 1;
                        ("noprogress_ci_red", check)
                    }
                    CiEvidence::NoRed => ("noprogress_other", None),
                }
            }
            _ => continue,
        };
        let entry = fold
            .table
            .get_mut(cause)
            .expect("registry causes are pre-inserted");
        entry.rows += 1;
        if let Some(session) = &row.session {
            entry.sessions.insert(session.clone());
        }
        if let Some(node) = &row.node {
            entry.nodes.insert(node.clone());
        }
        if let Some(day) = &row.day {
            if entry.first_day.is_none() || *day < entry.first_day.clone().unwrap_or_default() {
                entry.first_day = Some(day.clone());
            }
            if entry.last_day.is_none() || *day > entry.last_day.clone().unwrap_or_default() {
                entry.last_day = Some(day.clone());
            }
        }
        if let Some(d) = detail {
            *entry.detail.entry(d).or_insert(0) += 1;
        }
    }
    fold
}

/// Read the interesting rows from every journal. Dedup by canonical JSON
/// signature: the same row rides both project and global journals.
fn read_rows(paths: &[PathBuf], since_ms: i64, until_ms: i64) -> (Vec<Row>, usize) {
    let mut rows: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut malformed = 0usize;
    for path in paths {
        let content = match crate::event_store::journal_text_checked(
            path,
            &crate::event_store::EventQuery {
                since_ms: Some(since_ms),
                until_ms: Some(until_ms),
                types: TYPES.iter().map(|t| t.to_string()).collect(),
                include_rejected: true,
                ..Default::default()
            },
        ) {
            Ok(c) => c,
            Err(_) => {
                malformed += 1;
                continue;
            }
        };
        for line in content.lines() {
            let event: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => {
                    malformed += 1;
                    continue;
                }
            };
            if !event.is_object() {
                malformed += 1;
                continue;
            }
            if seen.insert(canonical_json(&event)) {
                rows.push(event);
            }
        }
    }
    (
        rows.into_iter().map(|e| Row::from_event(&e)).collect(),
        malformed,
    )
}

/// `fno-agents stalls`: fold the window into the cause table. The Python
/// shell resolves the default journal list; the binary needs at least one
/// `--events` path to read.
pub fn run_stalls(args: &[String]) -> i32 {
    let mut since = "7d".to_string();
    let mut until: Option<String> = None;
    let mut json_output = false;
    let mut events: Vec<PathBuf> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].clone();
        i += 1;
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        match name.as_str() {
            "--since" => match take_value(args, &mut i, inline) {
                Some(v) => since = v,
                None => return usage("stalls: --since needs a value"),
            },
            "--until" => match take_value(args, &mut i, inline) {
                Some(v) => until = Some(v),
                None => return usage("stalls: --until needs a value"),
            },
            "--events" => match take_value(args, &mut i, inline) {
                Some(v) => events.push(PathBuf::from(v)),
                None => return usage("stalls: --events needs a path"),
            },
            "--json" | "-J" => json_output = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            other => return usage(&format!("stalls: unknown flag {other:?}")),
        }
    }
    if events.is_empty() {
        return usage(
            "stalls: at least one --events path is required; \
the Python surface resolves the journal defaults",
        );
    }
    let now = chrono::Utc::now();
    let cutoff = match parse_since(&since) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };
    let top = until.as_deref().and_then(parse_loose_ts).unwrap_or(now);
    if top < cutoff {
        eprintln!("error: --until is before --since");
        return 1;
    }
    let (mut rows, malformed) =
        read_rows(&events, cutoff.timestamp_millis(), top.timestamp_millis());
    // Chronological order, untimestamped last: the NoProgress legacy join
    // takes the session's LATEST earlier loop_check row, and journals are
    // read project-then-global, so read order is not time order.
    rows.sort_by_key(|r| (r.ts.is_none(), r.ts));
    let fold = fold_rows(&rows);
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&table_json(&fold, &since, malformed))
                .expect("serializes")
        );
    } else {
        print_table(&fold, &since, malformed);
    }
    0
}

fn usage(msg: &str) -> i32 {
    if !msg.is_empty() {
        eprintln!("{msg}");
    }
    eprintln!("usage: {USAGE}");
    EXIT_USAGE
}

/// One `--flag value` / `--flag=value` token pair, advancing the cursor.
fn take_value(args: &[String], i: &mut usize, inline: Option<String>) -> Option<String> {
    if inline.is_some() {
        return inline;
    }
    let value = args.get(*i).cloned();
    if value.is_some() {
        *i += 1;
    }
    value
}

/// The CI evidence a NoProgress row carries: red on the row itself, red via
/// the legacy join, or no red at all.
enum CiEvidence {
    Red {
        check: Option<String>,
    },
    /// Red found through the legacy join, when the stop predates the
    /// structured fields.
    JoinedRed {
        check: Option<String>,
    },
    NoRed,
}

/// Read the row's own structured fields first (the post-fix shape); fall
/// back to the session's latest loop_check row at or before the stop for
/// the legacy rows that predate them.
fn no_progress_ci(
    data: &Value,
    ts: Option<chrono::DateTime<chrono::Utc>>,
    ci_history: &BTreeMap<&str, Vec<(Option<chrono::DateTime<chrono::Utc>>, &str)>>,
    session: Option<&str>,
) -> CiEvidence {
    let own = data.get("ci").and_then(|v| v.as_str());
    let ci = match own {
        Some(ci) => ci,
        None => {
            let Some(session) = session else {
                return CiEvidence::NoRed;
            };
            let Some(history) = ci_history.get(session) else {
                return CiEvidence::NoRed;
            };
            let prev = history
                .iter()
                .filter(|(row_ts, _)| ts.is_none() || row_ts.is_none() || row_ts <= &ts)
                .map(|(_, ci)| *ci)
                .last();
            let Some(ci) = prev else {
                return CiEvidence::NoRed;
            };
            return red_ci(ci, None).map_or(CiEvidence::NoRed, |check| CiEvidence::JoinedRed {
                check: Some(check),
            });
        }
    };
    let check = data
        .get("red_check")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    match red_ci(ci, check.as_deref()) {
        Some(check) => CiEvidence::Red { check: Some(check) },
        None => CiEvidence::NoRed,
    }
}

/// The failing check when a CI conclusion string reads red, else None.
/// `FAILURE:<name>` carries the check the way the loop_check rows spell it.
fn red_ci(ci: &str, check: Option<&str>) -> Option<String> {
    if let Some(name) = ci.strip_prefix("FAILURE:") {
        return Some(name.to_string());
    }
    if ci.starts_with("FAILURE") {
        return Some(
            check
                .map(str::to_string)
                .unwrap_or_else(|| "unknown".into()),
        );
    }
    None
}

/// The fold as one JSON payload: the fixed registry order, per-cause
/// detail splits, and the NoProgress totals beside their red split.
fn table_json(fold: &Fold, since: &str, malformed: usize) -> Value {
    json!({
        "since": since,
        "malformed_lines": malformed,
        "noprogress": {
            "total": fold.noprogress_total,
            "ci_red": fold.table["noprogress_ci_red"].rows,
            "joined_from_loop_check": fold.noprogress_joined,
        },
        "causes": CAUSES.iter().map(|cause| {
            let row = &fold.table[*cause];
            json!({
                "cause": cause,
                "rows": row.rows,
                "sessions": row.sessions.len(),
                "first_day": row.first_day,
                "last_day": row.last_day,
                "nodes": row.nodes,
                "detail": row.detail,
            })
        }).collect::<Vec<_>>(),
    })
}

/// The text table. Every registry row prints even at zero, so an absent
/// cause reads as a measured zero, never as an absent question. The node
/// column names the nodes the cause's own rows carry (up to two), not a
/// hard-coded fix-owner map: ownership the rows do not prove stays "-".
fn print_table(fold: &Fold, since: &str, malformed: usize) {
    println!("stalls: window starts {since}; {malformed} journal line(s) skipped");
    println!("cause                  rows  sessions  first       last        nodes      detail");
    for cause in CAUSES {
        let row = &fold.table[*cause];
        let detail = if row.detail.is_empty() {
            "-".to_string()
        } else {
            row.detail
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let nodes = if row.nodes.is_empty() {
            "-".to_string()
        } else {
            row.nodes
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        };
        println!(
            "{:<22} {:>4}  {:>8}  {:<10}  {:<10}  {:<9}  {}",
            cause,
            row.rows,
            row.sessions.len(),
            row.first_day.clone().unwrap_or_else(|| "-".into()),
            row.last_day.clone().unwrap_or_else(|| "-".into()),
            nodes,
            detail
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: &str, event_type: &str, session: Option<&str>, data: Value) -> Value {
        let mut payload = data.as_object().cloned().unwrap_or_default();
        if let Some(s) = session {
            payload.insert("session_id".into(), json!(s));
        }
        json!({"ts": ts, "type": event_type, "data": payload})
    }

    fn fold_of(values: &[Value]) -> Fold {
        let rows: Vec<Row> = values.iter().map(Row::from_event).collect();
        fold_rows(&rows)
    }

    #[test]
    fn stalls_cause_table_follows_its_contracts() {
        // Cause split with distinct sessions: one session re-firing 2
        // timeouts reads 2 rows / 1 session, the whole point of fix 4.
        let fold = fold_of(&[
            ev(
                "2026-09-20T10:00:00Z",
                "loop_check_gh_error",
                Some("s1"),
                json!({"read": "pr_view", "outcome": "timeout", "stderr_tail": "", "waited_on": "unknown"}),
            ),
            ev(
                "2026-09-20T11:00:00Z",
                "loop_check_gh_error",
                Some("s1"),
                json!({"read": "pr_view", "outcome": "timeout", "stderr_tail": ""}),
            ),
            ev(
                "2026-09-20T12:00:00Z",
                "loop_check_gh_error",
                Some("s2"),
                json!({"read": "pr_info_rest", "outcome": "failed", "stderr_tail": "gh: HTTP 403 secondary rate limit"}),
            ),
            ev(
                "2026-09-20T13:00:00Z",
                "loop_check_gh_error",
                Some("s3"),
                json!({"read": "pr_view", "outcome": "budget_refused"}),
            ),
            ev(
                "2026-09-20T14:00:00Z",
                "loop_check_gh_error",
                Some("s1"),
                json!({"read": "pr_view", "outcome": "failed", "stderr_tail": "exit 1"}),
            ),
        ]);
        assert_eq!(fold.table["gh_timeout"].rows, 2);
        assert_eq!(fold.table["gh_timeout"].sessions.len(), 1);
        assert_eq!(fold.table["gh_timeout"].detail.get("unknown"), Some(&2));
        assert_eq!(fold.table["gh_secondary_limit"].rows, 1);
        assert_eq!(fold.table["gh_budget_refusal"].rows, 1);
        // unattributed is its own row and never folds away.
        assert_eq!(fold.table["unattributed"].rows, 1);
        // NoProgress legacy join: red read from the previous loop_check row.
        let fold = fold_of(&[
            ev(
                "2026-09-20T10:00:00Z",
                "loop_check",
                Some("s1"),
                json!({"ci": "FAILURE:cargo test + schema parity"}),
            ),
            ev(
                "2026-09-20T10:05:00Z",
                "termination",
                Some("s1"),
                json!({"reason": "NoProgress", "message": "fingerprint unchanged"}),
            ),
        ]);
        assert_eq!(fold.table["noprogress_ci_red"].rows, 1);
        assert_eq!(
            fold.table["noprogress_ci_red"]
                .detail
                .get("cargo test + schema parity"),
            Some(&1)
        );
        assert_eq!(fold.noprogress_joined, 1);
        // Own fields beat the join: a NoProgress row carrying ci=SUCCESS
        // counts noprogress_other, never the join.
        let fold = fold_of(&[
            ev(
                "2020-01-01T00:00:00Z",
                "loop_check",
                Some("s1"),
                json!({"ci": "FAILURE:old"}),
            ),
            ev(
                "2026-09-20T10:05:00Z",
                "termination",
                Some("s1"),
                json!({"reason": "NoProgress", "ci": "SUCCESS"}),
            ),
        ]);
        assert_eq!(fold.table["noprogress_ci_red"].rows, 0);
        assert_eq!(fold.table["noprogress_other"].rows, 1);
        assert_eq!(fold.noprogress_joined, 0);
        // Sandbox rows route by their harness field.
        let fold = fold_of(&[ev(
            "2026-10-01T10:00:00Z",
            "sandbox_denial",
            Some("01a0"),
            json!({"harness": "codex", "class": "git_write", "hits": 3}),
        )]);
        assert_eq!(fold.table["codex_sandbox"].rows, 1);
        assert_eq!(
            fold.table["codex_sandbox"].detail.get("codex:git_write"),
            Some(&1)
        );
        // An undated row lands in the table without days.
        let fold = fold_of(&[ev(
            "not-a-timestamp",
            "loop_check_gh_error",
            Some("s1"),
            json!({"outcome": "timeout"}),
        )]);
        assert_eq!(fold.table["gh_timeout"].rows, 1);
        assert_eq!(fold.table["gh_timeout"].first_day, None);
    }
}
