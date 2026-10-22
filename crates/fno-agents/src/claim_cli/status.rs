//! `claim status` leaf, ported from `cli/src/fno/claims/cli.py::status`.
//! The goldens frozen in `tests/claim_status_parity.rs` are the contract:
//! payload field order, verdict-line wording, and the typer parse-error
//! layout. On a `node:` key nobody holds, one fleet roster read is shared
//! with the worked-overlay join; stdout stays parseable JSON on every path
//! and all prose rides stderr.

use crate::backlog::worked::{self, FleetRow};
use serde_json::{json, Map, Value};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub fn run(args: &[String]) -> i32 {
    let mut json_output = false;
    let mut roster = true;
    let mut root: Option<PathBuf> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut no_more_flags = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let arg: &str = arg;
        if no_more_flags || !arg.starts_with('-') || arg == "-" {
            positional.push(arg);
            continue;
        }
        if arg == "--" {
            no_more_flags = true;
            continue;
        }
        match arg {
            "--json" | "-J" => json_output = true,
            "--roster" => roster = true,
            "--no-roster" => roster = false,
            // Two-token form only: the Python core's `_native_root_flags`
            // always emits `--root <dir>`, as do the sibling leaves.
            "--root" => match it.next() {
                Some(value) => root = Some(PathBuf::from(value)),
                None => {
                    return super::usage_refusal(
                        "fno agents claim status [OPTIONS] KEY",
                        "fno agents claim status --help",
                        "Error: Option '--root' requires an argument.",
                    );
                }
            },
            _ => {
                return super::usage_refusal(
                    "fno agents claim status [OPTIONS] KEY",
                    "fno agents claim status --help",
                    &format!("Error: No such option: {arg}"),
                );
            }
        }
    }
    if positional.len() > 1 {
        let detail = if positional.len() == 2 {
            format!("Error: Got unexpected extra argument ({})", positional[1])
        } else {
            format!(
                "Error: Got unexpected extra arguments ({})",
                positional[1..].join(" ")
            )
        };
        return super::usage_refusal(
            "fno agents claim status [OPTIONS] KEY",
            "fno agents claim status --help",
            &detail,
        );
    }
    let Some(key_arg) = positional.first() else {
        return super::usage_refusal(
            "fno agents claim status [OPTIONS] KEY",
            "fno agents claim status --help",
            "Error: Missing argument 'KEY'.",
        );
    };

    // `_normalize_status_key`: a bare well-formed node id reads as the
    // node key; everything else passes through untouched.
    let key = if crate::backlog::node_ref::is_wellformed_node_id(key_arg) {
        format!("node:{key_arg}")
    } else {
        key_arg.to_string()
    };
    // The store is read directly: Python's core swallows the engine
    // subprocess's stderr on a corrupted file, so the leaf must not print
    // one either - the corrupted payload alone is the answer. A colon-less
    // free key answers exit 0 like the engine arm this leaf replaces: the
    // bare-key refusal is Python's (`_refuse_unknown_status_key`), keyed on
    // the dict this payload becomes.
    let store_root = match root {
        Some(explicit) => Some(explicit),
        None => super::node_aware_root(&key),
    };
    let payload = match crate::claim_store::read(&key, store_root.as_deref()) {
        Ok(Some(rec)) => crate::claim_commands::claim_status_value(&rec),
        Ok(None) => json!({"key": key.clone(), "state": "free"}),
        Err(_) => json!({"key": key.clone(), "state": "corrupted"}),
    };
    let state_str = payload
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut info = match payload {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let node_id = key.strip_prefix("node:").unwrap_or("").to_string();
    let crosschecked =
        roster && !node_id.is_empty() && (state_str == "free" || state_str == "stale");

    // Declared here so the overlay and the riders below read them without
    // re-borrowing the reading.
    let mut candidate_names: Vec<String> = Vec::new();
    let mut worker_rows: Vec<FleetRow> = Vec::new();
    let mut roster_consulted = false;
    let mut roster_probe = String::new();
    let mut reading_opt: Option<worked::RosterReading> = None;
    let mut worked_names: Vec<String> = Vec::new();

    if crosschecked {
        // ONE fleet read, shared with the overlay join below. The probe
        // flavor matches the overlay (read_roster's only flavor): a
        // registry-only view still carries attribution, and a tolerated
        // degraded probe surfaces as roster_probe.
        let reading = worked::read_roster();
        roster_consulted = reading.consulted;
        if !reading.consulted {
            info.insert("roster_consulted".into(), Value::Bool(false));
            info.insert("roster_rows_scanned".into(), json!(0));
            info.insert("roster_rows_unresolved".into(), json!(0));
            info.insert("roster_workers".into(), Value::Array(Vec::new()));
            info.insert(
                "roster_skip_reason".into(),
                Value::String(reading.reason.clone()),
            );
        } else {
            candidate_names = reading
                .unresolved_rows
                .iter()
                .filter(|row| Path::new(&row.cwd).file_name() == Some(OsStr::new(&node_id)))
                .map(|row| row.name.clone())
                .collect();
            worker_rows = reading
                .workers_by_node
                .get(&node_id)
                .cloned()
                .unwrap_or_default();
            info.insert("roster_consulted".into(), Value::Bool(true));
            info.insert("roster_rows_scanned".into(), json!(reading.rows_scanned));
            info.insert(
                "roster_rows_unresolved".into(),
                json!(reading.rows_unresolved),
            );
            info.insert(
                "roster_unresolved_candidates".into(),
                json!(candidate_names),
            );
            info.insert(
                "roster_workers".into(),
                Value::Array(worker_rows.iter().map(roster_row_value).collect()),
            );
            if !reading.reason.is_empty() {
                roster_probe = reading.reason.clone();
                info.insert("roster_probe".into(), Value::String(roster_probe.clone()));
            }
            reading_opt = Some(reading);
        }
    }
    if crosschecked && roster_consulted {
        let entry = crate::graph_store::read_rows_strict(&crate::backlog::settings::graph_path())
            .ok()
            .and_then(|rows| {
                rows.into_iter()
                    .find(|e| e.get("id").and_then(Value::as_str) == Some(node_id.as_str()))
            });
        if let (Some(entry), Some(reading)) = (entry.as_ref(), reading_opt.as_ref()) {
            match worked::fold_over_entries(std::slice::from_ref(entry), reading) {
                Ok(folded) => {
                    // A session whose own phase row closed never renders as
                    // an occupancy candidate; the dropped names ride
                    // roster_closed_workers so the verdict can still name
                    // them as finished.
                    let closed = worked::closed_worker_session_ids(entry);
                    let mut finished_names: Vec<String> = Vec::new();
                    let mut kept_rows: Vec<FleetRow> = Vec::new();
                    for w in std::mem::take(&mut worker_rows) {
                        if closed.contains(&w.row_id) {
                            finished_names.push(if w.name.is_empty() {
                                "unknown".to_string()
                            } else {
                                w.name.clone()
                            });
                        } else {
                            kept_rows.push(w);
                        }
                    }
                    info.insert(
                        "roster_workers".into(),
                        Value::Array(kept_rows.iter().map(roster_row_value).collect()),
                    );
                    if !finished_names.is_empty() {
                        info.insert("roster_closed_workers".into(), json!(finished_names));
                    }
                    worker_rows = kept_rows;
                    worked_names = folded
                        .into_iter()
                        .find(|(nid, _)| nid == &node_id)
                        .map(|(_, names)| names)
                        .unwrap_or_default();
                }
                Err(exc) => {
                    eprintln!("worked overlay degraded: {exc}");
                }
            }
        }
        // Mutations below live inside the consulted gate, outside the
        // entry-if: the overlay's answer arrives as worked_by regardless of
        // whether a graph entry was found.
        let reachable: Vec<String> = worked_names
            .iter()
            .filter(|n| !n.contains(worked::UNMEASURABLE_LABEL_MARK))
            .cloned()
            .collect();
        if !reachable.is_empty() {
            info.insert("worked_by".into(), json!(reachable));
            info.insert("state".into(), Value::String("unknown".into()));
            let degraded = info
                .get("roster_rows_unresolved")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                > 0
                || worked_names
                    .iter()
                    .any(|n| n.contains(worked::UNMEASURABLE_LABEL_MARK));
            info.insert(
                "basis".into(),
                Value::String(
                    if degraded {
                        "live-worker-degraded-coverage"
                    } else {
                        "live-worker"
                    }
                    .into(),
                ),
            );
        }
        let unresolved = info
            .get("roster_rows_unresolved")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if !candidate_names.is_empty() {
            info.insert("state".into(), Value::String("unknown".into()));
            info.insert(
                "basis".into(),
                Value::String("unresolved-roster-row".into()),
            );
        } else if unresolved > 0 {
            info.insert("roster_coverage".into(), Value::String("degraded".into()));
        }
    }

    let info_value = Value::Object(info);
    if json_output {
        println!("{}", super::spaced(&info_value));
        return 0;
    }
    println!("{}", serde_json::to_string_pretty(&info_value).unwrap());
    if crosschecked {
        let (engaged, unmeasurable) = worked::classify_node_workers(&worker_rows);
        let map = info_value.as_object().expect("payload is an object");
        let mut line = roster_verdict_line(map, &engaged, &unmeasurable);
        if !roster_probe.is_empty() {
            line.push_str(&format!("; roster probe degraded: {roster_probe}"));
        }
        if let Some(sb) = map.get("session_basis").and_then(Value::as_str) {
            line.push_str(&format!("; session witness: {sb}"));
        }
        eprintln!("{line}");
    }
    let clause = expiry_clause(info_value.as_object().expect("payload is an object"));
    if !clause.is_empty() {
        eprintln!("{clause}");
    }
    0
}

/// The roster row dict the Python `workers_on` fold produced, same key order.
fn roster_row_value(w: &FleetRow) -> Value {
    json!({
        "name": w.name,
        "state": w.state,
        "cwd": w.cwd,
        "row_id": w.row_id,
        "pid": w.pid,
        "pid_start_time": w.pid_start_time,
        "mux": w.mux
            .as_ref()
            .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
            .unwrap_or(Value::Null),
        "stopped_at": w.stopped_at,
    })
}

fn num(info: &Map<String, Value>, key: &str) -> i64 {
    info.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// One line naming what was consulted and what it found. Each string is
/// produced by exactly one outcome; the finished-session clause rides only
/// the outcomes that reach its branch (the alarm outcomes return first).
fn roster_verdict_line(
    info: &Map<String, Value>,
    engaged: &[&FleetRow],
    unmeasurable: &[&FleetRow],
) -> String {
    // The claim's OWN state, never the hardcoded word free.
    let state = info
        .get("state")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("free");
    if !info
        .get("roster_consulted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return format!(
            "{state}, roster not consulted ({})",
            info.get("roster_skip_reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        );
    }

    // The overlay join is the occupancy authority.
    let worked_by: Vec<&str> = info
        .get("worked_by")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !worked_by.is_empty() {
        let mut line = format!(
            "UNCLAIMED but a live worker is on this node: {}",
            worked_by.join(", ")
        );
        let unresolved = num(info, "roster_rows_unresolved");
        if unresolved > 0 {
            line.push_str(&format!(
                "; coverage degraded: {unresolved} of {} rows unresolved",
                num(info, "roster_rows_scanned")
            ));
        }
        return line;
    }

    if !engaged.is_empty() {
        let rendered = engaged
            .iter()
            .map(|w| format!("{} (state={})", w.name, w.state))
            .collect::<Vec<_>>()
            .join(", ");
        let mut line = format!("UNCLAIMED but a live worker is on this node: {rendered}");
        let unresolved = num(info, "roster_rows_unresolved");
        if unresolved > 0 || !unmeasurable.is_empty() {
            line.push_str(&format!(
                "; coverage degraded: {unresolved} of {} rows unresolved",
                num(info, "roster_rows_scanned")
            ));
            if !unmeasurable.is_empty() {
                line.push_str(&format!(", {} undated", unmeasurable.len()));
            }
        }
        return line;
    }
    if !unmeasurable.is_empty() {
        let rendered = unmeasurable
            .iter()
            .map(|w| format!("{} (state={})", w.name, w.state))
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "{state}, no positively-live worker; {} row(s) unmeasured, \
             never live: {rendered}. Confirm with: fno agents peek {}",
            unmeasurable.len(),
            unmeasurable[0].name
        );
    }

    // Two ways a row reads finished: the predicate said so (still in
    // roster_workers), or the session closed its own phase row and the
    // display field dropped it.
    let mut finished: Vec<String> = info
        .get("roster_workers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|w| w.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Some(closed_list) = info.get("roster_closed_workers").and_then(Value::as_array) {
        finished.extend(
            closed_list
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string),
        );
    }
    let tail = if finished.is_empty() {
        String::new()
    } else {
        format!(
            "; {} finished session(s) resolved to it: {}",
            finished.len(),
            finished.join(", ")
        )
    };
    let unresolved = num(info, "roster_rows_unresolved");
    if unresolved > 0 {
        let scanned = format!(
            "{state}, no row resolved to this node ({} scanned, {unresolved} unresolved){tail}",
            num(info, "roster_rows_scanned")
        );
        let candidates: Vec<&str> = info
            .get("roster_unresolved_candidates")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !candidates.is_empty() {
            return format!(
                "{scanned}; {} unresolved row's worktree names this node: {}. \
                 Confirm with: fno agents peek {}",
                candidates.len(),
                candidates.join(", "),
                candidates[0]
            );
        }
        return format!("{scanned}; roster coverage degraded");
    }
    format!(
        "{state}, no live worker found (roster scanned: {} rows){tail}",
        num(info, "roster_rows_scanned")
    )
}

/// One stderr line when a live holder's TTL lapsed; empty for every
/// non-live state, where "holder live by" would lie about stale.
fn expiry_clause(info: &Map<String, Value>) -> String {
    if info.get("expired").and_then(Value::as_bool) != Some(true)
        || info.get("state").and_then(Value::as_str) != Some("live")
    {
        return String::new();
    }
    let age = info
        .get("expires_at")
        .and_then(Value::as_i64)
        .map(|e| {
            let mins = ((crate::claims::now_ms() - e) / 60_000).max(0);
            format!("{mins}m ago")
        })
        .unwrap_or_else(|| "unknown age".to_string());
    let basis = info
        .get("basis")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("live");
    format!("live (ttl expired {age}; holder live by {basis})")
}
