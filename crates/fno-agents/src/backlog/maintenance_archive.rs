//! Native archive residency: the terminal-node sweep and its inverse.
//!
//! Terminal rows (done + superseded) age out of the live board: the sweep
//! stamps `archived_at` on them in the same store and every default read
//! filters stamped rows. Rows already stamped are never swept again.
//!
//! Never archived (an open node still points at them through a HARD edge):
//! a blocker in any open node's `blocked_by`, the parent of any open child,
//! and a `supersedes` / `superseded_by` target of an open node.
//!
//! SOFT edges (an open node's `related` peer, its `source_node_id` origin) do
//! not hold a terminal node: the sweep strips the reference on the open side
//! in the same write and the node leaves. Archived ids stay resolvable
//! through the archive-inclusive reads.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde_json::{json, Value};

use super::node_ref::find_node;
use super::settings;
use crate::graph_store;

/// The known skip reasons, in receipt order.
const SKIP_REASONS: [&str; 4] = [
    "referenced-by-open-node",
    "related-peer-not-archived",
    "too-recent",
    "no-parseable-timestamp",
];

/// The retro-triage trailer a landed postmortem carries (source_pr=None is
/// the postmortem shape). A literal match keeps the sweep free of a retro
/// dependency.
const POSTMORTEM_TRAILER: &str = "retro-triage source_pr=None";
const POSTMORTEM_MAX_AGE_DAYS: i64 = 30;
const DEFAULT_OLDER_THAN_DAYS: i64 = 30;

const ARCHIVE_HELP: &str = "Usage: fno backlog archive [OPTIONS]

  Sweep old terminal (done/superseded) nodes into archive residency: same
  store, but they stop answering default reads. Full contract:
  docs/architecture/backlog-graph-verb-contracts.md

Options:
  --apply                    Move the entries (default: dry-run, report only).
  --older-than-days INTEGER  Only archive terminal nodes older than N days.
                             [default: 30]
  --roadmap-id TEXT          Restrict the sweep to this roadmap group.
  -h, --help                 Show this message and exit.

Paired verb: `fno backlog unarchive <id>` moves one node back into the working
graph. Unlike `remove`, archiving keeps the node readable.";

const UNARCHIVE_HELP: &str = "Usage: fno backlog unarchive [OPTIONS] TASK_ID

  Clear one node's archive stamp: it answers default reads again. Full
  contract: docs/architecture/backlog-graph-verb-contracts.md

Arguments:
  TASK_ID  Feature ID (ab-XXXXXXXX)  [required]

Options:
  -h, --help  Show this message and exit.

Reverses `archive` for one node. Follow it with `fno backlog reopen <id>
--reason ...` if the node also needs to stop being done.";

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn is_archived(row: &Value) -> bool {
    truthy(row.get("archived_at"))
}

fn row_id(row: &Value) -> Option<&str> {
    row.get("id").and_then(Value::as_str)
}

/// Python `datetime.fromisoformat` over the shapes the store writes: a `Z`
/// or numeric offset, a naive stamp (read as UTC), or a bare date.
pub(crate) fn parse_ts(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let text = value.and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }
    let text = text.replace('Z', "+00:00");
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%:z"] {
        if let Ok(dt) = DateTime::parse_from_str(&text, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(&text, fmt) {
            return Some(dt.and_utc());
        }
    }
    NaiveDate::parse_from_str(&text, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc())
}

/// done -> completed_at; superseded -> updated; created_at last, so an oddly
/// shaped terminal row still has an age.
fn terminal_ts(row: &Value) -> Option<&Value> {
    ["completed_at", "updated", "created_at"]
        .iter()
        .map(|key| row.get(*key))
        .find(|v| truthy(*v))
        .flatten()
}

fn strings(value: Option<&Value>) -> impl Iterator<Item = &str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// Ids an OPEN row still references through a HARD edge. Soft edges are
/// absent on purpose: they are released on the open side instead of holding
/// the target, or finished work never leaves the working set.
fn guard_ids(rows: &[Value]) -> HashSet<String> {
    let mut guard = HashSet::new();
    for row in rows.iter().filter(|r| !graph_store::is_terminal_entry(r)) {
        guard.extend(strings(row.get("blocked_by")).map(String::from));
        guard.extend(strings(row.get("supersedes")).map(String::from));
        for key in ["parent", "superseded_by"] {
            if let Some(id) = row.get(key).and_then(Value::as_str) {
                guard.insert(id.to_string());
            }
        }
    }
    guard
}

/// The sweep's split over live rows: the indices to stamp, and the held-back
/// terminal indices with their reason.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Partition {
    pub to_archive: Vec<usize>,
    pub skipped: Vec<(usize, &'static str)>,
}

pub(crate) fn partition(rows: &[Value], older_than_days: i64, now: DateTime<Utc>) -> Partition {
    let cutoff = now.timestamp() as f64 - (older_than_days * 86_400) as f64;
    let guard = guard_ids(rows);
    let mut out = Partition::default();
    for (i, row) in rows.iter().enumerate() {
        if !graph_store::is_terminal_entry(row) {
            continue;
        }
        if row_id(row).is_some_and(|id| guard.contains(id)) {
            out.skipped.push((i, "referenced-by-open-node"));
            continue;
        }
        match parse_ts(terminal_ts(row)) {
            None => out.skipped.push((i, "no-parseable-timestamp")),
            Some(dt) if dt.timestamp_micros() as f64 / 1e6 >= cutoff => {
                out.skipped.push((i, "too-recent"))
            }
            Some(_) => out.to_archive.push(i),
        }
    }
    // A related pair of TERMINAL rows moves together: the older would sweep
    // while the newer stays naming an id the working graph no longer has.
    // An open peer holds nothing (its edge is soft and gets stripped).
    // Holding one back can strand the next along a chain, so iterate to a
    // fixed point; each pass moves at least one row out.
    loop {
        let leaving: HashSet<usize> = out.to_archive.iter().copied().collect();
        let terminal_staying: HashSet<&str> = rows
            .iter()
            .enumerate()
            .filter(|(i, r)| !leaving.contains(i) && graph_store::is_terminal_entry(r))
            .filter_map(|(_, r)| row_id(r))
            .collect();
        let (held, kept): (Vec<usize>, Vec<usize>) = out
            .to_archive
            .iter()
            .partition(|i| strings(rows[**i].get("related")).any(|r| terminal_staying.contains(r)));
        if held.is_empty() {
            break;
        }
        out.to_archive = kept;
        out.skipped
            .extend(held.into_iter().map(|i| (i, "related-peer-not-archived")));
    }
    out
}

/// Strip references to `arch_ids` from the staying rows' SOFT edges: the
/// archived id leaves every `related` list and nulls a `source_node_id`
/// naming it. Hard edges are untouched (the guard held their targets back).
/// Returns the number of references removed.
pub(crate) fn release_soft_edges(rows: &mut [Value], arch_ids: &HashSet<String>) -> usize {
    if arch_ids.is_empty() {
        return 0;
    }
    let mut stripped = 0;
    for row in rows.iter_mut() {
        if row_id(row).is_some_and(|id| arch_ids.contains(id)) {
            continue;
        }
        let Some(obj) = row.as_object_mut() else {
            continue;
        };
        if let Some(Value::Array(related)) = obj.get_mut("related") {
            let before = related.len();
            related.retain(|r| !r.as_str().is_some_and(|s| arch_ids.contains(s)));
            stripped += before - related.len();
        }
        if obj
            .get("source_node_id")
            .and_then(Value::as_str)
            .is_some_and(|s| arch_ids.contains(s))
        {
            obj.insert("source_node_id".into(), Value::Null);
            stripped += 1;
        }
    }
    stripped
}

/// Close postmortem receipts that aged out untriaged: an untouched `idea`
/// carrying the retro trailer, older than the age rule, closes as done with a
/// `retired` marker. Queued, claimed, and non-idea rows are never touched.
/// Returns how many rows it retired.
pub(crate) fn retire_stale_postmortems(rows: &mut [Value], now: DateTime<Utc>) -> usize {
    let cutoff = now.timestamp() - POSTMORTEM_MAX_AGE_DAYS * 86_400;
    let mut retired = 0;
    for row in rows.iter_mut() {
        if row.get("status").and_then(Value::as_str) != Some("idea") {
            continue;
        }
        if ["queued_at", "locked_by", "completed_at"]
            .iter()
            .any(|key| truthy(row.get(*key)))
        {
            continue;
        }
        let details = match row.get("details") {
            Some(Value::String(s)) => s.clone(),
            None | Some(Value::Null) => String::new(),
            Some(other) => other.to_string(),
        };
        if !details.contains(POSTMORTEM_TRAILER) {
            continue;
        }
        if parse_ts(row.get("created_at")).is_none_or(|dt| dt.timestamp() >= cutoff) {
            continue;
        }
        if let Some(obj) = row.as_object_mut() {
            obj.insert("status".into(), json!("done"));
            obj.insert(
                "completed_at".into(),
                json!(now.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()),
            );
            obj.insert("retired".into(), json!("stale-postmortem-receipt"));
            retired += 1;
        }
    }
    retired
}

/// Tally held rows by reason, zero-filled for every known one so a 0 reads
/// as "checked, none held". An unknown reason still lands, sorted after.
fn bucket_counts(reasons: &[&str]) -> Vec<(String, usize)> {
    let mut extra: BTreeMap<String, usize> = BTreeMap::new();
    let mut known = [0usize; SKIP_REASONS.len()];
    for reason in reasons {
        match SKIP_REASONS.iter().position(|k| k == reason) {
            Some(i) => known[i] += 1,
            None => *extra.entry(reason.to_string()).or_default() += 1,
        }
    }
    SKIP_REASONS
        .iter()
        .zip(known)
        .map(|(k, n)| (k.to_string(), n))
        .chain(extra)
        .collect()
}

fn held_count(held: &[(String, usize)], reason: &str) -> usize {
    held.iter()
        .find(|(k, _)| k == reason)
        .map_or(0, |(_, n)| *n)
}

/// Sweep freshness from the store's newest `archived_at` stamp. A read
/// failure answers unknown, never a clean "none".
fn last_sweep_line(rows: Result<Vec<Value>, String>, now: DateTime<Utc>) -> String {
    let Ok(rows) = rows else {
        return "unknown (archive unreadable)".to_string();
    };
    let archived: Vec<&Value> = rows.iter().filter(|r| is_archived(r)).collect();
    if archived.is_empty() {
        return "none on record".to_string();
    }
    let Some(newest) = archived
        .iter()
        .filter_map(|r| parse_ts(r.get("archived_at")))
        .max()
    else {
        return "none stamped".to_string();
    };
    let age = now - newest;
    let text = if age < chrono::Duration::hours(1) {
        "<1h ago".to_string()
    } else if age < chrono::Duration::hours(48) {
        format!("{}h ago", age.num_seconds() / 3600)
    } else {
        format!("{}d ago", age.num_seconds().div_euclid(86_400))
    };
    format!("{} ({text})", newest.format("%Y-%m-%dT%H:%M:%SZ"))
}

fn read_all(graph: &std::path::Path) -> Result<Vec<Value>, String> {
    graph_store::read_rows(graph).map_err(|e| e.to_string())
}

struct ArchiveArgs {
    apply: bool,
    older_than_days: i64,
    roadmap_id: Option<String>,
}

fn usage_error(verb: &str, message: &str) -> i32 {
    eprintln!("Usage: fno backlog {verb} [OPTIONS]\nTry 'fno backlog {verb} -h' for help.\n\nError: {message}");
    2
}

fn parse_archive(tail: &[String]) -> Result<Option<ArchiveArgs>, String> {
    let mut args = ArchiveArgs {
        apply: false,
        older_than_days: DEFAULT_OLDER_THAN_DAYS,
        roadmap_id: None,
    };
    let mut i = 0;
    while i < tail.len() {
        let token = tail[i].as_str();
        let (flag, inline) = match token.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (token, None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            i += 1;
            tail.get(i)
                .cloned()
                .ok_or_else(|| format!("Option '{name}' requires an argument."))
        };
        match flag {
            "-h" | "--help" => return Ok(None),
            "--apply" => args.apply = true,
            "--older-than-days" => {
                let raw = value("--older-than-days")?;
                args.older_than_days = raw.trim().parse().map_err(|_| {
                    format!(
                        "Invalid value for '--older-than-days': '{raw}' is not a valid integer."
                    )
                })?;
            }
            "--roadmap-id" => args.roadmap_id = Some(value("--roadmap-id")?),
            other if other.starts_with('-') => return Err(format!("No such option: {other}")),
            other => return Err(format!("Got unexpected extra argument ({other})")),
        }
        i += 1;
    }
    Ok(Some(args))
}

struct Sweep {
    retired: usize,
    moved: usize,
    held: Vec<(String, usize)>,
    stripped: usize,
}

/// The one sweep pass over the LIVE rows of `rows`: already-archived rows are
/// skipped in place, so row order and every archived row survive. `stamp`
/// false is the dry run.
fn sweep(
    rows: &mut [Value],
    older_than_days: i64,
    roadmap_id: Option<&str>,
    now: DateTime<Utc>,
    stamp: bool,
) -> Sweep {
    let live_pos: Vec<usize> = (0..rows.len())
        .filter(|i| !is_archived(&rows[*i]))
        .collect();
    let mut live: Vec<Value> = live_pos
        .iter()
        .map(|i| std::mem::take(&mut rows[*i]))
        .collect();
    let retired = retire_stale_postmortems(&mut live, now);
    // Guard against the FULL live graph so an open node in another roadmap
    // still protects its references; only the archive SET is restricted,
    // and the held counts describe what this run considered.
    let mut split = partition(&live, older_than_days, now);
    if let Some(roadmap) = roadmap_id {
        let in_roadmap =
            |i: &usize| live[*i].get("roadmap_id").and_then(Value::as_str) == Some(roadmap);
        split.to_archive.retain(in_roadmap);
        split.skipped.retain(|(i, _)| in_roadmap(i));
    }
    let reasons: Vec<&str> = split.skipped.iter().map(|(_, r)| *r).collect();
    let held = bucket_counts(&reasons);
    let moved = split.to_archive.len();
    let mut stripped = 0;
    if stamp && moved > 0 {
        let arch_ids: HashSet<String> = split
            .to_archive
            .iter()
            .filter_map(|i| row_id(&live[*i]).filter(|id| !id.is_empty()))
            .map(String::from)
            .collect();
        // Soft-edge release BEFORE the stamp: no live row keeps a pointer at
        // an archived id.
        stripped = release_soft_edges(&mut live, &arch_ids);
        let at = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        for i in &split.to_archive {
            if let Some(obj) = live[*i].as_object_mut() {
                obj.insert("archived_at".into(), json!(at));
            }
        }
    }
    for (row, pos) in live.into_iter().zip(live_pos) {
        rows[pos] = row;
    }
    Sweep {
        retired,
        moved,
        held,
        stripped,
    }
}

fn echo_receipt(held: &[(String, usize)], stripped: usize, last_sweep: &str) {
    for (reason, n) in held {
        println!("  held back ({reason}): {n}");
    }
    println!("  soft edges stripped from open nodes: {stripped}");
    println!("  last sweep: {last_sweep}");
}

/// Every run emits, dry-run included: a leg that went silent must stay
/// distinguishable from one that never ran. Best-effort by design.
fn emit_swept_event(sweep: &Sweep, mode: &str, older_than_days: i64) {
    let Some(state) = settings::state_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&state);
    let emitter = crate::events::EventEmitter::new(state.join("events.jsonl"), "backlog");
    let _ = emitter.emit(
        "graph_archive_swept",
        &json!({
            "moved": sweep.moved,
            "held_referenced": held_count(&sweep.held, "referenced-by-open-node"),
            "held_related": held_count(&sweep.held, "related-peer-not-archived"),
            "held_too_recent": held_count(&sweep.held, "too-recent"),
            "held_no_timestamp": held_count(&sweep.held, "no-parseable-timestamp"),
            "soft_edges_stripped": sweep.stripped,
            "mode": mode,
            "older_than_days": older_than_days,
        }),
    );
}

/// `fno backlog archive [--apply] [--older-than-days N] [--roadmap-id R]`.
pub fn run_archive(tail: &[String]) -> i32 {
    let args = match parse_archive(tail) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{ARCHIVE_HELP}");
            return 0;
        }
        Err(message) => return usage_error("archive", &message),
    };
    let graph = settings::graph_path();
    let now = Utc::now();
    let roadmap = args.roadmap_id.as_deref();

    if !args.apply {
        let mut rows = match read_all(&graph) {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("Error: the backlog graph could not be read: {err}");
                return 1;
            }
        };
        let last = last_sweep_line(Ok(rows.clone()), now);
        let result = sweep(&mut rows, args.older_than_days, roadmap, now, false);
        println!(
            "[dry-run] would archive {} terminal node(s) older than {}d into archive residency",
            result.moved, args.older_than_days
        );
        println!(
            "  would retire {} stale postmortem receipt(s)",
            result.retired
        );
        echo_receipt(&result.held, 0, &last);
        println!("Re-run with --apply to move them.");
        emit_swept_event(&result, "dry-run", args.older_than_days);
        return 0;
    }

    let mut outcome = Sweep {
        retired: 0,
        moved: 0,
        held: bucket_counts(&[]),
        stripped: 0,
    };
    let written = graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(30),
        None,
        None,
        |working| {
            outcome = sweep(working, args.older_than_days, roadmap, now, true);
            // Nothing retired and nothing moved: no write to publish.
            Ok(outcome.retired > 0 || outcome.moved > 0)
        },
    );
    if let Err(err) = written {
        eprintln!("Error: the archive sweep could not be written: {err}");
        return 1;
    }
    if outcome.moved > 0 {
        println!("Archived {} terminal node(s)", outcome.moved);
    } else {
        println!("No terminal nodes eligible to archive.");
    }
    if outcome.retired > 0 {
        println!(
            "Retired {} stale postmortem receipt(s) (closed by age rule)",
            outcome.retired
        );
    }
    echo_receipt(
        &outcome.held,
        outcome.stripped,
        &last_sweep_line(read_all(&graph), now),
    );
    emit_swept_event(&outcome, "apply", args.older_than_days);
    0
}

/// `fno backlog unarchive <id>`: clear one node's archive stamp.
pub fn run_unarchive(tail: &[String]) -> i32 {
    let mut task_id: Option<&str> = None;
    for token in tail {
        match token.as_str() {
            "-h" | "--help" => {
                println!("{UNARCHIVE_HELP}");
                return 0;
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return usage_error("unarchive", &format!("No such option: {other}"));
            }
            other if task_id.is_none() => task_id = Some(other),
            other => {
                return usage_error(
                    "unarchive",
                    &format!("Got unexpected extra argument ({other})"),
                )
            }
        }
    }
    let Some(task_id) = task_id else {
        return usage_error("unarchive", "Missing argument 'TASK_ID'.");
    };
    if !super::node_ref::has_node_id_prefix(task_id) {
        eprintln!("Error: task_id must be a <prefix>-<4..8 hex> node id, got '{task_id}'");
        return 1;
    }
    let graph = settings::graph_path();
    let rows = match read_all(&graph) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("Error: the backlog graph could not be read: {err}");
            return 1;
        }
    };
    let (archived, live): (Vec<Value>, Vec<Value>) = rows.into_iter().partition(is_archived);
    if find_node(&live, task_id).is_some() {
        eprintln!("warning: {task_id} is already in the working graph");
        return 0;
    }
    // Fuzzy-resolve like the working-graph lookup; a reminted archive entry
    // keeps its old id as previous_id.
    let row = find_node(&archived, task_id).or_else(|| {
        archived
            .iter()
            .find(|e| e.get("previous_id").and_then(Value::as_str) == Some(task_id))
    });
    let Some(row) = row else {
        eprintln!("Error: {task_id} is in neither the working graph nor the archive");
        return 1;
    };
    let resolved = row_id(row).unwrap_or(task_id).to_string();
    let store = super::api::Store::new(&graph);
    match super::api::node_unarchive(&store, &resolved) {
        Ok(payload) if payload.success => {
            println!("Unarchived {resolved}");
            0
        }
        Ok(_) => {
            eprintln!("Error: {resolved} could not be unarchived");
            1
        }
        Err(err) => {
            eprintln!("Error: {resolved} could not be unarchived: {}", err.0);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 8, 0, 0, 0).unwrap()
    }

    fn old(days: i64) -> String {
        (now() - chrono::Duration::days(days)).to_rfc3339()
    }

    fn ids(rows: &[Value], idx: &[usize]) -> Vec<String> {
        idx.iter()
            .map(|i| rows[*i]["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn hard_edges_hold_their_targets_and_soft_edges_do_not() {
        for edge in ["blocked_by", "parent", "supersedes"] {
            let open = match edge {
                "parent" => json!({"id": "x-open", "plan_path": "p", "parent": "x-hard"}),
                key => json!({"id": "x-open", "plan_path": "p", key: ["x-hard"]}),
            };
            let rows = vec![json!({"id": "x-hard", "completed_at": old(40)}), open];
            let split = partition(&rows, 30, now());
            assert!(split.to_archive.is_empty(), "{edge} must guard its target");
            assert_eq!(split.skipped, [(0, "referenced-by-open-node")]);
        }
        let rows = vec![
            json!({"id": "x-softrel", "completed_at": old(40)}),
            json!({"id": "x-softorg", "completed_at": old(40)}),
            json!({"id": "x-open", "plan_path": "p", "related": ["x-softrel"], "source_node_id": "x-softorg"}),
        ];
        let split = partition(&rows, 30, now());
        assert_eq!(ids(&rows, &split.to_archive), ["x-softrel", "x-softorg"]);
        assert!(split.skipped.is_empty());
    }

    #[test]
    fn a_terminal_related_pair_moves_together() {
        let rows = vec![
            json!({"id": "x-old", "completed_at": old(60), "related": ["x-new"]}),
            json!({"id": "x-new", "completed_at": old(5), "related": ["x-old"]}),
        ];
        let split = partition(&rows, 30, now());
        assert!(split.to_archive.is_empty());
        let reasons: Vec<&str> = split.skipped.iter().map(|(_, r)| *r).collect();
        assert_eq!(reasons, ["too-recent", "related-peer-not-archived"]);
        // Holding one back strands the next along a chain: b waits on c, a on b.
        let chain = vec![
            json!({"id": "x-a", "completed_at": old(400), "related": ["x-b"]}),
            json!({"id": "x-b", "completed_at": old(400), "related": ["x-a", "x-c"]}),
            json!({"id": "x-c", "completed_at": old(2), "related": ["x-b"]}),
        ];
        assert!(partition(&chain, 30, now()).to_archive.is_empty());
    }

    #[test]
    fn sweep_stamps_live_rows_only_and_restricts_by_roadmap() {
        let mut rows = vec![
            json!({"id": "x-dep", "roadmap_id": "rm-A", "completed_at": old(99)}),
            json!({"id": "x-open", "roadmap_id": "rm-B", "plan_path": "p", "blocked_by": ["x-dep"]}),
            json!({"id": "x-a", "roadmap_id": "rm-A", "completed_at": old(99), "related": []}),
            json!({"id": "x-b", "roadmap_id": "rm-B", "completed_at": old(1)}),
            json!({"id": "x-was", "completed_at": old(99), "archived_at": "2026-01-01T00:00:00Z"}),
        ];
        let result = sweep(&mut rows, 30, Some("rm-A"), now(), true);
        assert_eq!(result.moved, 1);
        assert_eq!(rows[2]["archived_at"], json!("2026-07-08T00:00:00Z"));
        assert!(
            rows[0].get("archived_at").is_none(),
            "an open rm-B node still blocks on it"
        );
        assert_eq!(
            rows[4]["archived_at"],
            json!("2026-01-01T00:00:00Z"),
            "never re-stamped"
        );
        assert_eq!(held_count(&result.held, "referenced-by-open-node"), 1);
        assert_eq!(
            held_count(&result.held, "too-recent"),
            0,
            "rm-B's hold is not this run's"
        );
    }

    #[test]
    fn stale_postmortem_receipts_retire_and_nothing_else_does() {
        let receipt = |id: &str, days: i64, extra: Value| {
            let mut row = json!({"id": id, "status": "idea", "created_at": old(days),
                "details": "<!-- retro-triage source_pr=None finding_hash=ab12 -->"});
            for (k, v) in extra.as_object().unwrap() {
                row[k] = v.clone();
            }
            row
        };
        let mut rows = vec![
            receipt("x-old", 40, json!({})),
            receipt("x-young", 5, json!({})),
            receipt("x-claimed", 40, json!({"locked_by": "s1"})),
            receipt("x-queued", 40, json!({"queued_at": old(1)})),
            receipt("x-deferred", 40, json!({"status": "deferred"})),
            receipt("x-plain", 40, json!({"details": "no trailer"})),
        ];
        assert_eq!(retire_stale_postmortems(&mut rows, now()), 1);
        assert_eq!(rows[0]["status"], json!("done"));
        assert_eq!(rows[0]["retired"], json!("stale-postmortem-receipt"));
        assert!(rows[1..].iter().all(|r| r.get("retired").is_none()));
    }
}
