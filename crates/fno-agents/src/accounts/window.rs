//! `fno config accounts window [--json] [--warn-seconds N]`: where each
//! account's rolling usage window stands, without probing anything. Ported
//! from the `window` command in `fno.adapters.providers.cli`; the parity
//! cases live in `tests/accounts_window_parity.rs`.
//!
//! A probe-less provider (z.ai, glm) reads UNKNOWN to the quota layer, so the
//! one account that caps can be the one nothing sees. Where a probe exists
//! this prints its headroom verdict. Where none does, it prints a projection
//! from the recorded window open plus the record's `window_minutes`. A
//! projection is never a utilization reading: it writes no usage window, and
//! a record with nothing recorded reads `unknown`, not zero.

use crate::accounts::records::Record;
use crate::accounts::state;
use crate::accounts::usage::resets_in;
use serde_json::{json, Map, Value};
use std::path::Path;

const HELP: &str = "Usage: fno config accounts window [--json] [--warn-seconds N]

  Where each account's rolling usage window is, without probing anything.

  --json, -J          Emit one JSON object keyed by account id.
  --warn-seconds N    Emit provider_window_closing under this many seconds to
                      close (default 1800). Once per window.";

/// What a closing window does. `warn` flips the once-per-window flag and is
/// true only when this call set it; `emit` logs one event's data.
pub struct Effects<'a> {
    pub warn: &'a dyn Fn(&str) -> bool,
    pub emit: &'a dyn Fn(Map<String, Value>),
}

/// One row per record, keyed by id, in the Python key order. A window at or
/// under `threshold` seconds to close warns once.
pub fn collect(
    records: &[Record],
    state_path: &Path,
    threshold: f64,
    now: f64,
    fx: &Effects<'_>,
) -> Map<String, Value> {
    let mut rows = Map::new();
    for record in records {
        let span = record
            .window_minutes
            .filter(|m| *m != 0)
            .map(|m| m as f64 * 60.0);
        let proj = state::project_window(state_path, &record.id, span, now);
        let verdict = state::headroom(
            state_path,
            &record.id,
            now,
            state::DEFAULT_USAGE_TTL_SECONDS,
            state::DEFAULT_THRESHOLD_PCT,
        );
        rows.insert(
            record.id.clone(),
            json!({
                "harness": record.harness,
                "headroom": verdict.verdict.as_str(),
                "headroom_resets_at": verdict.resets_at,
                "window_minutes": record.window_minutes,
                "opened_at": proj.opened_at,
                "closes_at": proj.closes_at,
                // Always present, null included: a key only on the happy path
                // lets a shapeless answer pass a grep.
                "closes_in_s": proj.closes_in_s.map(|s| s.trunc() as i64),
            }),
        );
        let Some(closes_in) = proj.closes_in_s.filter(|s| *s <= threshold) else {
            continue;
        };
        if (fx.warn)(&record.id) {
            let mut data = Map::new();
            data.insert("provider_id".into(), json!(record.id));
            data.insert("label".into(), json!("window"));
            data.insert("closes_in_s".into(), json!(closes_in.trunc() as i64));
            data.insert("closes_at".into(), json!(proj.closes_at));
            (fx.emit)(data);
        }
    }
    rows
}

/// The `--json` map as `json.dumps` printed it, or one line per record.
pub fn render(records: &[Record], rows: &Map<String, Value>, as_json: bool, now: f64) -> String {
    if as_json {
        let map = Value::Object(rows.clone());
        return crate::backlog::maintenance::validity::py_dumps(&map, false, true);
    }
    if records.is_empty() {
        return "No accounts configured.".to_string();
    }
    rows.iter()
        .map(|(id, row)| {
            let when = match row["closes_in_s"] {
                Value::Null => "unknown".to_string(),
                _ => format!("closes {}", resets_in(row["closes_at"].as_f64(), now)),
            };
            format!(
                "{id}  [{}]  headroom={:<9} {when}",
                row["harness"].as_str().unwrap_or_default(),
                row["headroom"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `[--json] [--warn-seconds N]`: `None` for `--help`, else the JSON flag and
/// the warn threshold. `Err` is the usage line; it exits 2.
pub fn parse(args: &[String]) -> Result<Option<(bool, f64)>, String> {
    let mut as_json = false;
    let mut threshold = state::DEFAULT_WINDOW_WARN_SECONDS;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = crate::accounts::add::split_flag(arg);
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--json" | "-J" if inline.is_none() => as_json = true,
            "--warn-seconds" => {
                let value = inline
                    .or_else(|| it.next().cloned())
                    .ok_or("error: option '--warn-seconds' requires an argument")?;
                threshold = value.trim().parse().map_err(|_| {
                    format!(
                        "error: invalid value for '--warn-seconds': '{value}' is not a valid float"
                    )
                })?;
            }
            other => return Err(format!("error: no such option: {other}")),
        }
    }
    Ok(Some((as_json, threshold)))
}

/// `fno-agents spawn-overlay accounts window [--json] [--warn-seconds N]`.
pub fn run(args: &[String]) -> i32 {
    let (as_json, threshold) = match parse(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            println!("{HELP}");
            return 0;
        }
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let cwd = crate::accounts::show::pwd();
    let accounts = match crate::accounts::records::load(Some(&cwd), &cwd) {
        Ok(accounts) => accounts,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let path = crate::provider_cap::runtime_state_path(&cwd);
    let now = state::wall_now();
    let warn = |id: &str| state::mark_warned(&path, id, now, state::LOCK_TIMEOUT).unwrap_or(false);
    // Best effort: a warning that cannot be logged was still printed.
    let emit = |data: Map<String, Value>| {
        if let Some(dir) = crate::agents_config::state_dir(&cwd) {
            let emitter = crate::events::EventEmitter::new(&dir.join("events.jsonl"), "daemon");
            let _ = emitter.emit_fields("provider_window_closing", data);
        }
    };
    let rows = collect(
        &accounts.records,
        &path,
        threshold,
        now,
        &Effects {
            warn: &warn,
            emit: &emit,
        },
    );
    println!("{}", render(&accounts.records, &rows, as_json, now));
    0
}
