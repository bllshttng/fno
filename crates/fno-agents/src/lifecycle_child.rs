//! Children fno-agents spawns for lifecycle actions: the bounded
//! `claude stop`; and the all-source token heal, native now. One module
//! answers one question - how every such child runs (cwd pinning, bounds,
//! output contract) - so a dead daemon cwd cannot take them all down at once
//! (: a daemon lazy-started from a worktree the reaper later deleted
//! kept that deleted cwd forever, and every child that inherited it died at
//! getcwd, leaving finished workers unstopable and holding fleet slots).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::client_commands::{backfill_row_aliases, is_identity_token, py_repr_str};

/// The cwd a lifecycle child inherits: this process's cwd while getcwd still
/// resolves, else `fallback`, else the temp dir. These children need no
/// particular directory, only an existing one.
pub(crate) fn lifecycle_child_cwd_from(cwd: std::io::Result<PathBuf>, fallback: &Path) -> PathBuf {
    cwd.unwrap_or_else(|_| {
        if fallback.is_dir() {
            fallback.to_path_buf()
        } else {
            std::env::temp_dir()
        }
    })
}

pub(crate) fn lifecycle_child_cwd(fallback: &Path) -> PathBuf {
    lifecycle_child_cwd_from(std::env::current_dir(), fallback)
}

/// Run `claude stop <short>`, refusing to wait past `timeout`. `kill_on_drop`:
/// on timeout the `output()` future is dropped, so the hung child does not
/// keep running past the deadline this call gave up at (self-review finding:
/// this was hand-duplicated at the RPC call site in daemon.rs; one shared
/// helper now backs both).
pub(crate) async fn bounded_claude_stop(
    short: &str,
    timeout: Duration,
) -> Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed> {
    bounded_claude_stop_in(short, timeout, None).await
}

/// The same stop, pinned to one claude account root. A session born under an
/// isolated account is invisible to the ambient root, so a stop sent there
/// exits nonzero and leaves the session running.
pub(crate) async fn bounded_claude_stop_in(
    short: &str,
    timeout: Duration,
    config_dir: Option<&Path>,
) -> Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed> {
    let mut command = tokio::process::Command::new("claude");
    command
        .arg("stop")
        .arg(short)
        .current_dir(lifecycle_child_cwd(
            crate::paths::AgentsHome::from_env().root(),
        ))
        .kill_on_drop(true);
    if let Some(dir) = config_dir {
        command.env("CLAUDE_CONFIG_DIR", dir);
    }
    tokio::time::timeout(timeout, command.output()).await
}

fn helper_registry_path(registry_path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if registry_path.is_absolute() {
        registry_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(registry_path)
    };
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return Ok(canonical);
    }
    if let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) {
        if let Ok(canonical_parent) = fs::canonicalize(parent) {
            return Ok(canonical_parent.join(name));
        }
    }
    Ok(absolute)
}

/// Resolve `token` against the registry and every harness store as one
/// namespace, adopting one unique store-only session on a registry miss.
///
/// `Ok(Some(row))` on resolution, `Ok(None)` only on a clean miss, and
/// `Err(msg)` on ambiguity or unavailable coverage.
pub(crate) fn heal_token(token: &str, registry_path: &Path) -> Result<Option<Value>, String> {
    let registry_path = helper_registry_path(registry_path).map_err(|exc| {
        format!(
            "cannot safely resolve token {} because the registry path does not resolve: {exc}. Use the full session id.",
            py_repr_str(token)
        )
    })?;
    let healed = crate::store_heal::heal_all_sources(token, &registry_path)
        .map_err(|err| err.message().to_string())?;
    let Some(mut row) = healed else {
        return Ok(None);
    };
    finish_healed_row(token, &mut row)?;
    recover_onto_healed_row(
        &mut row,
        &registry_path,
        &crate::claude_ask::ClaudeHome::from_env().projects_dir(),
    );
    Ok(Some(row))
}

/// A healed row skipped `load_registry_entries`, so it gets that loader's
/// alias reconciliation here; a row with no well-formed harness refuses as
/// incomplete rather than failing three frames later.
fn finish_healed_row(token: &str, row: &mut Value) -> Result<(), String> {
    let incomplete = || {
        format!(
            "cannot safely resolve token {} because the healer returned an incomplete row. Use the full session id.",
            py_repr_str(token)
        )
    };
    let obj = row.as_object_mut().ok_or_else(incomplete)?;
    backfill_row_aliases(obj, false);
    if !is_identity_token(obj.get("harness").and_then(Value::as_str)) || !obj.contains_key("name") {
        return Err(incomplete());
    }
    Ok(())
}

/// Fill a freshly healed bare claude row with the route its transcript's
/// birth identity proves, so `fno agents adopt` records the route at the
/// heal door instead of at the first relaunch. Acts only on a claude row
/// that records no route and no launch account; every miss (no transcript,
/// an Anthropic birth, no usable route file, a failed write) leaves the row
/// exactly as the healer wrote it - a refusal here belongs to the relaunch,
/// not to adopt.
fn recover_onto_healed_row(row: &mut Value, registry_path: &Path, projects: &Path) {
    if row.get("harness").and_then(Value::as_str) != Some("claude") {
        return;
    }
    let non_empty = |k: &str| {
        row.get(k)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    };
    if non_empty("route_settings_path") || non_empty("launch_account") {
        return;
    }
    let Some(sid) = ["claude_session_uuid", "harness_session_id"]
        .iter()
        .find_map(|k| non_empty(k).then(|| row[k].as_str().unwrap_or_default().to_string()))
    else {
        return;
    };
    let pins = crate::resume_pin::RowPins::from_json(row);
    let Ok(Some(r)) = crate::route_recovery::recover(pins, &sid, projects) else {
        return;
    };
    if crate::route_recovery::persist(registry_path, &sid, &r).is_ok() {
        row["route_settings_path"] = Value::String(r.route_settings_path.clone());
        row["provider"] = Value::String(r.provider.clone());
        row["requested_model"] = Value::String(r.model.clone());
        row["launch_account"] = Value::String("default".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::test_env_lock;

    /// a dead cwd (getcwd errored) falls back to an existing fallback;
    /// a missing fallback degrades to the temp dir instead of handing the child
    /// another missing directory.
    #[test]
    fn lifecycle_child_cwd_falls_back_when_getcwd_fails() {
        let fallback = std::env::temp_dir().join("x8f73-fallback");
        std::fs::create_dir_all(&fallback).unwrap();
        assert_eq!(
            lifecycle_child_cwd_from(
                Err(std::io::Error::from_raw_os_error(libc::ENOENT)),
                &fallback
            ),
            fallback
        );
        std::fs::remove_dir_all(&fallback).ok();
        // The fallback itself is gone: temp dir, never a missing path.
        let missing = Path::new("/nonexistent/x8f73");
        assert_eq!(
            lifecycle_child_cwd_from(
                Err(std::io::Error::from_raw_os_error(libc::ENOENT)),
                missing
            ),
            std::env::temp_dir()
        );
    }

    #[test]
    fn a_healed_row_without_a_harness_refuses_as_incomplete() {
        let mut row = serde_json::json!({"name": "w1", "cwd": "/tmp"});
        let err = finish_healed_row("deadbeef", &mut row).unwrap_err();
        assert!(err.contains("incomplete row"), "{err}");
        let mut ok = serde_json::json!({"name": "w1", "harness": "codex"});
        assert!(finish_healed_row("deadbeef", &mut ok).is_ok());
    }

    #[test]
    fn healed_bare_glm_row_gets_recovered_route() {
        // AC4-EDGE: a heal of a glm-born bare row carries the recovered
        // provider, requested_model and route onto the returned row AND the
        // registry; an Anthropic-born heal writes nothing new.
        let _guard = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let routes = dir.path().join("routes");
        std::fs::create_dir_all(&routes).unwrap();
        std::fs::write(
            routes.join("zai.json"),
            serde_json::json!({"env": {
                "ANTHROPIC_MODEL": "glm-5.3-flash[1m]",
                "ANTHROPIC_BASE_URL": "https://repro.invalid/api/anthropic",
                "ANTHROPIC_AUTH_TOKEN": "secret-token",
                "FNO_ROUTE_PROVIDER": "zai",
            }})
            .to_string(),
        )
        .unwrap();
        std::env::set_var("FNO_ROUTE_SETTINGS_DIR", &routes);
        let projects = dir.path().join("projects");
        let slug = projects.join("staged-project");
        std::fs::create_dir_all(&slug).unwrap();
        let sid = "77770014-2222-3333-4444-555555555555";
        std::fs::write(
            slug.join(format!("{sid}.jsonl")),
            r#"{"type":"attachment","attachment":{"type":"model","identity":{"modelId":"glm-5.3-flash[1m]","marketingName":null}},"type":"attachment"}"#,
        )
        .unwrap();
        let reg = dir.path().join("registry.json");
        crate::state::update_registry(&reg, |r| {
            r.entries.push(crate::state::RegistryEntry {
                name: "wk-heal".into(),
                harness: Some("claude".into()),
                harness_session_id: Some(sid.into()),
                claude_session_uuid: Some(sid.into()),
                ..Default::default()
            });
        })
        .unwrap();
        let mut row = serde_json::json!({
            "name": "wk-heal",
            "cwd": dir.path().to_string_lossy(),
            "log_path": dir.path().join("log").to_string_lossy(),
            "harness": "claude",
            "claude_session_uuid": sid,
            "harness_session_id": sid,
        });
        recover_onto_healed_row(&mut row, &reg, &projects);
        std::env::remove_var("FNO_ROUTE_SETTINGS_DIR");
        assert_eq!(row["provider"], "zai");
        assert_eq!(row["requested_model"], "glm-5.3-flash[1m]");
        assert_eq!(row["launch_account"], "default");
        let stored = crate::state::load_registry(&reg).unwrap();
        let e = stored
            .entries
            .iter()
            .find(|e| e.harness_session_id.as_deref() == Some(sid))
            .unwrap();
        assert_eq!(e.provider.as_deref(), Some("zai"));
        assert_eq!(e.requested_model.as_deref(), Some("glm-5.3-flash[1m]"));
        assert_eq!(
            e.route_settings_path.as_deref().map(str::len) > Some(0),
            true
        );

        // An Anthropic-born transcript writes nothing new onto a bare row.
        let sid2 = "77770015-2222-3333-4444-555555555555";
        std::fs::write(
            slug.join(format!("{sid2}.jsonl")),
            r#"{"type":"attachment","attachment":{"type":"model","identity":{"modelId":"claude-opus-5","marketingName":"Opus 5"}},"type":"attachment"}"#,
        )
        .unwrap();
        let reg2 = dir.path().join("registry2.json");
        crate::state::update_registry(&reg2, |r| {
            r.entries.push(crate::state::RegistryEntry {
                name: "wk-heal2".into(),
                harness: Some("claude".into()),
                harness_session_id: Some(sid2.into()),
                ..Default::default()
            });
        })
        .unwrap();
        let mut row2 = serde_json::json!({
            "name": "wk-heal2",
            "cwd": dir.path().to_string_lossy(),
            "log_path": dir.path().join("log2").to_string_lossy(),
            "harness": "claude",
            "harness_session_id": sid2,
        });
        recover_onto_healed_row(&mut row2, &reg2, &projects);
        assert!(row2.get("provider").is_none());
        assert!(row2.get("route_settings_path").is_none());
    }
}
