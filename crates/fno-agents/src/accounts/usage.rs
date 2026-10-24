//! `fno config accounts usage`: per-account rate-limit windows, used percent
//! and reset time. Ported from the `usage` command in
//! `fno.adapters.providers.cli` and `refresh_usage_detailed` in
//! `runtime_state.py`; the parity cases live in `tests/accounts_usage_parity.rs`.
//!
//! A refresh renders the observation the probe just produced, never a second
//! cache read: a losing write race or a TTL edge would otherwise turn a live
//! reading into `unknown`. Every unknown names the boundary it failed at.

use crate::accounts::binding::{self, AccountBinding, BindingRequest, SlotProbe, MATCHED};
use crate::accounts::records::{self, Record};
use crate::accounts::state::{self, Snapshot};
use crate::accounts::usage_probe;
use serde_json::{json, Map, Value};
use std::path::Path;

const USAGE_HELP: &str = "Usage: fno config accounts usage [--refresh] [--json]\n\n  Show per-account rate-limit windows: used % and reset time.\n\n  A provider with no fresh snapshot shows `unknown` with the reason, and the\n  command still exits 0: probing is advisory and fail-open.\n\n  --refresh   Force a fresh probe (bypass the TTL cache).\n  --json, -J  Emit a one-line JSON object keyed by provider id.";

/// The world one report reads. The probe and the cache write are passed in,
/// so a test drives the report without a network or a lock.
pub struct Inputs<'a> {
    pub records: &'a [Record],
    pub state: &'a Path,
    pub store: Option<&'a Path>,
    pub slot: &'a dyn SlotProbe,
    pub probe: &'a dyn Fn(&Record) -> (Option<Snapshot>, Option<String>),
    /// True when the snapshot landed in the cache.
    pub persist: &'a dyn Fn(&Record, &Snapshot) -> bool,
    pub refresh: bool,
    pub ttl: f64,
    pub now: f64,
}

/// One record's row: its JSON entry and, for a known claude reading, the
/// account the binding proved behind it.
pub struct Row {
    pub entry: Value,
    pub identity: Option<AccountBinding>,
}

fn unknown(reason: &str) -> Row {
    Row {
        entry: json!({"state": "unknown", "reason": reason}),
        identity: None,
    }
}

/// `refresh_usage_detailed` for one record: a fresh cached reading, else a
/// probe whose snapshot is persisted and then reported whatever the write did.
fn observe(
    inp: &Inputs<'_>,
    record: &Record,
    ttl: f64,
) -> (Option<Snapshot>, Option<String>, Option<bool>) {
    if let Some(cached) = state::read_usage(inp.state, &record.id, ttl, inp.now) {
        return (Some(cached), None, None);
    }
    if !inp.refresh {
        return (None, Some("not-probed".to_string()), None);
    }
    match (inp.probe)(record) {
        (Some(snap), _) => {
            let persisted = (inp.persist)(record, &snap);
            (Some(snap), None, Some(persisted))
        }
        (None, reason) => (None, reason, None),
    }
}

fn identity(inp: &Inputs<'_>, record: &Record) -> Option<AccountBinding> {
    if record.harness != "claude" {
        return None;
    }
    Some(binding::resolve(
        &BindingRequest {
            record: Some(record),
            harness: &record.harness,
            bearer: None,
            root: inp.store?,
            records: inp.records,
            now: inp.now,
            ttl: binding::PRINCIPAL_TTL_S,
        },
        inp.slot,
    ))
}

/// Every record's row, in config order.
pub fn collect(inp: &Inputs<'_>) -> Vec<Row> {
    let ttl = if inp.refresh { 0.0 } else { inp.ttl };
    inp.records
        .iter()
        .map(|record| {
            let (snap, reason, persisted) = observe(inp, record, ttl);
            let snap = match snap {
                Some(s) if !s.windows.is_empty() => s,
                Some(_) => return unknown("no-windows"),
                None => return unknown(reason.as_deref().unwrap_or("unknown")),
            };
            let mut entry = json!({
                "source": snap.source,
                "probed_at": snap.probed_at,
                // A partial or coarse reading that renders like a whole precise
                // one collapses what the markers keep apart.
                "partial": snap.partial,
                "confidence": snap.confidence,
                "windows": snap.windows.iter().map(|w| json!({
                    "label": w.label, "used_pct": w.used_pct, "resets_at": w.resets_at,
                })).collect::<Vec<_>>(),
            });
            if persisted == Some(false) {
                entry["persisted"] = json!(false);
            }
            let identity = identity(inp, record);
            if let Some(got) = &identity {
                entry["identity"] = json!({
                    "status": got.status,
                    "account": got.matched_record,
                    "reason": got.reason,
                    "observed_at": got.observed_at,
                });
            }
            Row { entry, identity }
        })
        .collect()
}

/// The `--json` object keyed by record id, as `json.dumps` printed it.
pub fn to_map(records: &[Record], rows: &[Row]) -> Map<String, Value> {
    records
        .iter()
        .zip(rows)
        .map(|(r, row)| (r.id.clone(), row.entry.clone()))
        .collect()
}

/// `_fmt_resets_in`: a reset epoch as `in 40m`, `in 2h05m`, `reset`, or
/// `reset unknown` for a window reported without one.
pub fn resets_in(resets_at: Option<f64>, now: f64) -> String {
    let Some(at) = resets_at else {
        return "reset unknown".to_string();
    };
    let delta = at - now;
    if delta <= 0.0 {
        return "reset".to_string();
    }
    let mins = (delta / 60.0).floor() as i64;
    if mins < 60 {
        return format!("in {mins}m");
    }
    format!("in {}h{:02}m", mins / 60, mins % 60)
}

/// The human report: one line per window, one per unknown, then the
/// identity line and, past the threshold, the manual switch `remedy`.
pub fn text_lines(
    records: &[Record],
    rows: &[Row],
    threshold: f64,
    now: f64,
    remedy: &str,
) -> Vec<String> {
    if records.is_empty() {
        return vec!["No accounts configured.".to_string()];
    }
    let mut lines = Vec::new();
    for (record, row) in records.iter().zip(rows) {
        let prefix = format!("{}  [{}]  ", record.id, record.harness);
        let entry = &row.entry;
        if entry.get("state").and_then(Value::as_str) == Some("unknown") {
            let reason = entry["reason"].as_str().unwrap_or("unknown");
            lines.push(format!("{prefix}unknown ({reason})"));
            continue;
        }
        let mut suffix = String::new();
        if let Some(c) = entry["confidence"]
            .as_str()
            .filter(|c| !c.is_empty() && *c != "exact")
        {
            suffix.push_str(&format!("  ({c})"));
        }
        if entry["partial"].as_bool() == Some(true) {
            suffix.push_str("  PARTIAL: a window was rejected, treat as a floor");
        }
        let windows = entry["windows"].as_array().cloned().unwrap_or_default();
        let mut worst = 0.0_f64;
        for w in &windows {
            let used = w["used_pct"].as_f64().unwrap_or(0.0);
            worst = worst.max(used);
            lines.push(format!(
                "{prefix}{:<8} {used:5.1}%  {}{suffix}",
                w["label"].as_str().unwrap_or(""),
                resets_in(w["resets_at"].as_f64(), now),
            ));
        }
        let Some(got) = &row.identity else {
            continue;
        };
        if got.status == MATCHED {
            let served = got
                .matched_record
                .as_deref()
                .or(got.requested_record.as_deref())
                .unwrap_or("None");
            let age = ((now - got.observed_at) / 60.0).floor().max(0.0) as i64;
            lines.push(format!("{prefix}identity: {served} (observed {age}m ago)"));
        } else {
            lines.push(format!("{prefix}{}", got.receipt()));
        }
        if worst >= threshold {
            lines.push(format!("{prefix}manual switch: {remedy}"));
        }
    }
    lines
}

/// The command's stdout, less its final newline: the `--json` object as
/// `json.dumps` printed it, or the human report.
pub fn render(
    records: &[Record],
    rows: &[Row],
    as_json: bool,
    threshold: f64,
    now: f64,
    remedy: &str,
) -> String {
    if as_json {
        let map = Value::Object(to_map(records, rows));
        return crate::backlog::maintenance::validity::py_dumps(&map, false, true);
    }
    text_lines(records, rows, threshold, now, remedy).join("\n")
}

/// The live report for `cwd`: the configured records, the runtime-state
/// cache, the Keychain and the probe endpoints.
pub fn report(cwd: &Path, refresh: bool, now: f64) -> Result<(Vec<Record>, Vec<Row>, f64), String> {
    let accounts = records::load(Some(cwd), cwd).map_err(|e| e.to_string())?;
    let quota = crate::backlog::quota_route::load_quota_config(cwd.to_str());
    let path = crate::provider_cap::runtime_state_path(cwd);
    // Only a refresh writes, so only a refresh folds; a plain read stays a read.
    if refresh {
        state::fold_legacy_files(&path, cwd, None);
    }
    let rows = usage_probe::with_system(cwd, Some(cwd), &accounts.records, |ctx| {
        let probe = |r: &Record| {
            let (snap, reason) = usage_probe::probe_detail(ctx, r, now);
            (snap, reason.map(str::to_string))
        };
        let persist = |r: &Record, s: &Snapshot| {
            state::write_usage(&path, &r.id, s.clone(), now, state::LOCK_TIMEOUT).unwrap_or(false)
        };
        collect(&Inputs {
            records: &accounts.records,
            state: &path,
            store: ctx.store.as_deref(),
            slot: ctx.slot,
            probe: &probe,
            persist: &persist,
            refresh,
            ttl: quota.probe_ttl_seconds,
            now,
        })
    });
    Ok((accounts.records, rows, quota.defer_threshold_pct))
}

/// `usage --refresh --json` in process: the map the route gate and the
/// capacity refresh read. `None` when the account config is unreadable.
pub fn refreshed(cwd: &Path) -> Option<Map<String, Value>> {
    let (records, rows, _) = report(cwd, true, state::wall_now()).ok()?;
    Some(to_map(&records, &rows))
}

/// `fno-agents spawn-overlay accounts usage [--refresh] [--json]`.
pub fn run(args: &[String]) -> i32 {
    let mut refresh = false;
    let mut as_json = false;
    for arg in args {
        match arg.as_str() {
            "--refresh" => refresh = true,
            "--json" | "-J" => as_json = true,
            "-h" | "--help" => {
                println!("{USAGE_HELP}");
                return 0;
            }
            other => {
                eprintln!("error: no such option: {other}");
                return 2;
            }
        }
    }
    let cwd = crate::accounts::show::pwd();
    let now = state::wall_now();
    let (records, rows, threshold) = match report(&cwd, refresh, now) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let remedy = crate::accounts::reconcile::slot_switch_remedy("the other account");
    println!(
        "{}",
        render(&records, &rows, as_json, threshold, now, &remedy)
    );
    0
}
