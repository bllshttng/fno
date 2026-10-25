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

/// `fno do think dispatch <node>`, in process. It bumps the shared counter
/// itself on a real spawn, so only a `spawned` decision counts as a
/// dispatch: `noop`, `skipped` and a refusal bump nothing.
fn dispatch_think(node_id: &str, cwd: Option<&str>) -> bool {
    crate::think_dispatch::dispatch_node(node_id, cwd).is_some_and(|r| r.decision == "spawned")
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
    let mut args = vec!["--harness".to_string(), "claude".to_string()];
    match cwd.filter(|d| !d.is_empty()) {
        Some(dir) => args.extend(["--cwd".to_string(), dir.to_string()]),
        None => args.push("--fresh".to_string()),
    }
    args.extend([
        "--name".to_string(),
        name,
        format!("/target --no-merge {node_id}"),
    ]);
    let env = std::collections::BTreeMap::from([("TARGET_NO_MERGE".to_string(), "1".to_string())]);
    let Ok(out) = crate::backlog::advance_dispatch::exec_agents_spawn(&args, &env, false) else {
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
