//! The native adopt: the row a session report mints when no row holds the
//! session id. A launch-record child joins as the launcher's row (origin
//! `spawn`, the launcher triple as the parent edge, the record's node on the
//! row); a session with no record is the user's own (origin `operator`): it
//! mints only once the session joined fno (`join_marker`), counts toward the
//! cap as an occupant and is never refused, managed, mailed or reaped. A spawned row that went missing before
//! its first report mints back under its own `--agent-self` name (origin
//! `spawn`, lineage unproven). Transcript IO (title, model) runs in
//! [`prepare_mint`], OUTSIDE the registry write lock; the write closure
//! re-decides absence under the lock.

use crate::state::{Lineage, RegistryEntry};
use crate::AgentStatus;

/// Why this report mints: a launch record (or a lost spawned row) is fleet
/// work; a session with neither is the user's own.
fn mint_flavor(record: bool, agent_self: bool) -> bool {
    record || agent_self
}

/// D8: a native or drawn name that collides with a live row takes a numeric
/// suffix, the way claude numbers `name (2)`. An fno name is a CLI handle
/// (`[a-z][a-z0-9._-]{0,31}`), so the suffix is `-2`, `-3`, ... instead of a
/// parenthesised word.
pub(crate) fn unique_name(entries: &[RegistryEntry], base: &str) -> String {
    let live: Vec<&RegistryEntry> = entries
        .iter()
        .filter(|e| crate::spawn_gate::status_is_liveish(&e.status))
        .collect();
    if !live.iter().any(|e| e.name == base) {
        return base.to_string();
    }
    for n in 2u32.. {
        let candidate = format!("{base}-{n}");
        if !live.iter().any(|e| e.name == candidate) {
            return candidate;
        }
    }
    unreachable!("a u32 suffix space never exhausts")
}

/// The row a session report mints when no row holds the id. `None` when a row
/// already exists (the pre-read's answer; the write closure re-decides under
/// the lock, so a racing writer is caught there).
pub(crate) struct NativeMint {
    pub(crate) entry: RegistryEntry,
    pub(crate) native: bool,
}

/// The parent edge a claimed launch record proves: its launcher's triple.
pub(crate) fn launcher_lineage(r: &crate::launch_record::Record) -> Lineage {
    Lineage::captured((
        Some(r.launcher_session.clone()),
        r.launcher_harness.clone(),
        r.launcher_cwd.clone(),
    ))
}

pub(crate) fn prepare_mint(
    home: &crate::paths::AgentsHome,
    harness: &str,
    session_id: &str,
    record: Option<&crate::launch_record::Record>,
    child_name: Option<&str>,
    agent_self: Option<&str>,
    transcript_path: Option<&str>,
    source: Option<&str>,
    report_cwd: Option<&str>,
) -> Option<NativeMint> {
    // Plain read; the write closure re-decides absence under the lock.
    let entries = crate::daemon::load_registry_asserted(&home.registry_json())
        .ok()?
        .entries;
    if crate::session_report::find_row(&entries, harness, session_id, agent_self).is_some()
        || superseded(&entries, session_id)
    {
        return None;
    }
    let native = mint_flavor(record.is_some(), agent_self.is_some());
    // Only the fleet mint answers to a retirement. A person who resumes a
    // reaped session by hand gets the operator row, which no sweep reaps.
    if native && retired_session(home, harness, session_id, record) {
        return None;
    }
    // A hand session gets a row only once it joined fno (its first fno skill
    // or command), unless `agents.auto_register_sessions` opts every session in.
    if !native
        && !crate::join_marker::is_joined(home.root(), harness, session_id)
        && !auto_register_sessions(report_cwd)
    {
        return None;
    }
    let short = crate::client_commands::derived_short_id(session_id);
    let origin = if native { "spawn" } else { "operator" };
    // Name precedence: the record's name, then the row the worker lost, then
    // the child's own name, then the draw (transcript title, else t-short).
    let base_name = record
        .and_then(|r| r.launch.name.clone())
        .or_else(|| agent_self.map(str::to_string))
        .or_else(|| child_name.map(str::to_string))
        .unwrap_or_else(|| {
            let node_name = record.and_then(|r| {
                let node = r.launch.node.as_deref()?;
                crate::naming::node_worker_name(node, std::path::Path::new(&r.launch.cwd))
            });
            crate::claude_adopt::synthesized_entry_name(session_id, &short, node_name)
        });
    let (node, cwd, substrate) = match record {
        Some(r) => (
            r.launch.node.clone(),
            Some(r.launch.cwd.clone()),
            Some(crate::launch_record::native_substrate_word(
                &r.launch.harness,
            )),
        ),
        None => (None, report_cwd.map(str::to_string), None),
    };
    let mut entry = RegistryEntry {
        name: base_name,
        origin: Some(origin.to_string()),
        status: AgentStatus::Live,
        short_id: short,
        harness: Some(harness.to_string()),
        substrate: substrate.map(str::to_string),
        node,
        transcript_path: transcript_path.map(str::to_string),
        start_source: source.map(str::to_string),
        cwd: cwd.clone().unwrap_or_default(),
        project_root: cwd.unwrap_or_default(),
        claude_session_uuid: (harness == "claude").then(|| session_id.to_string()),
        codex_session_id: (harness == "codex").then(|| session_id.to_string()),
        ..RegistryEntry::new(
            Some(session_id.to_string()),
            if native {
                match record {
                    Some(r) => launcher_lineage(r),
                    None => Lineage::unproven(
                        "spawned row absent at first report; minted at the session report",
                    ),
                }
            } else {
                Lineage::unproven("hand-started session: no launch record named it")
            },
        )
    };
    // The record child is a lane-attributed fleet row: the claude transcript
    // states the model, and the provider comes only from the route-settings
    // match, exactly the roster adopt's closure. The operator mint carries no
    // lane claim at all (the unattributed warning skips operator rows).
    if record.is_some() && harness == "claude" {
        if let Some(model) = crate::claude_adopt::transcript_model(session_id) {
            entry.provider = crate::claude_adopt::provider_from_route_settings(Some(&model));
            entry.model = Some(model);
            entry.model_basis = Some("verified".to_string());
        }
    }
    Some(NativeMint { entry, native })
}

/// A retired session stays retired. A reap receipt, or an rm tombstone
/// still inside its grace window, names the session and no launch record
/// newer than that retirement relaunched it. The report then mints no fleet row:
/// a reaped worker's own Stop hook reports after the sweep, and minting
/// there put every reaped row straight back. `fno agents adopt` is the door
/// back for a reaped session; it reads the receipt on purpose.
pub(crate) fn retired_session(
    home: &crate::paths::AgentsHome,
    harness: &str,
    session_id: &str,
    record: Option<&crate::launch_record::Record>,
) -> bool {
    let receipt_path = crate::receipt::reap_receipt_path_for(home, harness, session_id);
    // An unreadable receipt still marks the session reaped; with no time to
    // compare, no launch record counts as newer.
    let reaped_at_ms = receipt_path.exists().then(|| {
        crate::receipt::read_reap_receipt(&receipt_path)
            .ok()
            .and_then(|r| crate::event_store::parse_rfc3339_ms(&r.reaped_at))
            .unwrap_or(i64::MAX)
    });
    let removed_at_ms =
        crate::rm_tombstone::lookup_removed_beside(&home.registry_json(), session_id)
            .filter(|stone| stone.session_id == session_id)
            .map(|stone| stone.removed_at.saturating_mul(1000));
    let Some(retired_at_ms) = reaped_at_ms.into_iter().chain(removed_at_ms).max() else {
        return false;
    };
    record.is_none_or(|r| r.launched_at_ms <= retired_at_ms)
}

/// The session a claude fork was copied from, read off its launch argv:
/// `--fork-session` with `--resume <parent>`. Claude's bg host writes that
/// argv when `claude --bg --resume` meets a session still held, and replays
/// it on every daemon respawn, so the copy keeps naming its parent.
pub(crate) fn fork_parent_in_argv(argv: &[String], session_id: &str) -> Option<String> {
    if !argv.iter().any(|a| a == "--fork-session") {
        return None;
    }
    argv.windows(2)
        .find(|pair| pair[0] == "--resume" && pair[1] != session_id)
        .map(|pair| pair[1].clone())
}

/// The fork parent of a live claude bg session and the fork's own pid, from
/// the argv of the pid the claude agents listing names for it.
pub(crate) fn claude_fork_parent(session_id: &str, pid: Option<u32>) -> Option<(String, u32)> {
    let pid = pid.or_else(|| {
        let short = crate::client_commands::derived_short_id(session_id);
        crate::claude_roster::read_all_agents_union().live_bg_pid(&short)
    })?;
    let parent = fork_parent_in_argv(&crate::census::process_argv(pid)?, session_id)?;
    Some((parent, pid))
}

/// A session a row has already moved past. It never mints a row of its own:
/// the row that holds its successor is its row.
pub(crate) fn superseded(entries: &[RegistryEntry], session_id: &str) -> bool {
    entries
        .iter()
        .any(|e| e.predecessor_session_ids.iter().any(|id| id == session_id))
}

/// Bind a fork to the row that holds its parent: the copy does the parent's
/// work, so it keeps that row and the parent id moves to the predecessors.
/// `None` when no row holds the parent, and the caller mints as before.
pub(crate) fn bind_fork_child(
    entries: &mut [RegistryEntry],
    parent: &str,
    child: &str,
    pid: Option<u32>,
) -> Option<usize> {
    let i = entries
        .iter()
        .position(|e| e.harness_session_id.as_deref() == Some(parent))?;
    let e = &mut entries[i];
    if !e.apply_succession(parent, child) {
        return None;
    }
    // The child's later reports find the row through this field.
    if e.harness_name() == "claude" {
        e.claude_session_uuid = Some(child.to_string());
    }
    if e.session_id.as_deref() == Some(parent) {
        e.session_id = Some(child.to_string());
    }
    e.short_id = crate::client_commands::derived_short_id(child);
    if pid.is_some() {
        e.pid = pid;
        e.pid_start_time = None;
    }
    Some(i)
}

fn auto_register_sessions(report_cwd: Option<&str>) -> bool {
    let cwd = report_cwd
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    crate::agents_config::config_lookup(&cwd, &["agents", "auto_register_sessions"])
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// AC9's rule in one helper: every key a spawned birth carries is present on
/// the native one, per event kind. The panic names the missing key, so the
/// negative test (AC10) reads as evidence, not noise. Test-only: the mint
/// path carries the invariant by construction, and the publish build would
/// read an unused runtime assert as dead code.
#[cfg(test)]
pub(crate) fn assert_birth_superset(native: &serde_json::Value, spawned: &serde_json::Value) {
    let (Some(n), Some(s)) = (native.as_object(), spawned.as_object()) else {
        panic!("birth payloads must be objects");
    };
    for key in s.keys() {
        assert!(
            n.contains_key(key),
            "native birth is missing the spawned key `{key}`"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARENT: &str = "3feca5b6-585e-479f-a88c-cb83bce0444e";
    const CHILD: &str = "8789fe29-0e28-4d8e-aa00-8171920ce205";

    /// A claude bg worker resumed while still held comes back as a fork: the
    /// argv claude 2.1.309 writes names the parent, the fork takes the
    /// parent's row, and the parent never mints a second one.
    #[test]
    fn a_forked_native_row_keeps_one_row() {
        let argv = |line: String| {
            line.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let forked = argv(format!(
            "/u/claude/versions/2.1.309 --session-id {CHILD} --fork-session --resume {PARENT}"
        ));
        assert_eq!(fork_parent_in_argv(&forked, CHILD).as_deref(), Some(PARENT));
        let plain = argv(format!("claude --resume {PARENT}"));
        assert_eq!(fork_parent_in_argv(&plain, PARENT), None);

        let mut worker =
            RegistryEntry::new(Some(PARENT.to_string()), Lineage::unproven("test fixture"));
        worker.name = "fno-books-eval5".into();
        worker.harness = Some("claude".into());
        worker.node = Some("fb-e47c".into());
        worker.short_id = "3feca5b6".into();
        worker.claude_session_uuid = Some(PARENT.into());
        let mut entries = vec![worker];

        assert_eq!(
            bind_fork_child(&mut entries, PARENT, CHILD, Some(1600)),
            Some(0)
        );
        assert_eq!(entries.len(), 1);
        let row = &entries[0];
        assert_eq!(row.name, "fno-books-eval5");
        assert_eq!(row.node.as_deref(), Some("fb-e47c"));
        assert_eq!(row.harness_session_id.as_deref(), Some(CHILD));
        assert_eq!(row.claude_session_uuid.as_deref(), Some(CHILD));
        assert_eq!(row.short_id, "8789fe29");
        assert_eq!(row.pid, Some(1600));
        assert_eq!(row.predecessor_session_ids, vec![PARENT.to_string()]);
        assert!(crate::session_report::find_row(&entries, "claude", CHILD, None).is_some());
        assert!(crate::session_report::find_row(&entries, "claude", PARENT, None).is_none());
        assert!(superseded(&entries, PARENT));
        assert!(!superseded(&entries, CHILD));

        // A fork of a session no row holds binds nothing: the caller mints.
        assert_eq!(
            bind_fork_child(&mut entries, "unheld-parent", "c-2", None),
            None
        );
    }
}
