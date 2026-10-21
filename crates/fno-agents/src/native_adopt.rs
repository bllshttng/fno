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
    if crate::session_report::find_row(&entries, harness, session_id, agent_self).is_some() {
        return None;
    }
    let native = mint_flavor(record.is_some(), agent_self.is_some());
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
        .unwrap_or_else(|| crate::claude_adopt::synthesized_entry_name(session_id, &short));
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
                    Some(r) => Lineage::captured((
                        Some(r.launcher_session.clone()),
                        r.launcher_harness.clone(),
                        r.launcher_cwd.clone(),
                    )),
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
