//! The `config route` family's one leg: the data and the decisions for the
//! `config route` reads, plus the write-planning answers the front's
//! `set`/`unset` consume.
//!
//! Disposition: the family port's two-part disposition the repo proved on
//! the retro and delivery ports: the reads and renders live here
//! (in-process composition of the doors the Python leg drove by subprocess:
//! spawn-overlay providers/claude-route, route-slot inventory/states/policy),
//! and the WRITES stay in the front binary reusing the config-write monopoly
//! (`fno config set`, the validated writer with the file lock and the
//! scope resolution). Rendering lives here because preserve_order
//! serialization and the byte-equality goldens belong with the data.

pub mod env;
pub mod inventory;
pub mod plan_write;
pub mod settings;
pub mod table;

use serde_json::{json, Map, Value};
use std::path::Path;

// The routing-table constants, ported verbatim from
// `fno.agents.model_routing` (the values are a render contract the goldens
// pin).
pub(crate) const DEFAULT_SECONDARY_MODEL: &str = "glm-5.3";
pub(crate) const DEFAULT_ROUTED_ROLES: [&str; 5] =
    ["coordinate", "tidy", "orient", "consolidate", "post-merge"];
pub(crate) const KNOWN_LANE_ROLES: [&str; 1] = ["build"];
pub(crate) const PROTECTED_ROLES: [&str; 2] = ["implement", "review-verdict"];
pub(crate) const AUTO_ASSIGNED_BY: [(&str, &str); 3] = [
    ("post-merge", "pr_watch post-merge dispatch"),
    ("codex-verify", "codex verify lane"),
    ("build", "/target bg + fno backlog advance"),
];
pub(crate) const PROTECTED_ROLE_NOTE: [(&str, &str); 2] = [
    ("implement", "protected name; delivery routes via 'build'"),
    (
        "review-verdict",
        "protected name; no dispatch surface declares it",
    ),
];
pub(crate) const PROTECTED_ROLE_HINT: [(&str, &str); 2] = [
    (
        "implement",
        "delivery spawns route via the 'build' lane (fno config route set build <provider/model>)",
    ),
    (
        "review-verdict",
        "nothing declares this role; a verdict runs on the model of the session that reviews, so keep the reviewer off the routed authoring worker",
    ),
];

pub(crate) fn protected_note(role: &str) -> &'static str {
    PROTECTED_ROLE_NOTE
        .iter()
        .find(|(r, _)| *r == role)
        .map(|(_, note)| *note)
        .unwrap_or("protected (hard guard)")
}

pub(crate) fn protected_hint(role: &str) -> &'static str {
    PROTECTED_ROLE_HINT
        .iter()
        .find(|(r, _)| *r == role)
        .map(|(_, hint)| *hint)
        .unwrap_or_default()
}

/// The routing ops' entry behind route-slot's `config-route` mode: the front
/// shim sends `{mode: config-route, argv: [...]}` and the answer rides back
/// as (exit, stdout, stderr), nothing printed. Exit 0 on an answer, 2 on
/// usage.
pub fn run_capture(args: &[String]) -> (i32, String, String) {
    let rest: Vec<String> = args[1..].to_vec();
    match args.first().map(String::as_str) {
        Some("table") => table::run_capture(),
        Some("env") => env::run_capture(rest.first().map(String::as_str).unwrap_or("")),
        Some("inventory") => {
            let json = rest.iter().any(|a| a == "--json" || a == "-J");
            inventory::run_capture(json)
        }
        Some("settings") => settings::run_capture(&rest),
        Some("plan-write") => plan_write::run_capture(&rest),
        _ => (
            2,
            String::new(),
            "config-route: usage: <table|env|inventory|settings|plan-write> [...]\n".into(),
        ),
    }
}

/// The `model_routing` block, deep-merged across the config tiers the way
/// the Python loader folds them. Unset reads as the empty block.
pub(crate) fn config_block(cwd: &Path) -> Value {
    crate::agents_config::config_value_deep(cwd, &["model_routing"])
        .map(|block| serde_json::to_value(&block).expect("TOML serializes"))
        .unwrap_or_else(|| json!({}))
}

/// Built-in routed roles (-> the default provider+model) overlaid with the
/// config roles map (per-role override wins). Mirror of
/// `fno.agents.model_routing._effective_roles`.
pub(crate) fn effective_roles(block: &Value) -> serde_json::Map<String, Value> {
    let mut eff = serde_json::Map::new();
    for r in DEFAULT_ROUTED_ROLES {
        eff.insert(
            r.to_string(),
            json!(format!("zai,{}", DEFAULT_SECONDARY_MODEL)),
        );
    }
    if let Some(roles) = block.get("roles").and_then(Value::as_object) {
        for (k, v) in roles {
            eff.insert(
                k.trim().to_lowercase(),
                json!(v.as_str().unwrap_or_default()),
            );
        }
    }
    eff
}

/// Parse a `provider/model` (or legacy `provider,model`) target. Mirror of
/// `fno.agents.model_routing._parse_target`: comma wins as the separator
/// (it never appears inside a model id), otherwise the FIRST slash splits
/// (a namespaced model id survives); internal whitespace is rejected.
pub(crate) fn parse_target(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    let (provider, model) = if raw.contains(',') {
        let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
        if parts.len() != 2 {
            return None;
        }
        (parts[0], parts[1])
    } else {
        let (provider, found, model) = partition(raw, '/');
        if !found {
            return None;
        }
        (provider.trim(), model.trim())
    };
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    if provider.chars().any(char::is_whitespace) || model.chars().any(char::is_whitespace) {
        return None;
    }
    Some((provider.to_lowercase(), model.to_string()))
}

fn partition(text: &str, sep: char) -> (&str, bool, &str) {
    match text.split_once(sep) {
        Some((a, b)) => (a, true, b),
        None => (text, false, ""),
    }
}

/// One shell-quoted word (mirror of `shlex.quote`): a safe token passes
/// bare; anything else single-quotes with the embedded-quote escape.
pub(crate) fn quote_shell(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_%+=:,./-@".contains(c));
    if safe {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_splits_first_slash_and_rejects_whitespace() {
        assert_eq!(
            parse_target("zai/glm-5.3[1m]"),
            Some(("zai".into(), "glm-5.3[1m]".into()))
        );
        assert_eq!(
            parse_target("zai/z-ai/glm-5.3"),
            Some(("zai".into(), "z-ai/glm-5.3".into()))
        );
        assert_eq!(
            parse_target("zai,glm-5.3"),
            Some(("zai".into(), "glm-5.3".into()))
        );
        assert_eq!(parse_target("zai/"), None);
        assert_eq!(parse_target("no separator"), None);
        assert_eq!(parse_target("z ai/m"), None);
    }

    #[test]
    fn roles_overlay_config_over_builtins() {
        let block = json!({"roles": {"BUILD": "zai/glm-5.2", "tidy": "zai/glm-4.6"}});
        let eff = effective_roles(&block);
        assert_eq!(eff["build"], "zai/glm-5.2");
        assert_eq!(eff["tidy"], "zai/glm-4.6");
        assert_eq!(eff["coordinate"], "zai,glm-5.3");
    }

    #[test]
    fn quote_matches_shlex() {
        assert_eq!(quote_shell("zk-golden"), "zk-golden");
        assert_eq!(quote_shell("a b"), "'a b'");
        assert_eq!(quote_shell("it's"), "'it'\\''s'");
    }
}
