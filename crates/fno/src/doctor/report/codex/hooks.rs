//! The codex user-level hook-layer inspection, ported from
//! `fno/setup/cli_hooks.py::inspect_codex_hooks` (read-only, no mutation).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Map, Value};

use super::super::codex_home;

const HOOK_SUFFIX: &str = "hooks/session-start.sh";

fn is_footnote_codex_command(command: &str) -> bool {
    if !command.contains(HOOK_SUFFIX) {
        return false;
    }
    command.contains("FNO_PLATFORM=codex") || command.contains("/footnote/hooks/session-start.sh")
}

/// Every `(group index, hook index, command)` SessionStart record, or the
/// shape error that stopped the walk.
fn session_start_records(
    data: &Value,
    source: &str,
) -> Result<Vec<(usize, usize, String)>, String> {
    let Some(obj) = data.as_object() else {
        return Err(format!("{source} root is not an object"));
    };
    let hooks = obj.get("hooks");
    let Some(hooks) = hooks
        .filter(|h| !h.is_null())
        .unwrap_or(&json!({}))
        .as_object()
    else {
        return Err(format!("{source} `hooks` is not an object"));
    };
    let groups = hooks.get("SessionStart");
    let Some(groups) = groups
        .filter(|g| !g.is_null())
        .unwrap_or(&json!([]))
        .as_array()
    else {
        return Err(format!("{source} `hooks.SessionStart` is not an array"));
    };
    let mut records = Vec::new();
    for (group_index, group) in groups.iter().enumerate() {
        let Some(group) = group.as_object() else {
            return Err(format!("{source} SessionStart group is not an object"));
        };
        let entries = group.get("hooks");
        let Some(entries) = entries
            .filter(|e| !e.is_null())
            .unwrap_or(&json!([]))
            .as_array()
        else {
            return Err(format!("{source} SessionStart `hooks` is not an array"));
        };
        for (hook_index, hook) in entries.iter().enumerate() {
            let Some(hook) = hook.as_object() else {
                return Err(format!("{source} SessionStart hook is not an object"));
            };
            let command = hook.get("command");
            if let Some(command) = command {
                let Some(command) = command.as_str() else {
                    return Err(format!("{source} SessionStart command is not a string"));
                };
                if !command.is_empty() {
                    records.push((group_index, hook_index, command.to_string()));
                }
            }
        }
    }
    Ok(records)
}

/// Commands from every hook event array, excluding trust state, or the shape
/// error that stopped the walk.
fn all_hook_commands(data: &Value, source: &str) -> Result<Vec<String>, String> {
    let Some(obj) = data.as_object() else {
        return Err(format!("{source} root is not an object"));
    };
    let hooks = obj.get("hooks");
    let Some(hooks) = hooks
        .filter(|h| !h.is_null())
        .unwrap_or(&json!({}))
        .as_object()
    else {
        return Err(format!("{source} `hooks` is not an object"));
    };
    let mut commands = Vec::new();
    for (event, groups) in hooks {
        if event == "state" || groups.is_null() {
            continue;
        }
        let Some(groups) = groups.as_array() else {
            return Err(format!("{source} `hooks.{event}` is not an array"));
        };
        for group in groups {
            let Some(group) = group.as_object() else {
                return Err(format!("{source} {event} group is not an object"));
            };
            let entries = group.get("hooks");
            let Some(entries) = entries
                .filter(|e| !e.is_null())
                .unwrap_or(&json!([]))
                .as_array()
            else {
                return Err(format!("{source} {event} `hooks` is not an array"));
            };
            for hook in entries {
                let Some(hook) = hook.as_object() else {
                    return Err(format!("{source} {event} hook is not an object"));
                };
                if let Some(command) = hook.get("command") {
                    let Some(command) = command.as_str() else {
                        return Err(format!("{source} {event} command is not a string"));
                    };
                    if !command.is_empty() {
                        commands.push(command.to_string());
                    }
                }
            }
        }
    }
    Ok(commands)
}

/// The trust-state keys for footnote SessionStart hooks plus whether each is
/// recorded in `hooks.state` (cli_hooks.py:299-317).
fn trust_entries(
    data: &Value,
    config_path: &Path,
    records: &[(usize, usize, String)],
) -> (Vec<String>, Vec<bool>) {
    let mut keys = Vec::new();
    let mut recorded = Vec::new();
    let hooks_state = data
        .get("hooks")
        .and_then(|h| h.get("state"))
        .filter(|s| !s.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    for (group_index, hook_index, command) in records {
        if !is_footnote_codex_command(command) {
            continue;
        }
        keys.push(format!(
            "{}:session_start:{}:{}",
            absolute(config_path),
            group_index,
            hook_index
        ));
        recorded.push(
            hooks_state
                .get(&keys.last().expect("just pushed"))
                .is_some(),
        );
    }
    (keys, recorded)
}

fn absolute(path: &Path) -> String {
    if path.is_absolute() {
        path.to_string_lossy().into_owned()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }
}

/// `fno doctor --codex-hooks [--json]`: inspect the codex hook layers
/// (doctor.py:3136-3181). `--json` false renders through
/// `emit_codex_hooks_report`.
pub(crate) fn codex_hooks_report() -> Value {
    let config_path = codex_home().join("config.toml");
    let hooks_json_path = codex_home().join("hooks.json");
    let mut toml_session: Vec<String> = Vec::new();
    let mut toml_commands: Vec<String> = Vec::new();
    let mut toml_verified = false;
    let mut trust_map: Map<String, Value> = Map::new();
    let mut json_session: Vec<String> = Vec::new();
    let mut json_commands: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    if config_path.exists() {
        match parse_toml_layer(&config_path) {
            Ok((records, commands, keys, recorded)) => {
                toml_session = records.iter().map(|(_, _, c)| c.clone()).collect();
                toml_commands = commands;
                toml_verified = false;
                for (k, r) in keys.into_iter().zip(recorded) {
                    let verdict = if r { "recorded-unverified" } else { "missing" };
                    trust_map.insert(k, json!(verdict));
                }
            }
            Err(e) => {
                errors.push(e);
            }
        }
    }
    if hooks_json_path.exists() {
        match parse_json_layer(&hooks_json_path) {
            Ok((session, commands)) => {
                json_session = session;
                json_commands = commands;
            }
            Err(e) => {
                errors.push(e);
            }
        }
    }
    let state = if !errors.is_empty() {
        "malformed"
    } else if !toml_commands.is_empty() && !json_commands.is_empty() {
        "both"
    } else if !toml_commands.is_empty() {
        "toml-only"
    } else if !json_commands.is_empty() {
        "json-only"
    } else {
        "neither"
    };
    let toml_footnote: Vec<String> = toml_session
        .iter()
        .filter(|c| is_footnote_codex_command(c))
        .cloned()
        .collect();
    let json_footnote: Vec<String> = json_session
        .iter()
        .filter(|c| is_footnote_codex_command(c))
        .cloned()
        .collect();
    let toml_foreign: Vec<Value> = toml_commands
        .iter()
        .filter(|c| !toml_footnote.contains(c))
        .cloned()
        .map(Value::String)
        .collect();
    let json_foreign: Vec<Value> = json_commands
        .iter()
        .filter(|c| !json_footnote.contains(c))
        .cloned()
        .map(Value::String)
        .collect();
    let has_json_hooks = !json_commands.is_empty();
    let status = if !errors.is_empty() {
        "error"
    } else if toml_footnote.is_empty() || !toml_verified || has_json_hooks {
        "warn"
    } else {
        "ok"
    };
    json!({
        "status": status,
        "preferred_layer": "config.toml",
        "state": state,
        "config_path": config_path.to_string_lossy(),
        "hooks_json_path": hooks_json_path.to_string_lossy(),
        "footnote_toml_wired": !toml_footnote.is_empty(),
        "footnote_toml_trust_verified": toml_verified,
        "footnote_toml_trust": trust_map,
        "duplicate_layers": !toml_commands.is_empty() && has_json_hooks,
        "footnote_json_hooks": json_footnote,
        "foreign_json_hooks": json_foreign,
        "errors": errors,
    })
}

fn parse_toml_layer(
    path: &Path,
) -> Result<
    (
        Vec<(usize, usize, String)>,
        Vec<String>,
        Vec<String>,
        Vec<bool>,
    ),
    String,
> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc: toml::Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let data = serde_json::to_value(&doc).map_err(|e| format!("{}: {e}", path.display()))?;
    let records = session_start_records(&data, "config.toml")?;
    let commands = all_hook_commands(&data, "config.toml")?;
    let (keys, recorded) = trust_entries(&data, path, &records);
    Ok((records, commands, keys, recorded))
}

fn parse_json_layer(path: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let data: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let session = session_start_records(&data, "hooks.json")?
        .into_iter()
        .map(|(_, _, c)| c)
        .collect();
    let commands = all_hook_commands(&data, "hooks.json")?;
    Ok((session, commands))
}
