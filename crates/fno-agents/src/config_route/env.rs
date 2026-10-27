//! Eval-able interactive route environment; failures emit no shell commands.

use super::*;
use crate::client_commands::py_repr_str;

pub fn run_capture(spec: &str) -> (i32, String, String) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let block = config_block(&cwd);
    let name = spec.trim().to_lowercase();
    let explicit = spec.contains('/') || spec.contains(',');
    let mut notes = Vec::new();
    let legacy = effective_roles(&block)
        .get(&name)
        .and_then(Value::as_str)
        .and_then(parse_target);
    let target = if explicit {
        match parse_target(spec) {
            Some(target) => Some(target),
            None => {
                return failure(
                    2,
                    format!(
                        "malformed target {}; expected provider/model",
                        py_repr_str(spec)
                    ),
                )
            }
        }
    } else {
        let business = match crate::spawn_overlay::resolve(
            json!({"kind": "business-role", "role": spec.trim()}),
        ) {
            Ok(answer) => answer,
            Err(text) => return failure(1, text),
        };
        if let Some(reason) = business.get("reason").and_then(Value::as_str) {
            if reason != "not_found" {
                let id = business
                    .pointer("/role/id")
                    .and_then(Value::as_str)
                    .unwrap_or(spec);
                let detail = business
                    .get("detail")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(|v| format!(": {v}"))
                    .unwrap_or_default();
                return failure(
                    1,
                    format!(
                        "business role {} is blocked ({reason}){detail}; no worker launched",
                        py_repr_str(id)
                    ),
                );
            }
        }
        if PROTECTED_ROLES.contains(&name.as_str()) {
            notes.push(format!(
                "protected-role({name}) floor=high; never routed to a secondary provider"
            ));
            None
        } else if block.get("enabled").and_then(Value::as_bool) == Some(false) {
            None
        } else if business.get("reason").and_then(Value::as_str) == Some("not_found") {
            legacy.clone()
        } else if let Some(projection) = business.get("routing_projection").filter(|p| !p.is_null())
        {
            let provider = projection
                .get("provider")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .or_else(|| legacy.as_ref().map(|p| p.0.clone()));
            let model = projection
                .get("model")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .or_else(|| legacy.as_ref().map(|p| p.1.clone()));
            let missing = [("provider", provider.is_none()), ("model", model.is_none())]
                .into_iter()
                .filter_map(|(k, absent)| absent.then_some(k))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return failure(1, format!("resolved business role {} has an incomplete routing projection: missing {}; no configured legacy target can complete it; no worker launched", py_repr_str(&name), missing.join(" and ")));
            }
            provider.zip(model)
        } else {
            None
        }
    };
    let target_pname = if explicit {
        target.as_ref()
    } else {
        legacy.as_ref()
    }
    .map(|p| p.0.clone());
    if let Some((pname, model)) = target {
        let answer = match crate::spawn_overlay::resolve(json!({
            "kind": "claude-route", "provider": pname, "model": model,
            "cwd": cwd.to_string_lossy(), "ctx": {"kind": "peer", "name": pname},
        })) {
            Ok(answer) => answer,
            Err(text) => return failure(1, text),
        };
        notes.extend(
            answer
                .get("notices")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string),
        );
        if let Some(route) = answer
            .get("route")
            .and_then(Value::as_object)
            .filter(|r| !r.is_empty())
        {
            let mut keys: Vec<_> = route.keys().collect();
            keys.sort();
            let mut out = "unset ANTHROPIC_API_KEY\nunset CLAUDE_CODE_OAUTH_TOKEN\n".to_string();
            for key in keys {
                out.push_str(&format!(
                    "export {key}={}\n",
                    quote_shell(route[key].as_str().unwrap_or_default())
                ));
            }
            return (0, out, String::new());
        }
    }
    if let Some(pname) = target_pname {
        if let Ok(answer) = crate::spawn_overlay::resolve(
            json!({"kind": "providers", "cwd": cwd.to_string_lossy()}),
        ) {
            if let Some(key) = answer
                .get("providers")
                .and_then(|p| p.get(&pname))
                .and_then(|p| p.get("key"))
            {
                let checked = key
                    .get("checked")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>();
                if key
                    .get("satisfying")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .is_none()
                    && !checked.is_empty()
                {
                    notes.push(format!("missing API key (checked {})", checked.join(", ")));
                }
            }
        }
    }
    let reason = if notes.is_empty() {
        format!(
            "{} is not routed (unconfigured / protected / no key)",
            py_repr_str(spec)
        )
    } else {
        notes.join("; ")
    };
    failure(1, reason)
}

fn failure(code: i32, text: String) -> (i32, String, String) {
    (code, String::new(), format!("route env: {text}\n"))
}
