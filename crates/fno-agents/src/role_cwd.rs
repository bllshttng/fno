//! A titled lead's role commands run against the role's project, never the
//! shell's cwd. A lead whose shell sat in a job tmp dir journaled its beats
//! into the wrong space, read zero open PRs against seventeen, and nothing
//! refused. Every role command calls [`rebind`] before its first read, so
//! each `current_dir()` reader below it agrees with the role.

use std::path::{Path, PathBuf};

use crate::paths::{canonical_repo_root, worktree_repo_root, AgentsHome};
use crate::state::RegistryEntry;

/// What a role command does with its cwd.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// No titled row for this session, or the cwd already sits in the role's
    /// space: run where the shell is.
    Stay,
    /// The cwd is in another space: run from the role's project root.
    Move(PathBuf),
    /// The role's root is gone: refuse, naming both.
    Refuse(String),
}

fn space_key(path: &Path) -> PathBuf {
    canonical_repo_root(path).unwrap_or_else(|| worktree_repo_root(path))
}

/// The pure decision over one registry snapshot.
pub fn decide(command: &str, session_id: &str, entries: &[RegistryEntry], cwd: &Path) -> Decision {
    let row = entries.iter().find(|e| {
        e.role_scope.as_deref().is_some_and(|s| !s.is_empty())
            && (e.session_id.as_deref() == Some(session_id)
                || e.harness_session_id.as_deref() == Some(session_id))
    });
    let Some(row) = row else {
        return Decision::Stay;
    };
    let root = if row.project_root.is_empty() {
        row.cwd.as_str()
    } else {
        row.project_root.as_str()
    };
    if root.is_empty() {
        return Decision::Stay;
    }
    let root = PathBuf::from(root);
    let scope = row.role_scope.as_deref().unwrap_or_default();
    if !root.is_dir() {
        // A stale root (a pruned worktree the row fell back to) must not lock
        // out a lead whose shell already sits in a checkout.
        if worktree_repo_root(cwd).join(".git").exists() {
            return Decision::Stay;
        }
        return Decision::Refuse(format!(
            "{command}: cwd {} is outside role {scope}'s project {}, and that project does not exist; fix the role row's project_root",
            cwd.display(),
            root.display()
        ));
    }
    if space_key(cwd) == space_key(&root) {
        return Decision::Stay;
    }
    Decision::Move(root)
}

/// Resolve this session's role and move the process into its project when
/// the shell sits elsewhere. `Err` carries the refusal text.
pub fn rebind(command: &str) -> Result<(), String> {
    let Some((session_id, _)) = crate::mail_cli::send::own_identity() else {
        return Ok(());
    };
    let Ok(registry) = crate::state::load_registry(&AgentsHome::from_env().registry_json()) else {
        return Ok(());
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match decide(command, &session_id, &registry.entries, &cwd) {
        Decision::Stay => Ok(()),
        Decision::Refuse(message) => Err(message),
        Decision::Move(root) => {
            std::env::set_current_dir(&root).map_err(|e| {
                format!(
                    "{command}: cannot enter role project {}: {e}",
                    root.display()
                )
            })?;
            eprintln!(
                "{command}: cwd {} is outside the role's space; running from {}",
                cwd.display(),
                root.display()
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(session: &str, scope: Option<&str>, root: &Path) -> RegistryEntry {
        serde_json::from_value(serde_json::json!({
            "name": "lead",
            "harness_session_id": session,
            "role_scope": scope,
            "project_root": root.display().to_string(),
            "cwd": root.display().to_string(),
        }))
        .unwrap()
    }

    #[test]
    fn titled_lead_outside_its_space_moves_untitled_stays_missing_root_refuses() {
        let base = tempfile::tempdir().unwrap();
        let project = base.path().join("project");
        let elsewhere = base.path().join("jobs-tmp");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();

        let titled = [row("sess-lead", Some("fnh,fno"), &project)];
        assert_eq!(
            decide("checkin", "sess-lead", &titled, &elsewhere),
            Decision::Move(project.clone())
        );
        assert_eq!(
            decide("checkin", "sess-lead", &titled, &project),
            Decision::Stay
        );

        let untitled = [row("sess-lead", None, &project)];
        assert_eq!(
            decide("checkin", "sess-lead", &untitled, &elsewhere),
            Decision::Stay
        );

        let gone = [row("sess-lead", Some("fnh,fno"), &base.path().join("gone"))];
        match decide("checkin", "sess-lead", &gone, &elsewhere) {
            Decision::Refuse(message) => {
                assert!(
                    message.contains("jobs-tmp") && message.contains("gone"),
                    "{message}"
                )
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(
            decide("checkin", "sess-lead", &gone, &project),
            Decision::Stay
        );
    }
}
