//! The `inbox.peers` and `inbox.surface_patterns` readers: which peer owns
//! which surfaces, which project a peer serves, and which globs make up each
//! surface. One walk and one parse feed both the `--to-project` send resolver
//! and `fno inbox peers`, the read the blueprint and pre-promise skill steps
//! run.

use crate::operator_turns::python_json;
use serde_json::{Map, Value};
use std::path::Path;

/// The three maps, in config order, plus the drop warnings the read raised.
#[derive(Debug, Default)]
pub struct InboxPeers {
    pub surfaces: Map<String, Value>,
    pub projects: Map<String, Value>,
    pub surface_patterns: Map<String, Value>,
    pub warnings: Vec<String>,
}

/// Read the first `.fno` config at or above `start`. config.toml parses as
/// TOML and may wrap its keys in `config`. A legacy settings.yaml parses as
/// YAML and carries the block under `config.inbox` only. A missing or
/// malformed block reads as no peers.
pub fn read(start: &Path) -> InboxPeers {
    let mut out = InboxPeers::default();
    let Some(inbox) = inbox_block(start, &mut out.warnings) else {
        return out;
    };
    if let Some(peers) = inbox.get("peers").and_then(Value::as_object) {
        for (name, block) in peers {
            let Some(block) = block.as_object() else {
                out.warnings.push(format!(
                    "inbox.settings: peer {} entry is not a mapping; dropping (got {})",
                    crate::client_commands::py_repr_str(name),
                    crate::fallback_chain::type_name(block)
                ));
                continue;
            };
            match block.get("surfaces") {
                Some(Value::Array(items)) => {
                    out.surfaces.insert(name.clone(), py_str_list(items));
                }
                None | Some(Value::Null) => {}
                Some(other) => out.warnings.push(format!(
                    "inbox.settings: peer {} surfaces is not a list; dropping (got {})",
                    crate::client_commands::py_repr_str(name),
                    crate::fallback_chain::type_name(other)
                )),
            }
            if let Some(project) = block.get("project").and_then(Value::as_str) {
                if !project.is_empty() {
                    out.projects.insert(name.clone(), Value::from(project));
                }
            }
        }
    }
    if let Some(patterns) = inbox.get("surface_patterns").and_then(Value::as_object) {
        for (name, globs) in patterns {
            match globs {
                Value::Array(items) => {
                    out.surface_patterns
                        .insert(name.clone(), py_str_list(items));
                }
                other => out.warnings.push(format!(
                    "inbox.settings: surface_patterns[{}] is not a list; dropping (got {})",
                    crate::client_commands::py_repr_str(name),
                    crate::fallback_chain::type_name(other)
                )),
            }
        }
    }
    out
}

fn inbox_block(start: &Path, warnings: &mut Vec<String>) -> Option<Map<String, Value>> {
    let mut search = start.to_path_buf();
    loop {
        let fno = search.join(".fno");
        let toml_file = fno.join("config.toml");
        if toml_file.is_file() {
            // Straight into serde_json keeps the document's key order.
            let doc: Value = std::fs::read_to_string(&toml_file)
                .ok()
                .and_then(|text| toml::from_str(&text).ok())?;
            let inbox = doc
                .get("config")
                .and_then(|c| c.get("inbox"))
                .or_else(|| doc.get("inbox"));
            return inbox.and_then(Value::as_object).cloned();
        }
        let yaml_file = fno.join("settings.yaml");
        if yaml_file.is_file() {
            let text = match std::fs::read_to_string(&yaml_file) {
                Ok(text) => text,
                Err(e) => {
                    warnings.push(format!(
                        "inbox.settings: cannot read {}: {e}",
                        yaml_file.display()
                    ));
                    return None;
                }
            };
            let doc: Value = match serde_yaml_ng::from_str(&text) {
                Ok(doc) => doc,
                Err(e) => {
                    warnings.push(format!(
                        "inbox.settings: malformed YAML in {}: {e}",
                        yaml_file.display()
                    ));
                    return None;
                }
            };
            return doc
                .get("config")
                .and_then(|c| c.get("inbox"))
                .and_then(Value::as_object)
                .cloned();
        }
        if !search.pop() {
            return None;
        }
    }
}

/// Python `[str(s) for s in items]` for the scalar shapes a config holds.
fn py_str_list(items: &[Value]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| {
                Value::from(match item {
                    Value::String(s) => s.clone(),
                    Value::Bool(true) => "True".to_string(),
                    Value::Bool(false) => "False".to_string(),
                    Value::Null => "None".to_string(),
                    other => other.to_string(),
                })
            })
            .collect(),
    )
}

/// `fno-agents compaction inbox-peers [--json]`, the native answer behind
/// `fno inbox peers`.
pub fn run(args: &[String]) -> i32 {
    let mut json_out = false;
    for arg in args {
        match arg.as_str() {
            "--json" | "-J" => json_out = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return 0;
            }
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let peers = read(&cwd);
    for warning in &peers.warnings {
        eprintln!("{warning}");
    }
    if json_out {
        let payload = serde_json::json!({
            "peers": peers.surfaces,
            "projects": peers.projects,
            "surface_patterns": peers.surface_patterns,
        });
        println!("{}", python_json(&payload));
        return 0;
    }
    if peers.surfaces.is_empty() && peers.projects.is_empty() && peers.surface_patterns.is_empty() {
        println!("no inbox peers configured");
        return 0;
    }
    let mut names: Vec<&String> = peers.surfaces.keys().collect();
    names.extend(
        peers
            .projects
            .keys()
            .filter(|n| !peers.surfaces.contains_key(*n)),
    );
    for name in names {
        let surfaces = peers.surfaces.get(name).map(joined).unwrap_or_default();
        let project = peers
            .projects
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("-");
        println!("peer {name}\tsurfaces: {surfaces}\tproject: {project}");
    }
    for (surface, globs) in &peers.surface_patterns {
        println!("surface {surface}\t{}", joined(globs));
    }
    0
}

const USAGE: &str = "usage: fno inbox peers [--json]";

fn joined(list: &Value) -> String {
    list.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peer named under inbox.peers with a project serves it from any cwd;
    /// a block with no project adds nothing. The nearest config wins outright:
    /// an outer config's peers never merge in.
    #[test]
    fn the_inbox_peer_hint_adds_a_project_from_the_nearest_config() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".fno")).unwrap();
        std::fs::write(
            root.path().join(".fno/config.toml"),
            "[inbox.peers.gamma]\nproject = \"outer\"\n",
        )
        .unwrap();
        let inner = root.path().join("repo");
        std::fs::create_dir_all(inner.join(".fno")).unwrap();
        std::fs::write(
            inner.join(".fno/config.toml"),
            "[inbox.peers.alpha]\nproject = \"web\"\n\n[inbox.peers.beta]\nsurfaces = [\"api\"]\n",
        )
        .unwrap();
        let nested = inner.join("sub/dir");
        std::fs::create_dir_all(&nested).unwrap();
        let hints = read(&nested).projects;
        assert_eq!(hints.get("alpha").and_then(Value::as_str), Some("web"));
        assert!(!hints.contains_key("beta"));
        assert!(!hints.contains_key("gamma"));
    }
}
