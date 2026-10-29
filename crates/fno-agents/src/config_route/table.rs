//! The routing table read: the `fno config route ls` fold, one row per
//! role. Mirror of `fno.agents.model_routing.build_route_table` over the
//! spawn-overlay providers answer (the key column is the one Rust owner's).

use super::*;
use serde_json::Value;
use std::path::Path;

/// The answer: one row per role as JSON rows. The front shim renders text
/// or json from it.
pub(crate) fn run_capture() -> (i32, String, String) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let rows = table_json(&cwd);
    (
        0,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into())
        ),
        String::new(),
    )
}

/// The fold itself, exposed for the parity test.
pub(crate) fn table_json(cwd: &Path) -> Value {
    let block = config_block(cwd);
    // The same providers answer the Python leg consumed, now in-process.
    let providers = crate::spawn_overlay::resolve(json!({
        "kind": "providers",
        "cwd": cwd.to_string_lossy(),
    }))
    .ok()
    .and_then(|a| a.get("providers").cloned())
    .unwrap_or_else(|| json!({}));
    let config_roles: Vec<String> = block
        .get("roles")
        .and_then(Value::as_object)
        .map(|m| m.keys().map(|k| k.trim().to_lowercase()).collect())
        .unwrap_or_default();
    let mut config_sorted = config_roles.clone();
    config_sorted.sort();
    let mut names: Vec<String> = Vec::new();
    for r in DEFAULT_ROUTED_ROLES
        .into_iter()
        .chain(KNOWN_LANE_ROLES)
        .chain(config_sorted.iter().map(String::as_str))
        .chain(PROTECTED_ROLES)
    {
        let r = r.to_string();
        if !names.contains(&r) {
            names.push(r);
        }
    }
    let eff = effective_roles(&block);
    let mut rows: Vec<Value> = Vec::new();
    for role in &names {
        let source = if config_roles.contains(role) {
            "config"
        } else if DEFAULT_ROUTED_ROLES.contains(&role.as_str()) {
            "built-in"
        } else {
            "known lane"
        };
        rows.push(table_row(role, &eff, &providers, source));
    }
    json!(rows)
}

/// One row of the fold. Mirror of `build_route_table`'s per-role branch:
/// protected rows render the guard; an unconfigured role degrades its
/// cells; a known provider renders its protocol and key status.
fn table_row(role: &str, eff: &Map<String, Value>, providers: &Value, source: &str) -> Value {
    let provenance = |source: &str| -> String {
        let auto = AUTO_ASSIGNED_BY
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, by)| *by)
            .unwrap_or("manual only");
        format!("{auto} ({source})")
    };
    if PROTECTED_ROLES.contains(&role) {
        return json!({
            "role": role,
            "provider_model": "never routed (hard guard)",
            "protocol": "-",
            "key": "-",
            "assigned_by": protected_note(role),
        });
    }
    let raw = eff.get(role).and_then(Value::as_str).unwrap_or_default();
    if raw.is_empty() {
        return json!({
            "role": role,
            "provider_model": "unconfigured",
            "protocol": "-",
            "key": "-",
            "assigned_by": provenance("unconfigured"),
        });
    }
    let (pname, model) = match parse_target(raw) {
        Some(pair) => pair,
        None => {
            return json!({
                "role": role,
                "provider_model": format!("{raw} (malformed)"),
                "protocol": "-",
                "key": "-",
                "assigned_by": provenance(source),
            })
        }
    };
    let provider = providers
        .get(pname.as_str())
        .cloned()
        .unwrap_or(Value::Null);
    if provider.is_null() {
        return json!({
            "role": role,
            "provider_model": format!("{pname}/{model}"),
            "protocol": "unknown provider",
            "key": "-",
            "assigned_by": provenance(source),
        });
    }
    let protocol = provider
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or("anthropic")
        .to_lowercase();
    let key_answer = provider.get("key").cloned().unwrap_or_else(|| json!({}));
    let satisfying = key_answer.get("satisfying").and_then(Value::as_str);
    let checked = key_answer
        .get("checked")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<&str>>()
                .join(", ")
        })
        .unwrap_or_default();
    let key_status = match satisfying {
        Some(name) => format!("found via {name}"),
        None => {
            if checked.is_empty() {
                "MISSING (checked nothing)".to_string()
            } else {
                format!("MISSING (checked {checked})")
            }
        }
    };
    json!({
        "role": role,
        "provider_model": format!("{pname}/{model}"),
        "protocol": protocol,
        "key": key_status,
        "assigned_by": provenance(source),
    })
}
