//! The native lane exec: the launch kind behind the spawn-overlay action
//! (law d-fe66560a allows no new action). The ask names the lane's resolved
//! values plus the launch context; the kind writes the launch record first
//! (the dispatch reservation the gate counts - it carries the lane's
//! provider), fills the lane template, then execs the native argv and
//! streams its output. The row mints at the adopt: the session report
//! claims the record and names the row from it, so this path mints nothing
//! of its own.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The `{"kind": "launch"}` arm: the child streams to the verb's own stdio
/// and the answer carries its exit code, since the runner prints one JSON
/// line after the stream.
pub fn launch_kind(payload: &Value) -> Result<Value, String> {
    let code = exec_lane(payload)?;
    Ok(json!({ "exit": code }))
}

fn str_field(payload: &Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The lane's `--settings` file, written here from the RAW overlays so the
/// ask carries no pre-composed env and the compose has one owner. `None`
/// when the ask names no overlay at all.
fn write_lane_settings(
    payload: &Value,
    cwd: &Path,
    alias: Option<&str>,
) -> Result<Option<String>, String> {
    let route_env = payload
        .get("route_env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let account_env = payload
        .get("account_env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    crate::route_settings::write_composed(
        &route_env,
        &account_env,
        payload.get("sandbox").filter(|v| v.is_object()).cloned(),
        alias,
        cwd,
    )
}

fn exec_lane(payload: &Value) -> Result<i32, String> {
    let harness = str_field(payload, "harness");
    let name = str_field(payload, "name");
    let prompt = str_field(payload, "payload");
    let launcher_session = str_field(payload, "launcher_session");
    if harness.is_empty() || name.is_empty() {
        return Err("payload needs harness and name".into());
    }
    if prompt.is_empty() {
        return Err("payload needs a payload".into());
    }
    if launcher_session.is_empty() {
        // A record naming no parent proves nothing, so the door refuses
        // rather than launch ungated and unparented.
        return Err(
            "payload needs launcher_session: a record with no parent proves nothing".into(),
        );
    }
    let cwd = {
        let field = str_field(payload, "cwd");
        if field.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            PathBuf::from(field)
        }
    };

    // The lane self-resolves beside the inventory fold, so the ask carries
    // the resolved axes (provider, model), never a row projection.
    let lane = crate::route_gather::lane_for_axes(
        payload
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or(""),
        payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim(),
        &cwd,
    );
    let alias = lane
        .as_ref()
        .and_then(|row| row.get("name"))
        .and_then(Value::as_str);
    // The lane's settings file writes BEFORE the record: a launch whose
    // settings compose fails must not leave a pending record holding the
    // lane.
    let settings_path = write_lane_settings(payload, &cwd, alias)?;

    // The record first: between exec and the session report no registry row
    // exists, so the record is the seat the lane counts. An exec that fails
    // after the write leaves the record holding the lane until its window
    // lapses - the fail-safe direction, never the burst.
    let home = crate::paths::AgentsHome::from_env();
    crate::launch_record::write_record(
        home.root(),
        crate::launch_record::Launch {
            harness: harness.clone(),
            name: Some(name),
            node: payload
                .get("node")
                .and_then(Value::as_str)
                .map(str::to_string),
            slot: payload
                .get("slot")
                .and_then(Value::as_str)
                .map(str::to_string),
            provider: payload
                .get("provider")
                .and_then(Value::as_str)
                .map(str::to_string),
            cwd: cwd.to_string_lossy().into_owned(),
        },
        &launcher_session,
        payload.get("launcher_harness").and_then(Value::as_str),
        &str_field(payload, "launcher_cwd"),
        crate::claims::now_ms(),
    )
    .map_err(|error| format!("launch record: {error}"))?;

    let template = crate::spawn_compose::resolve_template(
        &harness,
        lane.as_ref().and_then(|row| row.get("launch")),
        None,
    )?;
    let mut values = crate::spawn_compose::fill_values(payload);
    values.insert("settings".to_string(), settings_path.unwrap_or_default());
    let tokens = crate::spawn_compose::fill_launch(&template, &values)?;
    if tokens.is_empty() {
        return Err("the filled argv is empty".into());
    }
    let mut command = Command::new(&tokens[0]);
    command.args(&tokens[1..]).current_dir(&cwd);
    // A separate supervisor per config dir: the account's own env rides the
    // child's process env, where the daemon fork keeps it; the rest of the
    // account overlay rides the settings file.
    let config_dir = payload
        .get("account_env")
        .and_then(|account| account.get("CLAUDE_CONFIG_DIR"))
        .and_then(Value::as_str)
        .filter(|dir| !dir.is_empty());
    if let Some(dir) = config_dir {
        command.env("CLAUDE_CONFIG_DIR", dir);
    }
    let status = command
        .status()
        .map_err(|error| format!("exec {}: {error}", tokens[0]))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.fno/config.toml` carrying the one row the tests' provider/model
    /// axes resolve: its `launch` keeps the tests off the real claude.
    fn stage_row(dir: &Path) {
        std::fs::create_dir_all(dir.join(".fno")).unwrap();
        std::fs::write(
            dir.join(".fno/config.toml"),
            "[[routing.models]]\nname = \"t-lane\"\nharness = \"claude\"\nmodel = \"tm-1\"\nroute = \"zai/tm-1\"\nlaunch = [\"sh\", \"-c\", \"exit 0\"]\n",
        )
        .unwrap();
    }

    /// The door's full path: the record lands pending (with the provider, so
    /// the gate counts it), the fill runs, and the child's exit code comes
    /// back. The lane resolves from a staged config row, so the test never
    /// touches real claude.
    #[test]
    fn execs_the_native_line_and_writes_the_record() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("FNO_AGENTS_HOME", dir.path().join("agents-home"));
        stage_row(dir.path());
        let out = exec_lane(&serde_json::json!({
            "harness": "claude", "name": "w1",
            "payload": "/fno:target x-aaaa",
            "launcher_session": "lead-1", "launcher_harness": "claude",
            "provider": "zai", "model": "tm-1", "node": "x-aaaa", "cwd": dir.path(),
        }))
        .unwrap();
        assert_eq!(out, 0);
        let home = crate::paths::AgentsHome::at(dir.path().join("agents-home"));
        let pending =
            crate::launch_record::pending_for_provider(home.root(), "zai", crate::claims::now_ms());
        assert_eq!(pending, vec!["w1".to_string()], "the record holds the lane");
        std::env::remove_var("FNO_AGENTS_HOME");
    }

    /// A record with no parent proves nothing: the door refuses before the
    /// record and before the exec.
    #[test]
    fn refuses_a_record_with_no_parent() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("FNO_AGENTS_HOME", dir.path().join("agents-home"));
        let error = exec_lane(&serde_json::json!({
            "harness": "claude", "name": "w1",
            "payload": "/fno:target x-aaaa",
        }))
        .unwrap_err();
        assert!(error.contains("launcher_session"), "{error}");
        let home = crate::paths::AgentsHome::at(dir.path().join("agents-home"));
        assert!(crate::launch_record::pending_for_provider(
            home.root(),
            "zai",
            crate::claims::now_ms()
        )
        .is_empty());
        std::env::remove_var("FNO_AGENTS_HOME");
    }

    /// The child's own failure surfaces as its exit code: the launch may
    /// have happened, so the record stays and the window lapses it. The lane
    /// resolves from a staged config row; the child's exit still wins.
    #[test]
    fn a_failing_child_returns_its_code() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("FNO_AGENTS_HOME", dir.path().join("agents-home"));
        stage_row(dir.path());
        let out = exec_lane(&serde_json::json!({
            "harness": "claude", "name": "w2",
            "payload": "/x",
            "launcher_session": "lead-1",
            "provider": "zai", "model": "tm-1", "cwd": dir.path(),
        }))
        .unwrap();
        assert_eq!(out, 3);
        std::env::remove_var("FNO_AGENTS_HOME");
    }
}
