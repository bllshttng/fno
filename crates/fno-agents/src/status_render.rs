//! The `fno-agents status` render cluster, moved byte for byte out of `bin/
//! client.rs` (the file sat at 4,995 of the 5,000-line budget; the family
//! doors were about to land there). The arms readout, the attention set, the
//! degraded payload, and the human render; `run_status` in client.rs calls
//! them through `crate::status_render`.

use serde_json::{json, Value};

use crate::paths::AgentsHome;
use crate::spawn_gate::machine_reading_notes;

/// The control-plane arms rows from the arm journals plus the pr_watch tick
/// trace the readout's cause rules consult.
pub fn arms_readout(
    home: &AgentsHome,
) -> (
    Vec<crate::tick_ledger::ArmStatus>,
    crate::tick_ledger::TickTrace,
) {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // The journal list is owned by tick_ledger::journals, so the readout
    // and the arm_watch fold the same files and cannot drift.
    let journals = crate::tick_ledger::journals(home);
    let arms = crate::tick_ledger::read_arms_starved(&journals, now_unix);
    let trace = crate::tick_ledger::read_tick_trace_live(&journals, &arms, now_unix);
    (arms, trace)
}

/// The Rust-owned attention set: rows needing operator attention (unobserved,
/// stale, or failing), selected by the one predicate in `tick_ledger`. The
/// Python doctor consumes this list as its `red` set and never re-derives the
/// verdict from the legacy booleans.
pub fn arms_attention(
    arms: &[crate::tick_ledger::ArmStatus],
) -> Vec<crate::tick_ledger::ArmStatus> {
    arms.iter()
        .filter(|r| crate::tick_ledger::needs_attention(r))
        .cloned()
        .collect()
}

/// The degraded status payload (daemon down or unreachable): the arms readout
/// stands on its own, carrying both the full row list and the Rust-owned
/// attention set, so a daemon's health never changes the payload schema.
pub fn degraded_status_payload(arms: &[crate::tick_ledger::ArmStatus]) -> Value {
    json!({
        "schema_version": 1,
        "daemon": null,
        "arms": arms,
        "arms_attention": arms_attention(arms),
        "stuck_work": crate::stuck_work::status_value(
            &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        ),
    })
}

/// The human render: one owned line per arm (red rows first-class), then the
/// daemon block the JSON payload carries. `explain` filled every row's
/// `line`, so the render prints them without re-formatting.
pub fn print_status_human(result: &Value, arms: &[crate::tick_ledger::ArmStatus]) {
    println!("control-plane arms:");
    for arm in arms {
        println!("  {}", arm.line);
    }
    // The readout teaches its own detail command (the loops table).
    println!("  loop detail: fno agents loops table");
    if let Some(stuck) = result.get("stuck_work") {
        for line in crate::stuck_work::render_lines(stuck) {
            println!("{line}");
        }
    }
    let Some(daemon) = result.get("daemon").and_then(Value::as_object) else {
        return;
    };
    let state = daemon.get("state").and_then(Value::as_str).unwrap_or("?");
    let pid = daemon.get("pid").and_then(Value::as_u64);
    let uptime = daemon.get("uptime_secs").and_then(Value::as_u64);
    println!(
        "daemon: {state}{}{}",
        pid.map(|p| format!(" pid={p}")).unwrap_or_default(),
        uptime
            .map(|u| format!(" uptime={}s", u))
            .unwrap_or_default(),
    );
    if let Some(agents) = result.get("agents").and_then(Value::as_object) {
        println!(
            "agents: total={} by_status={}",
            agents.get("total").and_then(Value::as_u64).unwrap_or(0),
            agents
                .get("by_status")
                .map(|s| s.to_string())
                .unwrap_or_else(|| "{}".into())
        );
    }
    // Best-effort: a machine footprint cannot be read prints no line
    // rather than a stale or fabricated one. The keeper note names the path
    // the live store keeper runs from, beside the footer (AC13).
    let (machine_line, keeper_note) = machine_reading_notes();
    if let Some(line) = machine_line {
        println!("machine: {line}");
    }
    if let Some(note) = keeper_note {
        println!("{note}");
    }
}
