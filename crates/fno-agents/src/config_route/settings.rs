//! Recorded overlays: metadata only, with conservative unreferenced pruning.

use super::inventory::{pretty, render_table, value_string};
use super::*;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run_capture(args: &[String]) -> (i32, String, String) {
    let args = crate::client_commands::expand_eq(args);
    if args.first().map(String::as_str) != Some("ls") {
        return (
            2,
            String::new(),
            "route settings: usage: ls [--prune] [--age-days N] [--json]\n".into(),
        );
    }
    let mut prune = false;
    let mut age_days = 14i64;
    let mut json_output = false;
    let mut pos = 1;
    while pos < args.len() {
        match args[pos].as_str() {
            "--prune" => prune = true,
            "--json" | "-J" => json_output = true,
            "--age-days" => {
                pos += 1;
                match args.get(pos).and_then(|s| s.parse().ok()) {
                    Some(age) => age_days = age,
                    None => {
                        return (
                            2,
                            String::new(),
                            "route settings: --age-days requires an integer\n".into(),
                        )
                    }
                }
            }
            other => {
                return (
                    2,
                    String::new(),
                    format!("route settings: unknown argument {other}\n"),
                )
            }
        }
        pos += 1;
    }
    if prune && age_days < 1 {
        return (2, String::new(), format!("--age-days {age_days} would prune a file a launching spawn may have just written but not yet recorded (unreferenced at age 0); the minimum is 1 day.\n"));
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let registry = crate::paths::AgentsHome::from_env().registry_json();
    let referenced: Option<HashSet<String>> =
        crate::client_commands::load_registry_entries(&registry)
            .ok()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| e.get("route_settings_path").and_then(Value::as_str))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            });
    let root = crate::agents_config::state_dir(&cwd).unwrap_or_default();
    let mut files = crate::state_layout::list_dir_children(&root, "route-settings");
    files.retain(|p| p.extension().and_then(|e| e.to_str()) == Some("json"));
    files.sort();
    let providers =
        crate::spawn_overlay::resolve(json!({"kind": "providers", "cwd": cwd.to_string_lossy()}))
            .ok()
            .and_then(|a| a.get("providers").cloned())
            .unwrap_or_else(|| json!({}));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let mut rows = Vec::new();
    let mut pruned = Vec::new();
    let mut err = String::new();
    for path in files {
        let modified = match path.metadata().and_then(|m| m.modified()) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = modified
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or_else(|e| -e.duration().as_secs_f64());
        let age = ((now - modified) / 86400.0).max(0.0);
        let route = read_route(&path);
        let provider = route.as_ref().and_then(|r| match_provider(r, &providers));
        let tiers = provider
            .as_ref()
            .and_then(|p| providers.get(p))
            .and_then(|p| p.get("tier_models"))
            .and_then(Value::as_object);
        let mut stale = Vec::new();
        if let (Some(route), Some(tiers)) = (&route, tiers) {
            for (alias, today) in tiers {
                if let Some(recorded) =
                    route.get(&format!("ANTHROPIC_DEFAULT_{}_MODEL", alias.to_uppercase()))
                {
                    let today = value_string(today);
                    let recorded = value_string(recorded);
                    if !today.is_empty() && !recorded.is_empty() && today != recorded {
                        stale.push(format!("{alias} {recorded} -> {today}"));
                    }
                }
            }
        }
        let path_text = path.to_string_lossy().into_owned();
        let reference = match &referenced {
            None => "?",
            Some(paths) if paths.contains(&path_text) => "yes",
            Some(_) => "no",
        };
        let tiers = tiers
            .map(|tiers| {
                let mut entries: Vec<_> = tiers.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                entries
                    .iter()
                    .map(|(a, m)| format!("{a}={}", value_string(m)))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".into());
        let route_cell = |key: &str| {
            route
                .as_ref()
                .and_then(|r| r.get(key))
                .cloned()
                .unwrap_or_else(|| json!("-"))
        };
        rows.push(json!({
            "file": path.file_name().unwrap_or_default().to_string_lossy(),
            "provider": provider.as_deref().unwrap_or("-"),
            "model": route_cell("ANTHROPIC_MODEL"),
            "haiku": route_cell("ANTHROPIC_DEFAULT_HAIKU_MODEL"),
            "tiers": tiers, "stale": if stale.is_empty() { "-".into() } else { stale.join("; ") },
            "age_days": (age * 10.0).round_ties_even() / 10.0, "referenced": reference,
        }));
        if prune && reference == "no" && age >= age_days as f64 {
            match std::fs::remove_file(&path) {
                Ok(()) => pruned.push(path_text),
                Err(e) => err.push_str(&format!("warn: could not prune {}: {e}\n", path.display())),
            }
        }
    }
    let out = if json_output {
        pretty(&json!({"rows": rows, "pruned": pruned}))
    } else {
        let mut out = render_table(
            &[
                "file",
                "provider",
                "model",
                "haiku",
                "tiers",
                "stale",
                "age_days",
                "referenced",
            ],
            &[
                "FILE", "PROVIDER", "MODEL", "HAIKU", "TIERS", "STALE", "AGE-D", "REF",
            ],
            &rows,
        );
        if prune {
            for path in &pruned {
                out.push_str(&format!("pruned {path}\n"));
            }
            out.push_str(&format!(
                "{} file(s) pruned (unreferenced, older than {age_days}d); {} kept\n",
                pruned.len(),
                rows.len() - pruned.len()
            ));
        }
        out
    };
    (0, out, err)
}

fn read_route(path: &Path) -> Option<Value> {
    let payload: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let env = payload.get("env")?.as_object()?;
    let route: serde_json::Map<String, Value> = env
        .iter()
        .filter_map(|(k, v)| {
            let v = value_string(v);
            (!v.is_empty()).then(|| (k.clone(), json!(v)))
        })
        .collect();
    let has_route = route.keys().any(|k| k != "FNO_ROUTE_PROVIDER");
    has_route.then_some(Value::Object(route))
}

fn match_provider(route: &Value, providers: &Value) -> Option<String> {
    let records = providers.as_object()?;
    let named = route
        .get("FNO_ROUTE_PROVIDER")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if records.contains_key(named) {
        return Some(named.into());
    }
    let base = route
        .get("ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_end_matches('/');
    records
        .iter()
        .find(|(_, r)| {
            !base.is_empty()
                && r.get("base_url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim_end_matches('/')
                    == base
        })
        .map(|(k, _)| k.clone())
}
