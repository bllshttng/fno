//! The advisory assembly: silent switches, dead letters, the per-harness
//! surface, the config-key readers, evals, and the cost-check +
//! transcript ops, ported from the deleted `cli/src/fno/doctor.py`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{
    call_binary_json, codex_home, command_call, home_dir, resolve_binary, s, which, Probes, Report,
};

// ---------------------------------------------------------------------------
// Config reading (the silent-switch inputs)
// ---------------------------------------------------------------------------

/// One layer of the config chain as a parsed TOML document.
fn read_config_layer(path: &Path) -> Option<toml::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

fn config_bool(doc: Option<&toml::Value>, section: &str) -> Option<bool> {
    doc?.get(section)?.get("enabled")?.as_bool()
}

fn config_string(doc: Option<&toml::Value>, section: &str, key: &str) -> Option<String> {
    doc?.get(section)?.get(key)?.as_str().map(str::to_string)
}

/// The config chain the silent-switch pair reads: global, then the project
/// config walking up from cwd. Later layers win; unset keys take the
/// Python loader's defaults at the read sites.
struct DoctorConfig {
    active_backlog: Option<bool>,
    think_spawn: Option<bool>,
    auto_merge: Option<bool>,
    grant: Option<String>,
}

fn load_doctor_config() -> DoctorConfig {
    let mut cfg = DoctorConfig {
        active_backlog: None,
        think_spawn: None,
        auto_merge: None,
        grant: None,
    };
    for path in config_candidates() {
        let doc = read_config_layer(&path);
        if let Some(v) = config_bool(doc.as_ref(), "active_backlog") {
            cfg.active_backlog = Some(v);
        }
        if let Some(v) = config_bool(doc.as_ref(), "think_spawn") {
            cfg.think_spawn = Some(v);
        }
        if let Some(v) = config_bool(doc.as_ref(), "auto_merge") {
            cfg.auto_merge = Some(v);
        }
        if let Some(v) = config_string(doc.as_ref(), "auto_merge", "grant") {
            cfg.grant = Some(v);
        }
    }
    cfg
}

fn config_candidates() -> Vec<PathBuf> {
    let mut paths = vec![home_dir().join(".fno/config.toml")];
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur: Option<&Path> = Some(cwd.as_path());
        while let Some(dir) = cur {
            let candidate = dir.join(".fno/config.toml");
            if candidate.is_file() {
                paths.push(candidate);
            }
            cur = dir.parent();
        }
    }
    paths
}

// ---------------------------------------------------------------------------
// Silent-switch legibility (doctor.py:1434-1791)
// ---------------------------------------------------------------------------

/// Backlog entries carrying `mission_active`, from the store's nodes table
/// extras (the canonical fail-safe reader: a torn graph reads as 0).
fn mission_active_count() -> i64 {
    let path = super::super::state_dir().join("graph.db");
    if !path.exists() {
        return 0;
    }
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return 0;
    };
    let Ok(mut stmt) = conn.prepare("SELECT id, extras FROM nodes") else {
        return 0;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0), row.get::<_, String>(1)))
    }) else {
        return 0;
    };
    let mut count = 0i64;
    for row in rows.flatten() {
        let extras: Value = serde_json::from_str(&row.1).unwrap_or(Value::Null);
        if extras.get("mission_active").and_then(Value::as_bool) == Some(true) {
            count += 1;
        }
    }
    count
}

/// Worktree manifests whose resolved `auto_merge_approved` is true, counted
/// by source layer. Two layouts: `<base>/<name>/.fno/target-state.md`
/// (harness-native) and `<base>/<repo>/<name>/.fno/target-state.md`
/// (fno-managed); deduped by canonical path.
pub(crate) fn auto_merge_armed_manifests() -> BTreeMap<String, i64> {
    let mut bases: Vec<PathBuf> = vec![super::super::state_dir().join("worktrees")];
    let repo = super::super::resolve_source(None);
    if let Some(repo) = repo {
        bases.push(repo.join(".claude/worktrees"));
    }
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for base in bases {
        if !base.is_dir() {
            continue;
        }
        let Ok(top) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in top.flatten() {
            let first = entry.path();
            let direct = first.join(".fno/target-state.md");
            if direct.is_file() {
                if let Ok(real) = direct.canonicalize() {
                    if seen.insert(real) {
                        count_manifest(&direct, &mut counts);
                    }
                }
                continue;
            }
            let Ok(mid) = std::fs::read_dir(&first) else {
                continue;
            };
            for sub in mid.flatten() {
                let nested = sub.path().join(".fno/target-state.md");
                if nested.is_file() {
                    if let Ok(real) = nested.canonicalize() {
                        if seen.insert(real) {
                            count_manifest(&nested, &mut counts);
                        }
                    }
                }
            }
        }
    }
    counts
}

fn count_manifest(path: &PathBuf, counts: &mut BTreeMap<String, i64>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let approved = text.lines().any(|l| {
        let l = l.trim();
        l == "auto_merge_approved: true" || l == "auto_merge_approved: \"true\""
    });
    if !approved {
        return;
    }
    let source = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("auto_merge_source: "))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_else(|| "unknown".to_string());
    *counts.entry(source).or_insert(0) += 1;
}

fn read_posture_stamp() -> Value {
    let path = super::super::state_dir().join("posture.json");
    match super::super::read_json_object(&path) {
        Some(map) => Value::Object(map),
        None => Value::Null,
    }
}

pub(crate) fn silent_switch_report(plugin_cache: &Value, armed: &BTreeMap<String, i64>) -> Value {
    let cfg = load_doctor_config();
    let ab = cfg.active_backlog;
    let ts = cfg.think_spawn;
    let am = cfg.auto_merge;
    let grant = cfg.grant.as_deref();
    let mut findings: Vec<Value> = Vec::new();
    let missions = mission_active_count();
    if ab == Some(false) && missions > 0 {
        findings.push(json!({
            "direction": "inaction",
            "switch": "active_backlog.enabled",
            "count": missions,
            "count_label": "mission_active epic(s) queued",
            "command": "fno config set active_backlog.enabled true",
        }));
    }
    if ts == Some(false) {
        findings.push(json!({
            "direction": "inaction",
            "switch": "think_spawn.enabled",
            "command": "fno config set think_spawn.enabled true",
        }));
    }
    if am == Some(true) {
        findings.push(json!({
            "direction": "irreversible",
            "switch": "auto_merge.enabled",
            "command": "fno config set auto_merge.enabled false",
        }));
    }
    if grant == Some("dispatch") {
        findings.push(json!({
            "direction": "irreversible",
            "switch": "auto_merge.grant",
            "command": "fno config set auto_merge.grant none",
        }));
    }
    if !armed.is_empty() && am == Some(true) {
        let total: i64 = armed.values().sum();
        let breakdown = breakdown_line(armed);
        let mut finding = json!({
            "direction": "irreversible",
            "switch": "auto_merge_approved (worktree manifests)",
            "count": total,
            "count_label": format!("manifest(s): {breakdown}"),
            "command": "fno config set auto_merge.enabled false",
        });
        if armed.contains_key("unknown") {
            let no_roots = plugin_cache
                .get("roots")
                .and_then(Value::as_array)
                .map(|a| a.is_empty())
                .unwrap_or(true);
            let stale_stage =
                plugin_cache.get("status").and_then(Value::as_str) == Some("stale") && no_roots;
            if stale_stage {
                let sha = plugin_cache
                    .get("sha")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(12)
                    .collect::<String>();
                let when_raw = plugin_cache
                    .get("installed_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(10)
                    .collect::<String>();
                let when = if when_raw.is_empty() {
                    "?"
                } else {
                    when_raw.as_str()
                };
                finding["cause"] = Value::String(format!(
                    "deployed plugin cache is stale ({sha}, {when}); likely predates the auto_merge_source writer. Fix: claude plugin update fno@fno (or fno@footnote; not fno doctor update), then restart"
                ));
            }
        }
        findings.push(finding);
    }
    json!({
        "findings": findings,
        "posture": read_posture_stamp(),
    })
}

fn breakdown_line(armed: &BTreeMap<String, i64>) -> String {
    let mut rows: Vec<(i64, String)> = armed.iter().map(|(src, n)| (*n, src.clone())).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    rows.into_iter()
        .map(|(n, src)| format!("{n} {src}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The durable-grant observer coupling (doctor.py:2570-2577): a standing
/// dispatch grant implies a live watcher; an unreadable config reads no
/// grant.
pub(crate) fn standing_dispatch_grant() -> bool {
    let cfg = load_doctor_config();
    cfg.auto_merge == Some(true) && cfg.grant.as_deref() == Some("dispatch")
}
