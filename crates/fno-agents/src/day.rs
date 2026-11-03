//! Native morning and end-of-day readback over the existing project records.

use chrono::{DateTime, FixedOffset, SecondsFormat, TimeZone, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct DayInputs {
    pub kind: String,
    pub now: String,
    pub questions_raw: String,
    pub decisions_raw: String,
    pub decisions_path: String,
    pub graph_entries: Vec<Value>,
    pub graph_path: String,
    pub event_journals: Vec<(String, String)>,
    /// (path, state) pairs parallel to `event_journals`: `read`, `missing` or
    /// `unreadable`. Empty means every provided journal was read.
    pub journal_states: Vec<(String, String)>,
    /// Precomputed `lead_history::scan_scopes(paths, None)` payload, read by
    /// the IO layer. `fold_day` stays pure: it filters this Value to the
    /// window and never opens a store itself.
    pub checkin_scan: Option<Value>,
    /// `read`, `missing` or `unreadable` for the check-in scan itself.
    pub checkin_state: String,
    pub questions_state: String,
    pub questions_path: String,
}

fn parse_ts(raw: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(raw).ok()
}

fn format_ts(ts: DateTime<FixedOffset>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn stamp(v: &Value) -> Option<&str> {
    v.get("ts").and_then(Value::as_str)
}

fn id_of(item: &Value) -> Option<&str> {
    item.get("id").and_then(Value::as_str)
}

fn open_sort_key(item: &Value) -> (&str, &str) {
    (stamp(item).unwrap_or(""), id_of(item).unwrap_or(""))
}

fn data(v: &Value) -> Option<&serde_json::Map<String, Value>> {
    v.get("data").and_then(Value::as_object)
}

fn string(data: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).map(str::to_string)
}

fn local_midnight(now: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    now.offset()
        .from_local_datetime(&now.date_naive().and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap_or(now)
}

fn in_window(raw: &str, from: DateTime<FixedOffset>, to: DateTime<FixedOffset>) -> bool {
    parse_ts(raw).is_some_and(|ts| ts >= from && ts < to)
}

fn boundary_id(kind: &str, now: DateTime<FixedOffset>) -> String {
    // Stable per (kind, local day): a retry after a failed index append
    // mints the same id, so the journal leg can recognize its own earlier
    // row instead of adding a second one.
    let day = now.date_naive().format("%Y%m%d").to_string();
    let checksum = format!("{kind}{day}")
        .bytes()
        .fold(0u16, |sum, byte| sum.wrapping_add(byte as u16));
    format!(
        "day-{kind}-{}-{:04x}",
        now.date_naive().format("%Y%m%d"),
        checksum
    )
}

fn prior_boundaries(raw: &str) -> Vec<Value> {
    raw.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(|row| row.get("type").and_then(Value::as_str) == Some("day_boundary"))
        .collect()
}

fn latest_boundary(rows: &[Value]) -> Option<&Value> {
    rows.iter().max_by_key(|row| {
        stamp(row)
            .and_then(parse_ts)
            .map(|ts| ts.timestamp_millis())
            .unwrap_or(i64::MIN)
    })
}

fn question_items(raw: &str) -> Vec<Value> {
    crate::needs::fold(raw, "", 0, crate::needs::DEFAULT_FIRES_FLOOR)
        .into_iter()
        .filter(|item| item.kind == "operator_question")
        .map(|item| {
            json!({
                "id": item.session_id,
                "question": item.evidence,
                "ts": item.ts,
                "node": item.node,
            })
        })
        .collect()
}

fn attention(open: &[Value], history: &[Value]) -> Vec<String> {
    // Presentation history from prior boundaries: which ids were featured,
    // and when they were featured last (the boundary row's ts).
    let mut last_featured: HashMap<&str, &str> = HashMap::new();
    for row in history {
        if let Some(ids) = row
            .get("data")
            .and_then(|d| d.get("featured"))
            .and_then(Value::as_array)
        {
            let row_ts = stamp(row).unwrap_or("");
            for id in ids.iter().filter_map(Value::as_str) {
                let newer = last_featured.get(id).is_none_or(|prior| row_ts >= *prior);
                if newer {
                    last_featured.insert(id, row_ts);
                }
            }
        }
    }
    // Slots 1-3: the shared rank's own order (needs order, newest first).
    let mut selected: Vec<String> = open
        .iter()
        .take(3)
        .filter_map(id_of)
        .map(str::to_string)
        .collect();
    // Reserved places: never-featured first, oldest ts leading, so old asks
    // cannot sit unseen forever; then the least recently featured. Ties on id.
    let mut never: Vec<&Value> = open
        .iter()
        .filter(|item| id_of(item).is_some_and(|id| !last_featured.contains_key(id)))
        .collect();
    never.sort_by(|a, b| open_sort_key(a).cmp(&open_sort_key(b)));
    let mut carried: Vec<&Value> = open
        .iter()
        .filter(|item| id_of(item).is_some_and(|id| last_featured.contains_key(id)))
        .collect();
    carried.sort_by(|a, b| {
        let ia = id_of(a).unwrap_or("");
        let ib = id_of(b).unwrap_or("");
        let ta = last_featured.get(ia).copied().unwrap_or("");
        let tb = last_featured.get(ib).copied().unwrap_or("");
        (ta, ia).cmp(&(tb, ib))
    });
    for item in never.into_iter().chain(carried) {
        if selected.len() >= 5 {
            break;
        }
        let Some(id) = id_of(item) else {
            continue;
        };
        if !selected.iter().any(|selected_id| selected_id == id) {
            selected.push(id.to_string());
        }
    }
    // Any leftover place comes from the shared rank order.
    for item in open {
        if selected.len() >= 5 {
            break;
        }
        if let Some(id) = id_of(item) {
            if !selected.iter().any(|selected_id| selected_id == id) {
                selected.push(id.to_string());
            }
        }
    }
    selected
}

fn collect_retractions(raw: &str, kind: &str, out: &mut Vec<Value>) {
    for line in raw.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(row_kind) = row.get("type").and_then(Value::as_str) else {
            continue;
        };
        let is_retraction = row_kind == "decision_retracted"
            || (row_kind == "review_attestation"
                && data(&row).is_some_and(|d| d.contains_key("retracts_attester")));
        if !is_retraction {
            continue;
        }
        let Some(ts) = stamp(&row) else { continue };
        let Some(fields) = data(&row) else { continue };
        let mut item = json!({
            "kind": kind,
            "target": string(fields, "target_decision_id")
                .or_else(|| string(fields, "target"))
                .or_else(|| string(fields, "decision_id"))
                .unwrap_or_default(),
            "reason": string(fields, "reason").unwrap_or_default(),
            "ts": ts,
        });
        for key in ["authority_source", "retracted_by", "quote"] {
            if let Some(value) = string(fields, key) {
                item[key] = json!(value);
            }
        }
        out.push(item);
    }
}

/// Filter the precomputed `lead_history::scan_scopes(paths, None)` payload to
/// the window and group its canonical rows by scope. The canonical/legacy
/// classification stays in lead_history: this reads its verdict, never a
/// second classifier over raw journal text.
fn checkin_summary(
    scan: Option<&Value>,
    state: &str,
    from: DateTime<FixedOffset>,
    to: DateTime<FixedOffset>,
) -> Value {
    let mut scopes: HashMap<String, (u64, String)> = HashMap::new();
    let events = scan
        .and_then(|scan| scan.get("events"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for event in events {
        let Some(ts) = stamp(&event).or_else(|| event.get("ts").and_then(Value::as_str)) else {
            continue;
        };
        if !in_window(ts, from, to) {
            continue;
        }
        let scope = event
            .get("data")
            .and_then(|d| d.get("scope"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let change = event
            .get("data")
            .and_then(|d| d.get("change"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let entry = scopes.entry(scope).or_insert((0, String::new()));
        entry.0 += 1;
        if parse_ts(ts)
            .is_some_and(|current| parse_ts(&entry.1).is_none_or(|previous| current >= previous))
        {
            entry.1 = change;
        }
    }
    let rejected = scan
        .and_then(|scan| scan.get("rejected"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut scope_rows: Vec<(String, (u64, String))> = scopes.into_iter().collect();
    // HashMap order is randomized per process; a deterministic JSON payload
    // needs the scopes sorted (the needs fold fixes the same disease).
    scope_rows.sort_by(|a, b| a.0.cmp(&b.0));
    json!({
        "state": state,
        "rejected": rejected,
        "scopes": scope_rows.into_iter().map(|(scope, (count, latest_change))| {
            json!({"scope": scope, "count": count, "latest_change": latest_change})
        }).collect::<Vec<_>>(),
    })
}

pub fn fold_day(inputs: &DayInputs) -> Result<Value, String> {
    let now =
        parse_ts(&inputs.now).ok_or_else(|| format!("invalid --now RFC3339: {}", inputs.now))?;
    if inputs.kind != "start" && inputs.kind != "end" {
        return Err("--kind must be start or end".to_string());
    }
    let history = prior_boundaries(&inputs.questions_raw);
    let newest = latest_boundary(&history);
    if let Some(saved) = newest {
        let saved_kind = data(saved)
            .and_then(|d| d.get("kind"))
            .and_then(Value::as_str);
        let saved_date = stamp(saved)
            .and_then(parse_ts)
            .map(|ts| ts.with_timezone(now.offset()).date_naive());
        if saved_kind == Some(inputs.kind.as_str()) && saved_date == Some(now.date_naive()) {
            let mut reused = data(saved).cloned().unwrap_or_default();
            reused.insert("reused".into(), json!(true));
            return Ok(Value::Object(reused));
        }
    }
    let from = newest
        .and_then(|row| {
            data(row)
                .and_then(|d| d.get("cutoff"))
                .and_then(Value::as_str)
        })
        .and_then(parse_ts)
        .unwrap_or_else(|| local_midnight(now));
    let label = if newest.is_some() {
        "since previous boundary"
    } else {
        "first boundary"
    };
    let projection = crate::feed::project(
        &inputs.questions_raw,
        &inputs.graph_entries,
        &[],
        "",
        "",
        "",
        "",
        None,
    );
    // node_ended rows carry no PR reference; the graph entry does. Join the
    // completion card to its PR through the entry the feed already walked.
    let mut pr_by_node: HashMap<&str, Option<i64>> = HashMap::new();
    for entry in &inputs.graph_entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        pr_by_node.insert(id, entry.get("pr_number").and_then(Value::as_i64));
    }
    let mut completed_items = Vec::new();
    let mut opened = 0u64;
    let mut closed = 0u64;
    for row in projection
        .rows
        .iter()
        .filter(|row| in_window(&row.ts, from, now))
    {
        match row.kind.as_str() {
            "node_ended" => completed_items.push(
                json!({
                    "node": row.node,
                    "title": row.title,
                    "ref": row.r#ref,
                    "pr_number": row.node.as_deref().and_then(|node| pr_by_node.get(node)).cloned().flatten(),
                    "ts": row.ts
                }),
            ),
            "question_asked" => opened += 1,
            "question_closed" => closed += 1,
            _ => {}
        }
    }
    let open_items = question_items(&inputs.questions_raw);
    let open_count = if inputs.questions_state == "read" {
        json!(open_items.len())
    } else {
        json!("unknown")
    };
    let featured = attention(&open_items, &history);
    let first_ask = featured
        .first()
        .and_then(|id| {
            open_items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()))
        })
        .and_then(|item| item.get("question").and_then(Value::as_str))
        .map(|ask| {
            if ask.chars().count() > 80 {
                ask.chars().take(80).collect::<String>()
            } else {
                ask.to_string()
            }
        })
        .unwrap_or_default();
    let mut retractions = Vec::new();
    collect_retractions(&inputs.decisions_raw, "decision", &mut retractions);
    for (_path, raw) in &inputs.event_journals {
        collect_retractions(raw, "review", &mut retractions);
    }
    retractions.retain(|row| {
        row.get("ts")
            .and_then(Value::as_str)
            .is_some_and(|ts| in_window(ts, from, now))
    });
    let mut receipts = vec![
        json!({"source":"questions", "path": inputs.questions_path, "state": inputs.questions_state, "scanned": inputs.questions_raw.lines().count(), "matched": open_items.len()}),
        json!({"source":"decisions", "path": inputs.decisions_path, "state":"read", "scanned": inputs.decisions_raw.lines().count(), "matched": retractions.iter().filter(|r| r["kind"] == "decision").count()}),
        json!({"source":"graph", "path": inputs.graph_path, "state":"read", "scanned": inputs.graph_entries.len(), "matched": completed_items.len()}),
    ];
    for (path, raw) in &inputs.event_journals {
        let state = inputs
            .journal_states
            .iter()
            .find(|(state_path, _)| state_path == path)
            .map(|(_, state)| state.as_str())
            .unwrap_or("read");
        // The journal's role in this fold is review-attestation retractions,
        // so its matched count is those rows, not check-ins (those carry
        // their own receipt under `checkins`).
        receipts.push(json!({"source":"journal", "path":path, "state":state, "scanned":raw.lines().count(), "matched":raw.lines().filter(|line| line.contains("retracts_attester")).count()}));
    }
    let id = boundary_id(&inputs.kind, now);
    Ok(json!({
        "boundary_id": id,
        "kind": inputs.kind,
        "reused": false,
        "cutoff": format_ts(now),
        "window": {"from": format_ts(from), "to": format_ts(now), "label": label},
        "prior_boundary_id": newest.and_then(|row| data(row).and_then(|d| d.get("boundary_id")).and_then(Value::as_str)),
        "completed": {"count": completed_items.len(), "items": completed_items.into_iter().take(10).collect::<Vec<_>>()},
        "questions": {"open": open_count, "opened": opened, "closed": closed, "featured": featured, "first_ask": first_ask},
        "questions_state": inputs.questions_state,
        "questions_path": inputs.questions_path,
        "questions_rows": inputs.questions_raw.lines().count() as u64,
        "retractions": retractions,
        "checkins": checkin_summary(inputs.checkin_scan.as_ref(), &inputs.checkin_state, from, now),
        "receipts": receipts,
    }))
}

fn render(payload: &Value) -> String {
    let questions = &payload["questions"];
    let questions_state = payload["questions_state"].as_str().unwrap_or("read");
    let questions_path = payload["questions_path"]
        .as_str()
        .unwrap_or("questions.jsonl");
    let action = questions["featured"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(Value::as_str);
    let first = if questions_state == "missing" {
        format!("open questions: unknown (questions store missing: {questions_path})")
    } else {
        match (payload["kind"].as_str(), action) {
            (Some("start"), Some(id)) => {
                let ask = questions["first_ask"].as_str().unwrap_or("");
                let ask = if ask.is_empty() {
                    String::new()
                } else {
                    format!(": {ask}")
                };
                format!("Start: answer {}{ask}", crate::numbers::display(id))
            }
            (Some("end"), Some(id)) => {
                let ask = questions["first_ask"].as_str().unwrap_or("");
                let ask = if ask.is_empty() {
                    String::new()
                } else {
                    format!(": {ask}")
                };
                format!("Tomorrow: {id}{ask}")
            }
            _ => format!(
                "Nothing waits on you: {} open questions ({} {}, {} rows)",
                questions["open"],
                questions_path,
                questions_state,
                payload["questions_rows"].as_u64().unwrap_or(0)
            ),
        }
    };
    let mut lines = vec![first];
    lines.push(format!("Completed: {}", payload["completed"]["count"]));
    lines.push(format!(
        "Questions: +{} / -{}; showing {} of {} open",
        questions["opened"],
        questions["closed"],
        questions["featured"].as_array().map_or(0, Vec::len),
        questions["open"]
    ));
    lines.push(format!(
        "Retractions: {}",
        payload["retractions"].as_array().map_or(0, Vec::len)
    ));
    let checkins = &payload["checkins"];
    let scope_parts: Vec<String> = checkins["scopes"]
        .as_array()
        .map(|scopes| {
            scopes
                .iter()
                .map(|scope| {
                    format!(
                        "{} {} (latest: {})",
                        scope["scope"], scope["count"], scope["latest_change"]
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    lines.push(format!(
        "Check-ins: {} (rejected {})",
        if scope_parts.is_empty() {
            "none in window".to_string()
        } else {
            scope_parts.join(", ")
        },
        checkins["rejected"]
    ));
    lines.push(format!(
        "Window: {} to {} ({})",
        payload["window"]["from"], payload["window"]["to"], payload["window"]["label"]
    ));
    lines.push(format!(
        "Sources: {}",
        payload["receipts"].as_array().map_or(0, Vec::len)
    ));
    lines.join("\n")
}

fn graph_path(home: &crate::paths::AgentsHome) -> PathBuf {
    let root = home
        .root()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    crate::state_layout::graph_anchor(&root)
}

/// The journals the deleted Python relay expanded from
/// `fno.paths.event_journals()`: the global journal (state root), the agents
/// home journal, and the project journal, each contributing its ephemeral
/// sibling and its numeric rotations when they exist, resolved and deduped,
/// the project journal first. The commit arm takes this order as-is: [0] is
/// the live project journal.
pub(crate) fn default_event_journals(home: &crate::paths::AgentsHome) -> Vec<PathBuf> {
    fn expand(journal: &Path) -> Vec<PathBuf> {
        let live = crate::event_store::live_journal(journal);
        let ephemeral = live.with_extension("jsonl.ephemeral");
        let mut bases: Vec<PathBuf> = vec![live.clone()];
        if ephemeral.exists() {
            bases.push(ephemeral);
        }
        let mut candidates: Vec<PathBuf> = Vec::new();
        for base in bases {
            let parent = base.parent().unwrap_or_else(|| Path::new("."));
            let prefix = format!(
                "{}.",
                base.file_name().unwrap_or_default().to_string_lossy()
            );
            let mut rotated: Vec<(u32, PathBuf)> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let Some(suffix) = name.strip_prefix(&prefix) else {
                        continue;
                    };
                    let Ok(n) = suffix.parse::<u32>() else {
                        continue;
                    };
                    if entry.path().exists() {
                        rotated.push((n, entry.path()));
                    }
                }
            }
            rotated.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
            rotated.dedup_by_key(|(n, _)| *n);
            for (_, path) in rotated {
                candidates.push(crate::paths::resolve_loose(&path));
            }
            candidates.push(crate::paths::resolve_loose(&base));
        }
        let mut out: Vec<PathBuf> = Vec::new();
        for path in candidates {
            if !out.contains(&path) {
                out.push(path);
            }
        }
        out
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let global = if let Some(dir) = std::env::var_os("FNO_STATE_DIR") {
        PathBuf::from(dir).join("events.jsonl")
    } else {
        crate::decision_index::default_state_path("events.jsonl")
    };
    let mut journals = Vec::new();
    journals.extend(expand(&global));
    journals.extend(expand(&home.events_jsonl()));
    journals.extend(expand(&crate::paths::events_path(&cwd)));
    journals
}

pub fn run_day(rest: &[String], home: &crate::paths::AgentsHome) -> i32 {
    let mut kind = None;
    let mut now = Utc::now().to_rfc3339();
    let mut json_output = false;
    let mut event_paths = Vec::new();
    let mut commit = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--kind" if i + 1 < rest.len() => {
                kind = Some(rest[i + 1].clone());
                i += 2;
            }
            "--now" if i + 1 < rest.len() => {
                now = rest[i + 1].clone();
                i += 2;
            }
            "--events-path" if i + 1 < rest.len() => {
                event_paths.push(PathBuf::from(&rest[i + 1]));
                i += 2;
            }
            "--json" | "-J" => {
                json_output = true;
                i += 1;
            }
            "--commit" => {
                commit = true;
                i += 1;
            }
            other => {
                eprintln!("fno-agents day: unknown flag {other}");
                return 2;
            }
        }
    }
    let Some(kind) = kind else {
        eprintln!("fno-agents day: --kind start|end is required");
        return 2;
    };
    // The deleted Python relay expanded the journals; with no caller-supplied
    // --events-path the native leg resolves the same stores itself.
    if event_paths.is_empty() {
        event_paths = default_event_journals(home);
    }
    let state_dir = home
        .root()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let questions_path = state_dir.join("questions.jsonl");
    let questions_exist =
        questions_path.exists() || crate::event_store::store_path(&questions_path).exists();
    let (questions_raw, questions_state) = if !questions_exist {
        (String::new(), "missing".to_string())
    } else {
        match crate::event_store::journal_text_checked(
            &questions_path,
            &crate::event_store::EventQuery::of_types(&[]),
        ) {
            Ok(raw) => (raw, "read".to_string()),
            Err(error) => {
                eprintln!(
                    "fno-agents day: questions store unreadable: {}: {error}",
                    questions_path.display()
                );
                return 1;
            }
        }
    };
    let decisions_path = state_dir.join("decisions.jsonl");
    let decisions_raw = crate::event_store::journal_text(&decisions_path, &[]);
    let graph_entries = crate::graph_store::read_rows_where(
        &graph_path(home),
        &crate::backlog::RowQuery {
            fields: Some(
                ["id", "pr_number"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ),
            with_blockers: true,
            ..Default::default()
        },
    )
    .unwrap_or_default();
    let mut journals: Vec<(String, String)> = Vec::new();
    let mut journal_states: Vec<(String, String)> = Vec::new();
    for path in &event_paths {
        let exists = path.exists() || crate::event_store::store_path(path).exists();
        let raw = match crate::event_store::journal_text_checked(
            path,
            &crate::event_store::EventQuery::of_types(&["review_attestation"]),
        ) {
            Ok(raw) if exists => raw,
            Ok(_) => {
                journal_states.push((path.display().to_string(), "missing".to_string()));
                String::new()
            }
            Err(_error) => {
                // A journal that cannot be read is an incomplete source, not
                // a reason to lose the whole read; the receipt names it.
                journal_states.push((path.display().to_string(), "unreadable".to_string()));
                String::new()
            }
        };
        journals.push((path.display().to_string(), raw));
    }
    let (checkin_scan, checkin_state) = if event_paths.is_empty() {
        (None, "missing".to_string())
    } else {
        match crate::lead_history::scan_scopes(&event_paths, None) {
            Ok(scan) => (Some(scan), "read".to_string()),
            Err(_) => (None, "unreadable".to_string()),
        }
    };
    let input = DayInputs {
        kind,
        now,
        questions_raw,
        decisions_raw,
        decisions_path: decisions_path.display().to_string(),
        graph_entries,
        graph_path: crate::backlog::database_path(&graph_path(home))
            .display()
            .to_string(),
        event_journals: journals,
        journal_states,
        checkin_scan,
        checkin_state,
        questions_state,
        questions_path: questions_path.display().to_string(),
    };
    match fold_day(&input) {
        Ok(payload) => {
            if commit && payload["reused"] != json!(true) {
                // The project journal is the first journal the caller named;
                // the Python adapter passes `fno.paths.event_journals()` in
                // order, so [0] is the live project journal.
                let journal = match event_paths.first() {
                    Some(path) => path.as_path(),
                    None => {
                        eprintln!(
                            "fno-agents day: --commit needs --events-path for the project journal"
                        );
                        return 2;
                    }
                };
                if let Err(error) = commit_boundary(&payload, journal, &questions_path) {
                    eprintln!("fno-agents day: {error}");
                    return 1;
                }
            }
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            } else {
                println!("{}", render(&payload));
            }
            0
        }
        Err(error) => {
            eprintln!("fno-agents day: {error}");
            2
        }
    }
}

/// One locked O_APPEND write, mirroring the Python journal writer's lock
/// convention: a `<file>.lock.d` mkdir mutex, an owner file, one atomic append.
pub(crate) fn append_row(path: &Path, line: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("events.jsonl"); // events-discipline:allow: a day-ledger fallback name, not the state-root journal
    let lock_dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{name}.lock.d"));
    let token =
        crate::claims::acquire_dir_mutex(&lock_dir, std::time::Duration::from_secs(30), true)
            .ok_or_else(|| format!("lock timeout: {}", lock_dir.display()))?;
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|e| e.to_string());
    crate::claims::release_dir_mutex(&lock_dir, &token);
    result
}

/// The boundary row's fields: ids and counts only, never titles. Full
/// evidence stays in its owning sources. Sized against the validated limit
/// so a refusal names the boundary instead of substituting a meta-event.
fn boundary_fields(payload: &Value) -> Result<serde_json::Map<String, Value>, String> {
    let questions = payload.get("questions").cloned().unwrap_or(json!({}));
    let mut fields = serde_json::Map::new();
    fields.insert("boundary_id".into(), payload["boundary_id"].clone());
    fields.insert("kind".into(), payload["kind"].clone());
    fields.insert("cutoff".into(), payload["cutoff"].clone());
    if let Some(prior) = payload.get("prior_boundary_id").filter(|v| !v.is_null()) {
        fields.insert("prior_boundary_id".into(), prior.clone());
    }
    if let Some(featured) = questions
        .get("featured")
        .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    {
        fields.insert("featured".into(), featured.clone());
    }
    if let Some(count) = payload.pointer("/completed/count").and_then(Value::as_u64) {
        fields.insert("completed".into(), json!(count));
    }
    for key in ["open", "opened", "closed"] {
        if let Some(count) = questions.get(key).and_then(Value::as_u64) {
            fields.insert(key.into(), json!(count));
        }
    }
    let retraction_count = payload
        .get("retractions")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    fields.insert("retractions".into(), json!(retraction_count));
    // An overflow substitute is not a committed boundary: size the row
    // against the validated limit and refuse rather than write the
    // event_payload_too_large meta-event in its place.
    let encoded = serde_json::to_string(&fields).map_err(|e| e.to_string())?;
    let limit = crate::events_limits::max_data_bytes();
    if encoded.len() > limit {
        return Err(format!(
            "boundary provenance is {} bytes, over the validated limit of {limit}; refusing the boundary instead of substituting",
            encoded.len()
        ));
    }
    Ok(fields)
}

/// The durable boundary row: [`boundary_fields`] wrapped in the envelope.
fn boundary_event_line(payload: &Value) -> Result<String, String> {
    let fields = boundary_fields(payload)?;
    let event = json!({
        "ts": crate::events::now_rfc3339(),
        "type": "day_boundary",
        "source": "target",
        "data": fields,
    });
    let mut line = serde_json::to_string(&event).map_err(|e| e.to_string())?;
    line.push('\n');
    Ok(line)
}

/// Commit the fold's boundary row: project journal first, then the
/// machine-wide question index, mirroring the decide retraction order. A
/// question-index failure is reported with the boundary id: the row is in
/// the journal, so recovery names what landed and what did not. The id is
/// stable per (kind, local day), so a retry after that failure recognizes
/// its own earlier journal row and only retries the index leg.
fn commit_boundary(
    payload: &Value,
    journal_path: &Path,
    questions_path: &Path,
) -> Result<(), String> {
    let line = boundary_event_line(payload)?;
    let id = payload["boundary_id"].as_str().unwrap_or_default();
    let journal_has_it = crate::event_store::journal_text(journal_path, &["day_boundary"])
        .lines()
        .any(|row| {
            serde_json::from_str::<Value>(row.trim())
                .ok()
                .and_then(|v| {
                    v.get("data")
                        .and_then(|d| d.get("boundary_id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .is_some_and(|row_id| row_id == id)
        });
    if !journal_has_it {
        append_row(journal_path, &line)
            .map_err(|e| format!("project journal append failed: {e}"))?;
    }
    // The routed question index takes the row through the store: a raw
    // append on a routed stem strands the boundary from every committed
    // read. The fields are the same ones the line wraps.
    let data = boundary_fields(payload).map_err(|e| {
        format!(
            "boundary {} landed in the project journal but not the question index: {e}",
            payload["boundary_id"]
        )
    })?;
    crate::events::EventEmitter::new(questions_path, "target")
        .emit_fields("day_boundary", data)
        .map_err(|e| {
            format!(
                "boundary {} landed in the project journal but not the question index: {e}",
                payload["boundary_id"]
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inputs(kind: &str, now: &str, questions: &str) -> DayInputs {
        DayInputs {
            kind: kind.to_string(),
            now: now.to_string(),
            questions_raw: questions.to_string(),
            questions_state: "read".to_string(),
            questions_path: "questions.jsonl".to_string(),
            ..DayInputs::default()
        }
    }

    #[test]
    fn prior_boundary_limits_completed_work_to_the_interval() {
        let mut input = inputs(
            "start",
            "2026-09-10T18:00:00Z",
            r#"{"ts":"2026-09-10T08:00:00Z","type":"day_boundary","source":"operator","data":{"kind":"end","boundary_id":"day-end-old","cutoff":"2026-09-10T08:00:00Z"}}"#,
        );
        input.graph_entries = vec![
            json!({"id":"x-in","title":"inside","completed_at":"2026-09-10T09:00:00Z"}),
            json!({"id":"x-out","title":"outside","completed_at":"2026-09-10T07:00:00Z"}),
        ];
        input.decisions_raw = r#"{"ts":"2026-09-10T10:00:00Z","type":"decision_retracted","data":{"target_decision_id":"d-x","reason":"r","authority_source":"chat_attested","retracted_by":"testf4c6","quote":"drop it"}}"#.to_string();
        let payload = fold_day(&input).unwrap();
        assert_eq!(payload["completed"]["count"], json!(1));
        assert_eq!(payload["window"]["from"], json!("2026-09-10T08:00:00Z"));
        assert_eq!(payload["prior_boundary_id"], json!("day-end-old"));
        let retraction = &payload["retractions"][0];
        assert_eq!(retraction["target"], json!("d-x"));
        assert_eq!(retraction["quote"], json!("drop it"));
        assert_eq!(retraction["authority_source"], json!("chat_attested"));
    }

    #[test]
    fn same_kind_on_same_local_day_reuses_the_saved_boundary() {
        let input = inputs(
            "start",
            "2026-09-10T12:00:00Z",
            r#"{"ts":"2026-09-10T08:00:00Z","type":"day_boundary","source":"operator","data":{"kind":"start","boundary_id":"day-start-saved","cutoff":"2026-09-10T08:00:00Z","featured":["q-1"]}}"#,
        );
        let payload = fold_day(&input).unwrap();
        assert_eq!(payload["reused"], json!(true));
        assert_eq!(payload["boundary_id"], json!("day-start-saved"));
    }

    #[test]
    fn render_names_the_first_action_within_twelve_lines() {
        let input = inputs(
            "start",
            "2026-09-10T12:00:00Z",
            r#"{"ts":"2026-09-10T08:00:00Z","type":"operator_question","source":"target","data":{"question_id":"q-1","question":"pick the runner"}}"#,
        );
        let payload = fold_day(&input).unwrap();
        let text = render(&payload);
        let first = text.lines().next().unwrap();
        assert!(first.starts_with("Start: answer q-1"), "{first}");
        assert!(first.contains("pick the runner"), "{first}");
        assert!(text.lines().count() <= 12, "{text}");
    }

    #[test]
    fn boundary_event_line_names_required_keys_and_counts_only() {
        let payload = json!({
            "boundary_id": "day-end-20260920-ab12",
            "kind": "end",
            "reused": false,
            "cutoff": "2026-09-20T18:05:00+00:00",
            "prior_boundary_id": null,
            "completed": {"count": 3, "items": []},
            "questions": {"open": 5, "opened": 2, "closed": 1, "featured": ["q-1"], "first_ask": "why"},
            "retractions": [],
            "window": {"from": "2026-09-20T08:00:00+00:00", "to": "2026-09-20T18:05:00+00:00", "label": "since previous boundary"}
        });
        let line = boundary_event_line(&payload).unwrap();
        let event: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(event["type"], json!("day_boundary"));
        assert_eq!(event["source"], json!("target"));
        let data = &event["data"];
        assert_eq!(data["boundary_id"], json!("day-end-20260920-ab12"));
        assert_eq!(data["kind"], json!("end"));
        assert_eq!(data["featured"].as_array().unwrap().len(), 1);
        assert!(data.get("title").is_none(), "the row never carries titles");
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn fold_refuses_a_kind_outside_start_and_end() {
        let input = inputs("noon", "2026-09-10T12:00:00Z", "");
        let error = fold_day(&input).unwrap_err();
        assert!(error.contains("--kind must be start or end"), "{error}");
    }

    #[test]
    fn commit_boundary_skips_a_store_committed_boundary() {
        // AC10-DAY: a day_boundary committed to the store only is not re-appended.
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("events.jsonl");
        let questions = tmp.path().join("questions.jsonl");
        let payload = serde_json::json!({
            "boundary_id": "day-start-20260910",
            "kind": "start",
            "day": "2026-09-10",
        });
        let line = boundary_event_line(&payload).unwrap();
        crate::event_store::append_envelope(&journal, &line, None).unwrap();
        commit_boundary(&payload, &journal, &questions).unwrap();
        let rows = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&["day_boundary"]),
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "the store-only boundary is recognized");
        let raw = std::fs::read_to_string(&journal).unwrap_or_default();
        assert!(raw.is_empty(), "no second append for the same id");
    }
}
