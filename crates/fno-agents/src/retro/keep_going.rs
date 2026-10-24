//! The autonomous keep-going engine: classify each landed carve-out
//! follow-up into a think, build, or file arm and dispatch it under the
//! shared per-day ceiling (`crate::think_daily`), so think and target
//! dispatches spend one budget.
//!
//! Every follow-up is already a filed node, so a capped or failed dispatch
//! leaves the node in place and drops nothing. It runs in autonomous mode
//! only, and only when `keep_going.enabled` is set: interactive harvests
//! queue nodes for a human ack, which auto-dispatch would bypass.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::land::LandResult;
use super::{Candidate, KIND_CARVEOUT};

pub const ARM_THINK: &str = "think";
pub const ARM_BUILD: &str = "build";
pub const ARM_FILE: &str = "file";

pub const OUTCOME_DISPATCHED: &str = "dispatched";
pub const OUTCOME_FILED: &str = "filed";
pub const OUTCOME_CAPPED: &str = "capped";
pub const OUTCOME_FAILED: &str = "failed";

const SPAWN_TIMEOUT: Duration = Duration::from_secs(600);

fn truthy_word(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Whether the engine is armed: the autonomy master switch first, then the
/// `FNO_KEEP_GOING` force on or off, then `keep_going.enabled`, default off.
pub fn enabled(project_root: Option<&Path>) -> bool {
    if !crate::backlog::advance_settings::autonomy_master_enabled(project_root) {
        return false;
    }
    if let Ok(v) = std::env::var("FNO_KEEP_GOING") {
        return truthy_word(&v);
    }
    match crate::backlog::advance_settings::load_merged(project_root).pointer("/keep_going/enabled")
    {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => truthy_word(s),
        Some(Value::Number(n)) => n.as_i64().is_some_and(|n| n != 0),
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FollowupResult {
    pub node_id: String,
    pub arm: String,
    pub outcome: String,
}

/// The arm for one candidate. Only carve-outs dispatch: an `oos-bug` is a
/// clear fix (build), a `deferred` row is an open decision (think), and
/// anything else is filed only, never dispatched on a guess.
pub fn classify_followup(candidate: &Candidate) -> &'static str {
    if candidate.extra_str("kind") != Some(KIND_CARVEOUT) {
        return ARM_FILE;
    }
    match candidate
        .extra_str("subkind")
        .unwrap_or("")
        .trim()
        .to_lowercase()
        .as_str()
    {
        "oos-bug" => ARM_BUILD,
        "deferred" => ARM_THINK,
        _ => ARM_FILE,
    }
}

fn fno_command(cwd: Option<&str>) -> std::process::Command {
    let mut cmd = std::process::Command::new(crate::scrape::fno_bin());
    if let Some(dir) = cwd.filter(|d| !d.is_empty()) {
        cmd.current_dir(dir);
    }
    cmd
}

/// `fno do think dispatch <node> --json`. It bumps the shared counter
/// itself on a real spawn, so only a `spawned` decision counts as a
/// dispatch: exit 0 also covers `offered` and `noop`, which bump nothing.
fn dispatch_think(node_id: &str, cwd: Option<&str>) -> bool {
    let mut cmd = fno_command(cwd);
    cmd.args(["do", "think", "dispatch", node_id, "--json"]);
    let Ok(out) = crate::bounded_cmd::output_with_timeout_result(cmd, SPAWN_TIMEOUT.as_secs())
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    serde_json::from_slice::<Value>(&out.stdout)
        .ok()
        .and_then(|v| {
            v.get("decision")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .as_deref()
        == Some("spawned")
}

/// A detached `/target --no-merge <node>` worker. The env carrier backs the
/// flag, so a worker that loses it after compaction still folds the
/// refusal at init. True only on a spawn receipt.
fn spawn_target_worker(node_id: &str, cwd: Option<&str>) -> bool {
    let name = match crate::naming::dispatch_agent_name(
        Some("kg"),
        "t",
        node_id,
        Some("follow-up"),
        None,
        None,
        None,
    ) {
        Ok(name) => name,
        Err(err) => {
            eprintln!("keep_going: unrepresentable worker name for {node_id}: {err}");
            return false;
        }
    };
    let mut cmd = fno_command(None);
    cmd.args(["agents", "spawn", "--harness", "claude"]);
    match cwd.filter(|d| !d.is_empty()) {
        Some(dir) => cmd.args(["--cwd", dir]),
        None => cmd.arg("--fresh"),
    };
    cmd.args(["--name", &name, &format!("/target --no-merge {node_id}")]);
    cmd.env("TARGET_NO_MERGE", "1");
    let Ok(out) = crate::bounded_cmd::output_with_timeout_result(cmd, SPAWN_TIMEOUT.as_secs())
    else {
        return false;
    };
    out.status.success()
        && !crate::backlog::advance_dispatch::spawn_receipt_identity(&String::from_utf8_lossy(
            &out.stdout,
        ))
        .is_empty()
}

/// The injectable seams of one dispatch pass.
pub struct Seams<'a> {
    pub count: &'a dyn Fn() -> i64,
    pub bump: &'a dyn Fn(),
    pub cap: i64,
    pub think: &'a dyn Fn(&str, Option<&str>) -> bool,
    pub build: &'a dyn Fn(&str, Option<&str>) -> bool,
}

/// Classify each landed carve-out node and dispatch its arm under the
/// ceiling. The think arm bumps the counter itself; the build arm bumps
/// once here. The check-then-bump is not atomic, so concurrent rituals can
/// overshoot the cap by their own count, which still bounds runaway fan-out.
pub fn dispatch_with(
    landed: &[LandResult],
    cwd: Option<&str>,
    seams: &Seams,
    echo: &mut dyn FnMut(&str),
) -> Vec<FollowupResult> {
    let mut capped = false;
    let mut results = Vec::new();
    for r in landed {
        let Some(node_id) = r.node_id.as_deref().filter(|n| !n.is_empty()) else {
            continue;
        };
        let arm = classify_followup(&r.candidate);
        let outcome = if arm == ARM_FILE {
            OUTCOME_FILED
        } else if seams.cap > 0 && (seams.count)() >= seams.cap {
            capped = true;
            OUTCOME_CAPPED
        } else {
            let ok = if arm == ARM_THINK {
                (seams.think)(node_id, cwd)
            } else {
                let ok = (seams.build)(node_id, cwd);
                if ok {
                    (seams.bump)();
                }
                ok
            };
            if ok {
                OUTCOME_DISPATCHED
            } else {
                OUTCOME_FAILED
            }
        };
        results.push(FollowupResult {
            node_id: node_id.to_string(),
            arm: arm.to_string(),
            outcome: outcome.to_string(),
        });
    }
    if capped {
        echo(&format!(
            "keep-going: daily dispatch cap reached ({}); parking follow-ups as backlog nodes",
            seams.cap
        ));
    }
    results
}

/// `dispatch_with` over the real counter and spawners.
pub fn dispatch_followups(
    landed: &[LandResult],
    project_root: Option<&Path>,
    cwd: Option<&str>,
) -> Vec<FollowupResult> {
    let seams = Seams {
        count: &crate::think_daily::count,
        bump: &crate::think_daily::bump,
        cap: crate::think_daily::daily_cap(project_root),
        think: &dispatch_think,
        build: &spawn_target_worker,
    };
    dispatch_with(landed, cwd, &seams, &mut |line| println!("{line}"))
}
