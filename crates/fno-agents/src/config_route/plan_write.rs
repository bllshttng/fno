//! Scope-local route write plans. The front applies them through config set/unset.

use super::*;
use crate::client_commands::py_repr_str;
use std::path::PathBuf;

pub fn run_capture(args: &[String]) -> (i32, String, String) {
    let args = crate::client_commands::expand_eq(args);
    match plan(&args) {
        Ok(answer) => (0, format!("{answer}\n"), String::new()),
        Err((code, text)) => (code, String::new(), text),
    }
}

fn plan(args: &[String]) -> Result<Value, (i32, String)> {
    let op = args.first().map(String::as_str).unwrap_or("");
    let mut scope = "global";
    let mut words = Vec::new();
    for arg in &args[args.len().min(1)..] {
        match arg.as_str() {
            "--local" | "-l" => scope = "project",
            "--global" | "-g" => scope = "global",
            arg if arg.starts_with('-') => {
                return Err((2, format!("route {op}: unknown option {arg}\n")))
            }
            _ => words.push(arg.as_str()),
        }
    }
    let target = scope_path(scope)?;
    if op == "init" && words.is_empty() {
        return Ok(json!({"action": "init", "path": target, "scope": scope}));
    }
    if !matches!(op, "set" | "unset") || words.len() != if op == "set" { 2 } else { 1 } {
        return Err((
            2,
            "route: usage: set ROLE PROVIDER/MODEL | unset ROLE | init [--local|--global]\n".into(),
        ));
    }
    let name = words[0].trim().to_lowercase();
    let mut warning = String::new();
    let normalized = if op == "set" {
        if PROTECTED_ROLES.contains(&name.as_str()) {
            let hint = protected_hint(&name);
            return Err((2, format!("error: {} is a protected role (never routable via the roles table); refusing. Config unchanged.{}\n", py_repr_str(&name), if hint.is_empty() { String::new() } else { format!("\nhint: {hint}.") })));
        }
        let (pname, model) = parse_target(words[1]).ok_or_else(|| (2, format!("error: provider/model must be 'provider/model' with a non-empty model token; got {}. Config unchanged.\n", py_repr_str(words[1]))))?;
        let cwd = std::env::current_dir().unwrap_or_default();
        let answer = crate::spawn_overlay::resolve(
            json!({"kind": "providers", "cwd": cwd.to_string_lossy()}),
        )
        .map_err(|e| (1, format!("error: {e}\n")))?;
        let providers = answer.get("providers").and_then(Value::as_object);
        let provider = providers.and_then(|p| p.get(&pname)).ok_or_else(|| {
            let mut known = providers
                .map(|p| p.keys().map(String::as_str).collect::<Vec<_>>())
                .unwrap_or_default();
            known.sort();
            (
                2,
                format!(
                    "error: unknown provider {}; known: {}. Config unchanged.\n",
                    py_repr_str(&pname),
                    known.join(", ")
                ),
            )
        })?;
        let protocol = provider
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or("anthropic")
            .to_lowercase();
        if protocol != "anthropic" {
            warning = format!("warning: provider {} uses the {} protocol; a claude worker needs anthropic, so resolve_route will skip this lane at spawn (fail-safe). Writing anyway.\n", py_repr_str(&pname), py_repr_str(&protocol));
        }
        Some(format!("{pname}/{model}"))
    } else {
        None
    };
    let mut roles = scope_roles(&target)?;
    let builtin = format!("zai,{DEFAULT_SECONDARY_MODEL}");
    if op == "unset" && !roles.contains_key(&name) {
        let out = if DEFAULT_ROUTED_ROLES.contains(&name.as_str()) {
            format!("not configured in {scope}: {name} is routed by the built-in default ({builtin}); nothing to unset.\n")
        } else {
            format!("not configured in {scope}: {name} (no-op).\n")
        };
        return Ok(json!({"action": "noop", "out": out, "err": "", "scope": scope}));
    }
    let out = if let Some(value) = normalized {
        roles.insert(name.clone(), json!(value));
        format!("route set {name} = {value} ({scope})\n")
    } else {
        roles.shift_remove(&name);
        if DEFAULT_ROUTED_ROLES.contains(&name.as_str()) {
            format!("route unset {name}; reverts to built-in default ({builtin}) ({scope})\n")
        } else {
            format!(
                "route unset {name}; no longer routed (next spawn -> primary model) ({scope})\n"
            )
        }
    };
    Ok(json!({
        "action": if roles.is_empty() { "unset" } else { "set" },
        "key": "model_routing.roles", "value": roles, "scope": scope,
        "out": out, "err": warning,
    }))
}

fn scope_path(scope: &str) -> Result<PathBuf, (i32, String)> {
    let path = if scope == "global" {
        crate::agents_config::global_config_path()
            .ok_or_else(|| (1, "error: HOME is unset\n".into()))?
    } else {
        let cwd = std::env::current_dir().map_err(|e| (1, format!("error: {e}\n")))?;
        let root = std::env::var_os("FNO_REPO_ROOT")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                cwd.ancestors()
                    .find(|p| p.join(".git").exists())
                    .map(Path::to_path_buf)
            })
            .unwrap_or(cwd);
        root.join(".fno/config.toml")
    };
    if path.is_symlink() {
        std::fs::canonicalize(&path)
            .map_err(|e| (1, format!("error: cannot read {}: {e}\n", path.display())))
    } else {
        Ok(path)
    }
}

fn scope_roles(path: &Path) -> Result<serde_json::Map<String, Value>, (i32, String)> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(serde_json::Map::new()),
        Err(e) => return Err((1, format!("error: cannot read {}: {e}\n", path.display()))),
    };
    let parsed: toml::Value = toml::from_str(&text).map_err(|e| {
        (
            1,
            format!(
                "error: existing config at {} is malformed: {e}\n",
                path.display()
            ),
        )
    })?;
    let roles = parsed
        .get("model_routing")
        .and_then(|b| b.get("roles"))
        .and_then(toml::Value::as_table);
    Ok(roles
        .map(|r| {
            r.iter()
                .map(|(k, v)| (k.clone(), serde_json::to_value(v).expect("TOML serializes")))
                .collect()
        })
        .unwrap_or_default())
}
