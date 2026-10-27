//! The drain-hook wiring read of the dead-letter collector
//! (doctor.py:3517-3562).

use std::path::PathBuf;

use serde_json::Value;

use super::{read_json_object, resolve_source};

fn plugin_hooks_json() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("CLAUDE_PLUGIN_ROOT") {
        let p = PathBuf::from(root).join("hooks/hooks.json");
        if p.exists() {
            return Some(p);
        }
    }
    let src = resolve_source(None)?;
    let p = src.join("hooks/hooks.json");
    p.exists().then_some(p)
}

pub(crate) fn drain_hook_wired() -> Option<bool> {
    let path = plugin_hooks_json()?;
    let map = read_json_object(&path)?;
    let mut cmds: Vec<String> = Vec::new();
    let starts = map
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for group in starts {
        for hook in group
            .get("hooks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Some(cmd) = hook.get("command").and_then(Value::as_str) {
                cmds.push(cmd.to_string());
            }
        }
    }
    // Producers moved into hooks/context-hooks.json; their argv strings
    // name the same scripts.
    if let Some(parent) = path.parent() {
        let ctx = parent.join("context-hooks.json");
        if let Some(decl) = read_json_object(&ctx) {
            if let Some(groups) = decl.get("groups").and_then(Value::as_object) {
                for (_name, group) in groups {
                    for producer in group
                        .get("producers")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                    {
                        let argv: Vec<String> = producer
                            .get("argv")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(|t| t.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        cmds.push(argv.join(" "));
                    }
                }
            }
        }
    }
    Some(
        cmds.iter()
            .any(|c| c.contains("mail-drain") || c.contains("drain-self")),
    )
}
