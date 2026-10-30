//! `fno agents watchdog`: the operator's unfinished-work report, and the explicit
//! doors to the internal recovery lanes.

use super::apply::{apply_verdict, outcome_event, SKIPPED};
use super::sweep::{
    emit_event, fresh_non_leave, last_events_signature, mail_gate, now_epoch_s, payload_verdicts,
    provider_outage_lines, run_sweep, union_signature, utc_stamp, verdict_signature,
    write_sweep_file, SweepWrite,
};
use super::{unknown_provider_report, Verdict, KEEPER, LEAVE, RECOVERABLE, VERDICTS};
use crate::backlog::worked::FleetRow;
use serde_json::{json, Value};
use std::path::PathBuf;

const USAGE: &str = "usage: fno agents watchdog [--json] [--apply] [--apply-all] [--only VERDICT] [--since DURATION] [--cwd DIR] [--session-id UUID] [--mail HANDLE]";

#[derive(Default)]
struct Args {
    json_out: bool,
    apply: bool,
    apply_all: bool,
    only: Option<String>,
    since: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
    mail_to: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    let mut out = Args::default();
    let mut index = 0;
    let value = |index: &mut usize, flag: &str| -> Result<String, String> {
        *index += 1;
        args.get(*index)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(None),
            "--json" | "-J" => out.json_out = true,
            "--apply" => out.apply = true,
            "--apply-all" => out.apply_all = true,
            "--only" => out.only = Some(value(&mut index, "--only")?),
            "--since" => out.since = Some(value(&mut index, "--since")?),
            "--cwd" => out.cwd = Some(value(&mut index, "--cwd")?),
            "--session-id" => out.session_id = Some(value(&mut index, "--session-id")?),
            "--mail" => out.mail_to = Some(value(&mut index, "--mail")?),
            other => return Err(format!("unknown argument {other}")),
        }
        index += 1;
    }
    Ok(Some(out))
}

fn config_mail_to() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::agents_config::config_lookup(&cwd, &["recovery", "watchdog", "mail_to"])
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn print_json(value: &Value) {
    println!("{value}");
}

/// The default surface: session verdicts stay behind `--apply` and `--only`, and
/// are never rendered as the operator's answer.
fn run_unfinished_report(now: f64, json_out: bool, mail_to: Option<&str>) -> i32 {
    let mut roots = super::unfinished::report_roots();
    if roots.is_empty() {
        roots.push(
            crate::paths_cli::repo_root()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        );
    }
    let snapshot = super::unfinished::build_report(&roots, now);
    let recipient = mail_to.map_or_else(config_mail_to, str::to_string);
    let note = |line: &str| eprintln!("{line}");
    let mut payload =
        super::unfinished::publish_report(&snapshot, "manual", now, &recipient, Some(&note));
    // The keeper lane rides the default report and is read-only whatever the
    // flags; collection is `--only keeper --apply-all`.
    let lane = super::keeper::discover();
    payload["keepers"] = lane.to_json();
    let lane_text = lane.render();
    let lane_lines: Vec<&str> = lane_text.lines().collect();
    // A -J reader must not need --only to learn a provider lane is down.
    let outages = measure_outages_safe(now);
    payload["provider_outages"] = outages.clone();
    if json_out {
        print_json(&payload);
        return 0;
    }
    println!("{}", super::unfinished::snapshot_digest(&snapshot));
    for line in lane_lines {
        println!("{line}");
    }
    for line in provider_outage_lines(Some(&outages)) {
        println!("{line}");
    }
    for warning in payload
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        eprintln!("warning: {warning}");
    }
    0
}

/// A crash lands as a named unknown report, never a missing key.
fn measure_outages_safe(now: f64) -> Value {
    let (rows, _warnings) = super::sweep::fleet_rows();
    let entries = super::sweep::read_entries(&rows);
    let report = super::outage::measure_provider_outages(&rows, now, &entries);
    if report.is_object() {
        report
    } else {
        unknown_provider_report("provider_outage_measure_failed")
    }
}

fn run_keeper_lane(args: &Args, now: f64) -> i32 {
    let lane = super::keeper::discover();
    if args.json_out {
        let mut payload = json!({"generated_at": utc_stamp(now), "lane": KEEPER});
        if let (Some(target), Value::Object(fields)) = (payload.as_object_mut(), lane.to_json()) {
            target.extend(fields);
        }
        if args.apply_all {
            let (keepers, children) = lane.reap_keepers();
            payload["reaped_keepers"] = json!(keepers);
            payload["reaped_children"] = json!(children);
        }
        print_json(&payload);
        return if lane.broken() { 3 } else { 0 };
    }
    println!("{}", lane.render());
    if lane.broken() {
        return 3;
    }
    if args.apply && !args.apply_all {
        println!("--apply never kills a keeper (wake lane only); the collect flag is --apply-all");
    } else if args.apply_all {
        let (keepers, children) = lane.reap_keepers();
        println!(
            "reaped {} keeper(s), {} hosted child(ren)",
            keepers.len(),
            children.len()
        );
    }
    0
}

fn run_recoverable_lane(args: &Args, now: f64) -> i32 {
    use super::recoverable as rec;
    if let Some(session_id) = args.session_id.as_deref().filter(|s| !s.is_empty()) {
        if !rec::is_canonical_full_uuid(session_id) {
            eprintln!("fno agents watchdog: --session-id requires one canonical full UUID");
            return 2;
        }
    }
    let since = args.since.as_deref().unwrap_or("24h");
    let (recency, scope) = match (
        rec::parse_recovery_since(since),
        rec::resolve_recovery_cwd(args.cwd.as_deref()),
    ) {
        (Ok(recency), Ok(scope)) => (recency, scope),
        (Err(exc), _) | (_, Err(exc)) => {
            eprintln!("fno agents watchdog: {exc}");
            return 2;
        }
    };
    let selected = args.session_id.as_deref().filter(|s| !s.is_empty());
    let (mut payload, rows, scan) =
        rec::run_recoverable_sweep(&scope, recency, Some(now), selected);
    let verdicts = payload_verdicts(&payload);
    let print_verdicts = |verdicts: &[Verdict]| {
        for (verdict, row) in verdicts.iter().zip(rows.iter()) {
            println!(
                "{:11} {} handle={} cwd={}",
                verdict.verdict, verdict.row_id, verdict.name, row.cwd
            );
        }
    };
    let apply_and_emit = |payload: &Value| -> Vec<Value> {
        let results = rec::apply_recoverable(&scan);
        if args.json_out {
            let mut combined = payload.clone();
            combined["results"] = json!(results);
            combined["result_counts"] = rec::recovery_result_counts(&results);
            print_json(&combined);
        }
        results
    };
    if !scan.complete {
        if args.apply || args.apply_all {
            let results = apply_and_emit(&payload);
            let detail = results
                .first()
                .and_then(|r| r.get("detail"))
                .and_then(Value::as_str)
                .unwrap_or("");
            eprintln!("{detail}");
            return 3;
        }
        if args.json_out {
            print_json(&payload);
        } else {
            print_verdicts(&verdicts);
            for warning in payload
                .get("warnings")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                eprintln!("warning: {warning}");
            }
            println!(
                "recoverable=unknown complete=false scanned={}",
                payload["scanned_count"]
            );
        }
        return 0;
    }
    if let (Some(session_id), true) = (selected, scan.recoverable.is_empty()) {
        payload["selection_error"] = json!("selected session is not recoverable");
        if args.json_out {
            print_json(&payload);
        } else {
            eprintln!("fno agents watchdog: session {session_id} is not one recoverable candidate");
        }
        return 3;
    }
    let previous_events = last_events_signature();
    let signature = verdict_signature(&payload);
    write_sweep_file(
        "manual",
        now,
        SweepWrite {
            counts: payload.get("counts").cloned(),
            signature: Some(signature.clone()),
            events_signature: Some(signature),
            recoverable_count: payload.get("recoverable_count").and_then(Value::as_i64),
            ..SweepWrite::default()
        },
    );
    let fresh = fresh_non_leave(&payload, &previous_events);
    for verdict in &verdicts {
        if fresh.contains(&verdict.row_id) {
            emit_event(
                "watchdog_verdict",
                json!({
                    "row_id": verdict.row_id,
                    "name": verdict.name,
                    "verdict": verdict.verdict,
                    "basis": verdict.basis,
                }),
            );
        }
    }
    if !args.apply && !args.apply_all {
        if args.json_out {
            print_json(&payload);
        } else {
            print_verdicts(&verdicts);
            println!(
                "recoverable={} usable={} unusable={} complete=true scanned={}",
                payload["recoverable_count"],
                payload["usable_recoverable_count"],
                payload["unusable_recoverable_count"],
                payload["scanned_count"]
            );
        }
        return 0;
    }
    let results = apply_and_emit(&payload);
    if !args.json_out {
        for result in &results {
            let outcome = result.get("outcome").and_then(Value::as_str).unwrap_or("");
            let detail = result.get("detail").and_then(Value::as_str).unwrap_or("");
            let line = format!("{outcome:9} {detail}");
            if outcome == "applied" {
                println!("{line}");
            } else {
                eprintln!("{line}");
            }
        }
        let counts = rec::recovery_result_counts(&results);
        println!(
            "applied={} refused={} deferred={}",
            counts["applied"], counts["refused"], counts["deferred"]
        );
    }
    0
}

fn run_verdict_lanes(args: &Args, now: f64) -> i32 {
    let (payload, rows) = run_sweep(now, None);
    if let Some(refused) = payload.get("refused").and_then(Value::as_str) {
        // A zero-row roster is an unreadable instrument: write no sweep file and
        // advance no gate, so staleness reads loud. The warnings say why.
        eprintln!("fno agents watchdog: {refused}");
        for warning in payload
            .get("warnings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            eprintln!("  {warning}");
        }
        return 3;
    }
    let all = payload_verdicts(&payload);
    let mut pairs: Vec<(Verdict, FleetRow)> = all.into_iter().zip(rows).collect();
    let mut shown_counts = payload.get("counts").cloned().unwrap_or_else(|| json!({}));
    if let Some(only) = args.only.as_deref() {
        pairs.retain(|(v, _)| v.verdict == only);
        // A filtered view must not report the full sweep's counts.
        let mut counts = serde_json::Map::new();
        for (v, _) in &pairs {
            let next = counts.get(&v.verdict).and_then(Value::as_i64).unwrap_or(0) + 1;
            counts.insert(v.verdict.clone(), json!(next));
        }
        shown_counts = Value::Object(counts);
    }
    // Push, not pull: mail before writing the sweep file, so only a delivered
    // digest advances the change gate.
    let recipient = args.mail_to.clone().unwrap_or_else(config_mail_to);
    let (ok, receipt, signature) = mail_gate(&payload, &recipient);
    if !ok {
        eprintln!("watchdog mail: {receipt}");
    }
    // A filtered run publishes only its own rows, and stamps the union with what
    // was already published, or the next tick re-emits every filtered-out row.
    let shown_verdicts: Vec<&Verdict> = pairs.iter().map(|(v, _)| v).collect();
    let mut events_payload = payload.clone();
    if args.only.is_some() {
        events_payload["verdicts"] = json!(shown_verdicts);
    }
    let prev_events = last_events_signature();
    let stamp = if args.only.is_some() {
        union_signature(&[&prev_events, &verdict_signature(&events_payload)])
    } else {
        verdict_signature(&events_payload)
    };
    write_sweep_file(
        "manual",
        now,
        SweepWrite {
            counts: payload.get("counts").cloned(),
            signature: Some(signature),
            events_signature: Some(stamp),
            terminal_harness_rows: Some(
                payload
                    .get("terminal_harness_rows")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            ),
            provider_outages: Some(
                payload
                    .get("provider_outages")
                    .cloned()
                    .filter(|v| !v.is_null())
                    .unwrap_or_else(|| unknown_provider_report("provider_outage_payload_missing")),
            ),
            ..SweepWrite::default()
        },
    );
    // Classification events ride every mode, gated so a filtered hand-run neither
    // diverges from the tick's record nor re-emits the fleet.
    let fresh = fresh_non_leave(&events_payload, &prev_events);
    for (v, _) in &pairs {
        if v.verdict != LEAVE && fresh.contains(&v.row_id) {
            emit_event(
                "watchdog_verdict",
                json!({"row_id": v.row_id, "name": v.name, "verdict": v.verdict, "basis": v.basis}),
            );
        }
    }
    if !args.apply && !args.apply_all {
        if args.json_out {
            let mut filtered = payload.clone();
            filtered["verdicts"] = json!(shown_verdicts);
            filtered["counts"] = shown_counts;
            print_json(&filtered);
            return 0;
        }
        for (v, _) in &pairs {
            println!("{:34} {:9} {:8} {}", v.name, v.state, v.verdict, v.basis);
        }
        for warning in payload
            .get("warnings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            eprintln!("warning: {warning}");
        }
        let mut shown: Vec<String> = shown_counts
            .as_object()
            .map(|m| m.iter().map(|(k, c)| format!("{k}={c}")).collect())
            .unwrap_or_default();
        shown.sort();
        println!("{} row(s): {}", pairs.len(), shown.join(" "));
        println!(
            "terminal harness rows: {}",
            payload
                .get("terminal_harness_rows")
                .and_then(Value::as_i64)
                .unwrap_or(0)
        );
        for line in provider_outage_lines(payload.get("provider_outages")) {
            println!("{line}");
        }
        return 0;
    }
    let lanes = if args.apply_all { "all" } else { "wake" };
    let mut results: Vec<Value> = Vec::new();
    for (v, row) in &pairs {
        let (outcome, detail) = apply_verdict(v, lanes, &row.cwd, None, row.node.as_deref());
        results.push(json!({
            "row_id": v.row_id,
            "verdict": v.verdict,
            "outcome": outcome,
            "detail": detail,
        }));
        if outcome == SKIPPED {
            // The one silent outcome: a line per healthy row drowns the few that
            // acted. Anything else surfaces, so a new outcome cannot go silent.
            continue;
        }
        let line = format!("{outcome:9} {:34} {detail}", v.name);
        if outcome != "applied" {
            eprintln!("{line}");
        } else if !args.json_out {
            println!("{line}");
        }
        emit_event(
            outcome_event(&outcome),
            json!({"row_id": v.row_id, "verdict": v.verdict, "detail": detail, "outcome": outcome}),
        );
    }
    let mut document = json!({"results": results});
    if args.apply_all && args.only.is_none() {
        // Keeper collection rides --apply-all and the full surface only: a
        // filtered run must not widen its destructive scope.
        let lane = super::keeper::discover();
        let (keepers, children) = lane.reap_keepers();
        if let Some(reason) = &lane.broken_reason {
            eprintln!("keeper lane: {reason}");
        } else if !keepers.is_empty() || !children.is_empty() {
            println!(
                "reaped {} keeper(s), {} hosted child(ren)",
                keepers.len(),
                children.len()
            );
        }
        document["keepers"] = json!(keepers);
        document["keeper_children"] = json!(children);
    }
    if args.json_out {
        print_json(&document);
    }
    0
}

/// `fno agents watchdog`, nested under the census action.
pub fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            println!("{USAGE}");
            return 0;
        }
        Err(message) => {
            eprintln!("fno agents watchdog: {message}");
            eprintln!("{USAGE}");
            return 2;
        }
    };
    if let Some(only) = parsed.only.as_deref() {
        if !VERDICTS.contains(&only) {
            eprintln!("fno agents watchdog: unknown verdict '{only}'");
            return 2;
        }
    }
    let now = now_epoch_s();
    match parsed.only.as_deref() {
        Some(RECOVERABLE) => run_recoverable_lane(&parsed, now),
        Some(KEEPER) => run_keeper_lane(&parsed, now),
        None if !parsed.apply && !parsed.apply_all => {
            run_unfinished_report(now, parsed.json_out, parsed.mail_to.as_deref())
        }
        _ => run_verdict_lanes(&parsed, now),
    }
}

/// `distress-verdicts`: `{"<session>": "<verdict>"|null}` for blocked_child's
/// lookup. An unavailable sweep is enrichment lost, never fatal.
pub(crate) fn distress_verdicts(sessions: &[String]) -> Value {
    let (payload, _rows) = run_sweep(now_epoch_s(), None);
    let verdicts = payload_verdicts(&payload);
    let mut out = serde_json::Map::new();
    for session in sessions {
        let word = verdicts
            .iter()
            .find(|v| v.row_id == *session)
            .map_or(Value::Null, |v| json!(v.verdict));
        out.insert(session.clone(), word);
    }
    Value::Object(out)
}
