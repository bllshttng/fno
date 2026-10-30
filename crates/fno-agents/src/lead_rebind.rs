//! `org init --rebind-from`: move a live role to a new harness session id.
//!
//! A lead spawned as a pane and then backgrounded into the claude agent view
//! continues under a NEW session id with a new transcript, while its role row
//! and scope manifest keep the old one. The SessionStart register hook never
//! restamps a promoted row in place (it conservatively branches; backgrounding
//! drops `FNO_AGENT_SELF`, so the restamp key is gone), and `org init --force`
//! re-arms by a fresh rewrite that re-mints fno_id/created_at and drops
//! `role_grantor`, term and shape. This is the deliberate repair door: one
//! flag on the existing init action (law d-fe66560a bars a new action), the
//! operator names the move explicitly, and every other manifest and row field
//! survives byte-for-byte.

use crate::client_commands::py_repr_str;
use crate::paths::AgentsHome;
use serde_json::json;
use std::path::{Path, PathBuf};
/// `org init --rebind-from OLD`: move the role named by the scope's existing
/// manifest from its current session to `new_session`. Exit 0 printed the
/// receipt; exit 2 refused with the reason on stderr and nothing was written.
pub fn run(scope: &str, new_session: &str, from: Option<&str>) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match rebind(&cwd, scope, new_session, from) {
        Ok(receipt) => {
            for line in receipt {
                println!("{line}");
            }
            0
        }
        Err(message) => {
            eprintln!("lead: {message}");
            2
        }
    }
}

/// One rebinding, on injected paths: refuse or move. The manifest rewrite
/// runs first (its expect-session guard is the authority check) and the
/// registry move second; the error on a registry failure names the split
/// state, because the next run must not guess.
pub(crate) fn rebind(
    cwd: &Path,
    scope: &str,
    new_session: &str,
    from: Option<&str>,
) -> Result<Vec<String>, String> {
    if !crate::lead_state::full_session_uuid(new_session) {
        return Err(format!(
            "--harness-session-id needs a full 36-char session uuid, got {}: the stop hook \
             matches the manifest id against the transcript basename, and a short id matches \
             no transcript, so the gate would arm dead.",
            py_repr_str(new_session)
        ));
    }
    let root = crate::paths::space_dir(cwd);
    let manifest_path = crate::lead_state::manifest_path(&root, scope)?;
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let parsed = crate::lead_termination::parse_lead_manifest(&content)
        .ok_or_else(|| format!("manifest unreadable: {}", manifest_path.display()))?;
    let old = parsed
        .harness_session_id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("manifest names no session: {}", manifest_path.display()))?;
    if let Some(from) = from {
        if from != old {
            return Err(format!(
                "--rebind-from names {from}, but the manifest names {old}: the scope's \
                 session moved under you. Re-read with `fno agents org` before touching \
                 anything."
            ));
        }
    }
    let harness = parsed
        .harness
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "claude".to_string());
    let home = AgentsHome::from_env();
    let folded = preflight_registry(&home.registry_json(), &harness, &old, new_session)?;
    crate::lead_state::set_manifest_fields(
        &root,
        scope,
        &[("harness_session_id", new_session)],
        Some(&old),
    )?;
    move_registry_row(&home, &harness, &old, new_session, &folded).map_err(|e| {
        format!(
            "registry move failed after the manifest rewrite: {e}; the manifest now \
             names {new_session} but the row still names {old}"
        )
    })?;
    let events = crate::events::EventEmitter::new(home.events_jsonl(), "agents");
    let _ = events.emit(
        "session_id_restamped",
        &json!({
            "scope": scope,
            "harness": harness,
            "predecessor": old,
            "successor": new_session,
            "manifest": manifest_path.display().to_string(),
            "cause": "org_init_rebind_from",
        }),
    );
    let mut lines = vec![
        "lead: role rebound".to_string(),
        format!("  scope: {scope}"),
        format!("  manifest: {}", manifest_path.display()),
        format!("  session: {old} -> {new_session}"),
    ];
    for name in &folded {
        lines.push(format!("  folded stray row: {name}"));
    }
    Ok(lines)
}

/// Preflight the registry BEFORE any write: exactly one row must name the
/// old session and hold the role, and no roled stray may already sit on the
/// new session. Returns the retiring (foldable stray) names.
fn preflight_registry(
    path: &Path,
    harness: &str,
    old: &str,
    new_session: &str,
) -> Result<Vec<String>, String> {
    let reg = crate::state::load_registry(path).map_err(|e| format!("registry unreadable: {e}"))?;
    let holders: Vec<&crate::state::RegistryEntry> = reg
        .entries
        .iter()
        .filter(|e| {
            e.harness.as_deref() == Some(harness) && e.harness_session_id.as_deref() == Some(old)
        })
        .collect();
    match holders[..] {
        [] => Err(
            "no registry row names the old session; the role row is gone or already moved"
                .to_string(),
        ),
        [one] => {
            if one.role_scope.as_deref().is_none_or(|s| s.is_empty()) {
                return Err(format!(
                    "the row on the old session ({}) holds no role; nothing to move",
                    one.name
                ));
            }
            let strays = stray_names_in(&reg.entries, harness, new_session);
            for name in &strays {
                let roled = reg
                    .entries
                    .iter()
                    .find(|e| &e.name == name)
                    .and_then(|e| e.role_scope.as_deref())
                    .is_some_and(|s| !s.is_empty());
                if roled {
                    return Err(format!(
                        "session {new_session} already has a roled row ({name})"
                    ));
                }
            }
            Ok(strays)
        }
        _ => Err(format!(
            "{} rows name the old session; rebinding an ambiguous role is refused",
            holders.len()
        )),
    }
}

/// Move the one role row from `old` to `new_session` under the registry
/// write lock. Folds stray un-roled rows already sitting on `new_session`
/// (the fresh row registering the backgrounded session mis-attributes the
/// live session once the role row lands); refuses a stray that holds a role.
fn move_registry_row(
    home: &AgentsHome,
    harness: &str,
    old: &str,
    new_session: &str,
    retiring: &[String],
) -> Result<(), String> {
    let path = home.registry_json();
    let retire: std::collections::BTreeSet<String> = retiring.iter().cloned().collect();
    crate::state::update_registry_retiring(&path, &retire, |reg| {
        let mut holder = Vec::new();
        for (i, e) in reg.entries.iter().enumerate() {
            if e.harness.as_deref() == Some(harness) && e.harness_session_id.as_deref() == Some(old)
            {
                holder.push(i);
            }
        }
        let holder_idx = match holder[..] {
            [i] => i,
            _ => return Err("the row set changed under the lock; rerun the rebind".to_string()),
        };
        {
            let row = &mut reg.entries[holder_idx];
            if !row.predecessor_session_ids.iter().any(|p| p == old) {
                row.predecessor_session_ids.push(old.to_string());
            }
            row.harness_session_id = Some(new_session.to_string());
            if harness == "claude" && row.mux.is_none() {
                let lead = new_session
                    .split('-')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let stale = old.split('-').next().unwrap_or("").to_ascii_lowercase();
                if row.short_id.is_empty() || row.short_id == stale {
                    row.short_id = lead;
                }
            }
        }
        for name in retiring {
            if let Some(pos) = reg.entries.iter().position(|e| &e.name == name) {
                reg.entries.remove(pos);
            }
        }
        Ok(())
    })
    .map_err(|e| e.to_string())
}

fn stray_names_in(
    entries: &[crate::state::RegistryEntry],
    harness: &str,
    new_session: &str,
) -> Vec<String> {
    entries
        .iter()
        .filter(|e| {
            e.harness.as_deref() == Some(harness)
                && e.harness_session_id.as_deref() == Some(new_session)
                && e.role_scope.as_deref().is_none_or(|s| s.is_empty())
        })
        .map(|e| e.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OLD: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    const NEW: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    const SCOPE: &str = "fnh,fno";

    /// Pins FNO_AGENTS_HOME on a tempdir and removes FNO_SPACES_DIR, the
    /// claims.rs EnvGuard pattern; restores both on drop.
    struct EnvGuard {
        prev_home: Option<std::ffi::OsString>,
        prev_spaces: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn new(root: &Path) -> Self {
            let prev_home = std::env::var_os("FNO_AGENTS_HOME");
            let prev_spaces = std::env::var_os("FNO_SPACES_DIR");
            unsafe {
                std::env::set_var("FNO_AGENTS_HOME", root.join("home"));
                std::env::remove_var("FNO_SPACES_DIR");
            }
            Self {
                prev_home,
                prev_spaces,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.prev_home.take() {
                    Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
                    None => std::env::remove_var("FNO_AGENTS_HOME"),
                }
                match self.prev_spaces.take() {
                    Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                    None => std::env::remove_var("FNO_SPACES_DIR"),
                }
            }
        }
    }

    /// A manifest + a roled row on OLD, optionally a stray un-roled row on
    /// NEW: the shape the backgrounding bug leaves behind. Returns the repo
    /// cwd to pass rebind.
    fn seed(root: &Path, stray: bool) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let leads = crate::paths::space_dir(&repo).join("leads");
        std::fs::create_dir_all(&leads).unwrap();
        let manifest = format!(
            "---\nfno_id: lead-x\ncreated_at: 2026-10-24T08:55:00Z\nscope: {SCOPE}\nshape: team\n\
             harness: claude\nharness_session_id: {OLD}\nmodel: \nowner_cwd: /tmp\n\
             budget_max_iterations: 40\nrespawn_count: 2\nrespawn_ceiling: 4\nwake_times: \n\
             role_level: 2\nrole_scope: {SCOPE}\nrole_grantor: human\n---\n"
        );
        std::fs::write(leads.join(format!("{SCOPE}.md")), manifest).unwrap();
        let mut agents = vec![json!({
            "name": "jasper", "cwd": "/tmp", "created_at": "2026-10-01T00:00:00Z",
            "status": "exited", "harness": "claude", "harness_session_id": OLD,
            "role_level": 2, "role_scope": SCOPE, "role_grantor": "human",
        })];
        if stray {
            agents.push(json!({
                "name": "bbbbbbbb", "cwd": "/tmp", "created_at": "2026-10-01T00:00:00Z",
                "status": "live", "harness": "claude", "harness_session_id": NEW,
            }));
        }
        crate::registry_store::seed_raw(
            &home.join("registry.json"),
            json!({"schema_version": crate::state::REGISTRY_SCHEMA_VERSION, "agents": agents})
                .to_string(),
        );
        repo
    }

    #[test]
    fn rebind_moves_the_row_and_manifest_and_folds_the_stray() {
        let td = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::new(td.path());
        let repo = seed(td.path(), true);
        let before = std::fs::read_to_string(
            crate::paths::space_dir(&repo)
                .join("leads")
                .join(format!("{SCOPE}.md")),
        )
        .unwrap();
        let lines = rebind(&repo, SCOPE, NEW, None).unwrap();
        assert!(lines.iter().any(|l| l.contains("rebound")), "{lines:?}");
        let home = AgentsHome::from_env();
        let reg = crate::state::load_registry(&home.registry_json()).unwrap();
        assert_eq!(reg.entries.len(), 1, "stray folded");
        let row = &reg.entries[0];
        assert_eq!(row.name, "jasper");
        assert_eq!(row.harness_session_id.as_deref(), Some(NEW));
        assert!(row.predecessor_session_ids.iter().any(|p| p == OLD));
        assert_eq!(row.role_scope.as_deref(), Some(SCOPE));
        assert_eq!(row.short_id, "bbbbbbbb");
        let after = std::fs::read_to_string(
            crate::paths::space_dir(&repo)
                .join("leads")
                .join(format!("{SCOPE}.md")),
        )
        .unwrap();
        let (b, a): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
        assert_eq!(b.len(), a.len());
        let mut changed = 0;
        for (x, y) in b.iter().zip(a.iter()) {
            if x != y {
                assert!(x.starts_with("harness_session_id: "), "{x}");
                changed += 1;
            }
        }
        assert_eq!(changed, 1, "exactly the session line moved");
    }

    #[test]
    fn rebind_refuses_before_any_write() {
        // A short new id refuses before any IO.
        let err = rebind(Path::new("/nonexistent"), SCOPE, "abc", None).unwrap_err();
        assert!(err.contains("uuid"), "{err}");
        let td = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::new(td.path());
        let repo = seed(td.path(), true);
        // A --rebind-from that does not match the manifest refuses.
        let err = rebind(
            &repo,
            SCOPE,
            NEW,
            Some("cccccccc-cccc-cccc-cccc-cccccccccccc"),
        )
        .unwrap_err();
        assert!(err.contains("moved under you"), "{err}");
        // A scope with no manifest refuses.
        let err = rebind(&repo, "nope", NEW, None).unwrap_err();
        assert!(
            err.contains("no manifest") || err.contains("cannot read"),
            "{err}"
        );
        // The state is untouched: the stray still sits on NEW.
        let home = AgentsHome::from_env();
        let reg = crate::state::load_registry(&home.registry_json()).unwrap();
        assert_eq!(reg.entries.len(), 2);
        let after = std::fs::read_to_string(
            crate::paths::space_dir(&repo)
                .join("leads")
                .join(format!("{SCOPE}.md")),
        )
        .unwrap();
        assert!(after.contains(&format!("harness_session_id: {OLD}")));
    }

    #[test]
    fn rebind_refuses_when_no_row_names_the_old_session() {
        let td = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::new(td.path());
        let repo = seed(td.path(), false);
        let home = AgentsHome::from_env();
        let path = home.registry_json();
        // Drop the roled row: the registry holds only... nothing. An empty
        // roster means the preflight finds no holder.
        crate::registry_store::seed_raw(
            &path,
            json!({"schema_version": crate::state::REGISTRY_SCHEMA_VERSION, "agents": []})
                .to_string(),
        );
        let err = rebind(&repo, SCOPE, NEW, None).unwrap_err();
        assert!(
            err.contains("no registry row names the old session"),
            "{err}"
        );
        // The manifest is untouched (the preflight refuses before any write).
        let after = std::fs::read_to_string(
            crate::paths::space_dir(&repo)
                .join("leads")
                .join(format!("{SCOPE}.md")),
        )
        .unwrap();
        assert!(after.contains(&format!("harness_session_id: {OLD}")));
    }
}
