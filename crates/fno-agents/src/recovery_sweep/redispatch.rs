//! Stop one worker and respawn `/target` on an already selected route, in the
//! same worktree. The order is the invariant: stop, then release the node claim,
//! then spawn. `stop` does not free the claim, so a spawn without the release
//! refuses at `target init`. A release after a failed stop is an admin override
//! over a worker that may still be live, and would put two workers on one node.

use super::{node_id_from_worktree, Candidate};
use crate::naming::{bridge_name, parse_dispatch_agent_name};

const SHORT_TIMEOUT_S: u64 = 30;
const SPAWN_TIMEOUT_S: u64 = 60;

/// One finished native command. `None` from [`RedispatchSeams::run`] is a
/// launch failure or a timeout: the outcome is unknown, so nothing runs after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// What a redispatch reaches outside. `argv[0]` is `fno` or `fno-agents`.
pub trait RedispatchSeams {
    fn run(
        &self,
        argv: &[String],
        cwd: &str,
        timeout_s: u64,
        env: &[(&str, &str)],
    ) -> Option<RunResult>;
    fn node_is_done(&self, node: &str) -> bool;
    /// Best-effort: keep the predecessor's name addressable on the successor.
    fn alias_predecessor(&self, new_name: &str, old_name: Option<&str>);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedispatchOutcome {
    Respawned,
    /// A pre-spawn refusal, a failed spawn or an unknown command result.
    NotStarted,
    /// Gave up before the spawn, with the named reason.
    Failed(&'static str),
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// The `rec-<command>-<node>-<short>` name; the command comes from the
/// predecessor, else `t`.
fn recovery_agent_name(predecessor: Option<&str>, node: &str, short: &str) -> Option<String> {
    let parsed = parse_dispatch_agent_name(predecessor);
    let command = parsed.as_ref().map_or("t", |p| p.command.as_str());
    bridge_name(
        None,
        node,
        Some(short),
        None,
        None,
        Some("rec"),
        Some(command),
        None,
    )
    .ok()
}

/// The pane form the daemon names when it refuses `stop` on a pane row.
fn named_pane_kill(text: &str) -> Option<(String, String)> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"fno mux pane kill (\S+):(\d+)").unwrap());
    let found = re.captures(text)?;
    Some((found[1].to_string(), found[2].to_string()))
}

/// Free a dead lane's slot after a respawn that did not start; a live dispatch
/// slot would keep the node unselectable until its TTL. Best-effort.
fn release_lane_slot(seams: &dyn RedispatchSeams, node: &str, cwd: &str) {
    let command = argv(&["fno-agents", "claim", "lane-release", "--lane", node]);
    match seams.run(&command, cwd, SHORT_TIMEOUT_S, &[]) {
        Some(done) if done.code == 0 => {}
        Some(done) => eprintln!(
            "recovery: lane-release failed for {node} (exit {}); slot lingers to TTL",
            done.code
        ),
        None => eprintln!("recovery: lane-release failed for {node}"),
    }
}

/// Stop, release, then spawn. With no `flags` the spawn is a claude bg worker on
/// the active record; `flags` carries one chain link's axis bundle, which is how
/// a refused node reaches another vendor. `pre_spawn` runs after the worker
/// stopped and before the spawn reads credentials.
pub fn redispatch(
    candidate: &Candidate,
    seams: &dyn RedispatchSeams,
    pre_spawn: Option<&dyn Fn() -> bool>,
    flags: Option<&[String]>,
) -> RedispatchOutcome {
    use RedispatchOutcome::{Failed, NotStarted, Respawned};
    let Some(cwd) = candidate.cwd.as_deref().filter(|c| !c.is_empty()) else {
        return Failed("cwd-missing");
    };
    let Some(node) = node_id_from_worktree(cwd) else {
        return Failed("node-missing");
    };
    if seams.node_is_done(&node) {
        return Failed("node-done");
    }
    let name = candidate.name.as_deref().filter(|n| !n.is_empty());
    let Some(agent) = recovery_agent_name(name, &node, &candidate.short_id) else {
        return Failed("name-unmintable");
    };

    if let Some(name) = name {
        let stop = argv(&["fno", "agents", "stop", name]);
        let Some(stopped) = seams.run(&stop, cwd, SHORT_TIMEOUT_S, &[]) else {
            return NotStarted;
        };
        if stopped.code != 0 {
            let said = format!("{}{}", stopped.stdout, stopped.stderr);
            let Some((server, pane)) = named_pane_kill(&said) else {
                return Failed("stop-failed");
            };
            let kill = argv(&["fno", "mux", "pane", "kill", "--server", &server, &pane]);
            let Some(killed) = seams.run(&kill, cwd, SHORT_TIMEOUT_S, &[]) else {
                return NotStarted;
            };
            if killed.code != 0 {
                return Failed("stop-failed");
            }
        }
    }

    let claim = format!("node:{node}");
    let reason = format!("failover respawn {}", candidate.short_id);
    let release = argv(&[
        "fno", "agents", "claim", "release", &claim, "--force", "-R", &reason,
    ]);
    let Some(released) = seams.run(&release, cwd, SHORT_TIMEOUT_S, &[]) else {
        return NotStarted;
    };
    if released.code != 0 {
        return Failed("claim-held");
    }

    if pre_spawn.is_some_and(|ready| !ready()) {
        release_lane_slot(seams, &node, cwd);
        return NotStarted;
    }

    let mut spawn = argv(&["fno", "agents", "spawn"]);
    match flags.filter(|f| !f.is_empty()) {
        Some(axis) => spawn.extend_from_slice(axis),
        None => spawn.extend(argv(&["--harness", "claude", "--substrate", "bg"])),
    }
    spawn.extend(argv(&["--cwd", cwd, "--name", &agent]));
    spawn.push(format!("/target --no-merge {node}"));
    // The env carrier backs the flag: a replacement that drops the flag after
    // compaction still folds the refusal at init.
    let env = [("TARGET_NO_MERGE", "1")];
    let Some(spawned) = seams.run(&spawn, cwd, SPAWN_TIMEOUT_S, &env) else {
        return NotStarted;
    };
    if spawned.code != 0 {
        release_lane_slot(seams, &node, cwd);
        return NotStarted;
    }
    seams.alias_predecessor(&agent, name);
    Respawned
}
