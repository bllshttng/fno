//! The status-fanout target resolver behind `fno config status-sinks`:
//! which projects the status-fanout supervisor should tick. The Rust owner,
//! ported from the deleted half of `cli/src/fno/active_backlog.py`.
//!
//! A project is a target when it has >=1 enabled `status_sinks` entry -
//! independent of active_backlog drain enablement. Read-only and
//! best-effort: a malformed config yields an empty list, never an error (the
//! daemon never crashes on an operator config typo).
//!
//! Consumers: the active-backlog daemon (in-process now, instead of shelling
//! the old Python command - one crossing retired), the config command's forward
//! leaf, and the characterization tests.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::backlog::advance_settings::{load_merged, settings_candidates};

/// project -> path, from the workspace map, nearest-file-wins per name.
fn workspace_paths() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    // Outside any checkout the repo walk answers empty before its global
    // leg; the Python resolver this fold replaces always read the global
    // files, so the supervisor (started at an agents-home anchor, not a
    // repo) keeps its configured workspaces.
    let mut candidates = settings_candidates(None);
    if candidates.is_empty() {
        if let Some(gp) = std::env::var_os("FNO_GLOBAL_SETTINGS_PATH").filter(|v| !v.is_empty()) {
            candidates.push(PathBuf::from(gp));
        } else if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            candidates.push(home.join(".fno").join("config.toml"));
            candidates.push(home.join(".fno").join("settings.yaml"));
        }
    }
    for path in candidates {
        let Ok(text) = settings_bytes(&path) else {
            continue;
        };
        let doc = parse_settings_doc(&path, &text);
        let Some(work) = doc.get("work").and_then(Value::as_object) else {
            continue;
        };
        collect_work_block(work, &mut out);
    }
    // Later files override earlier ones (the Python dict fold), so one name
    // survives per fold with the LAST file's path.
    let mut kept: Vec<(String, PathBuf)> = Vec::new();
    for (name, path) in out {
        kept.retain(|(n, _)| &name != n);
        kept.push((name, path));
    }
    kept
}

fn settings_bytes(path: &std::path::Path) -> Result<String, std::io::Error> {
    std::fs::read_to_string(path)
}

fn parse_settings_doc(path: &std::path::Path, text: &str) -> Value {
    match path.extension().and_then(|e| e.to_str()) {
        Some("toml") => text
            .parse::<toml::Table>()
            .ok()
            .and_then(|t| serde_json::to_value(t).ok())
            .unwrap_or(Value::Null),
        _ => serde_yaml_ng::from_str::<Value>(text).unwrap_or(Value::Null),
    }
}

fn expand_tilde(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| PathBuf::from(raw)),
        None => PathBuf::from(raw),
    }
}

/// One settings file's `work` block: multi-workspace entries first, then the
/// legacy flat map. Best-effort: malformed shapes contribute nothing.
fn collect_work_block(work: &serde_json::Map<String, Value>, out: &mut Vec<(String, PathBuf)>) {
    if let Some(workspaces) = work.get("workspaces").and_then(Value::as_object) {
        for ws in workspaces.values() {
            let Some(projects) = ws.get("projects").and_then(Value::as_array) else {
                continue;
            };
            for proj in projects {
                if let (Some(name), Some(raw)) = (
                    proj.get("name").and_then(Value::as_str),
                    proj.get("path").and_then(Value::as_str),
                ) {
                    out.push((name.to_string(), expand_tilde(raw)));
                }
            }
        }
    }
    if let Some(flat) = work.get("projects").and_then(Value::as_object) {
        for (name, cfg) in flat {
            let Some(raw) = cfg.get("path").and_then(Value::as_str) else {
                continue;
            };
            out.push((name.clone(), expand_tilde(raw)));
        }
    }
}

/// One project the status-fanout supervisor should tick. Enablement is
/// "has >=1 enabled status sink", independent of active_backlog drain.
struct FanoutTarget {
    project: String,
    cwd: PathBuf,
    interval_seconds: i64,
}

/// Projects with at least one enabled status sink, each carrying its own
/// `status_fanout.interval_secs` (default 5, floored at 1). Reuses the same
/// workspace project->path map as the drain resolver; a project without a
/// workspace path is skipped (no cwd to tick from - the standalone/cron
/// `fno doctor event fanout tick` covers a runner-less setup).
fn resolve_fanout_targets() -> Vec<FanoutTarget> {
    let mut targets = Vec::new();
    for (name, cwd) in workspace_paths() {
        if cwd.as_os_str().is_empty() {
            continue;
        }
        let settings = load_merged(Some(cwd.as_path()));
        if settings.is_null() {
            continue;
        }
        let has_enabled_sink = settings
            .get("status_sinks")
            .and_then(Value::as_array)
            .is_some_and(|sinks| {
                sinks
                    .iter()
                    .any(|s| s.get("enabled").and_then(Value::as_bool).unwrap_or(true))
            });
        if !has_enabled_sink {
            continue;
        }
        let interval = settings
            .get("status_fanout")
            .and_then(|f| f.get("interval_secs"))
            .and_then(Value::as_i64)
            .unwrap_or(5);
        targets.push(FanoutTarget {
            project: name,
            cwd,
            interval_seconds: interval.max(1),
        });
    }
    targets
}

/// The daemon's in-process discovery leg: JSON rows the supervisor folds.
pub fn fanout_targets_as_dicts() -> Value {
    json!(resolve_fanout_targets()
        .iter()
        .map(|t| json!({
            "project": t.project,
            "cwd": t.cwd.display().to_string(),
            "interval_seconds": t.interval_seconds,
        }))
        .collect::<Vec<_>>())
}

/// `fno-agents status-sinks [--json]`: the command surface the forward leaf in
/// config_cli.py execs. Compact JSON list on `--json`; a tab render
/// otherwise; exit 0 always (best-effort read, never a gate).
pub fn run(args: &[String]) -> i32 {
    let json_out = args.iter().any(|a| a == "--json" || a == "-J");
    let targets = resolve_fanout_targets();
    if json_out {
        let rows: Vec<Value> = targets
            .iter()
            .map(|t| {
                json!({
                "project": t.project,
                "cwd": t.cwd.display().to_string(),
                "interval_seconds": t.interval_seconds,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
        );
        return 0;
    }
    if targets.is_empty() {
        println!("status-sinks: no projects with enabled sinks");
        return 0;
    }
    for t in &targets {
        println!(
            "{}\t{}\tinterval={}s",
            t.project,
            t.cwd.display(),
            t.interval_seconds
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_expands_only_the_home_prefix() {
        std::env::set_var("HOME", "/Users/test");
        assert_eq!(
            expand_tilde("~/code/x"),
            PathBuf::from("/Users/test/code/x")
        );
        assert_eq!(expand_tilde("/abs/x"), PathBuf::from("/abs/x"));
        assert_eq!(expand_tilde("a~b"), PathBuf::from("a~b"));
    }

    #[test]
    fn collect_reads_multi_workspace_then_flat() {
        let work: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "workspaces": {"main": {"projects": [
                {"name": "a", "path": "/a"}, {"name": "bad"}, {"path": "/no-name"}
            ]}},
            "projects": {"flat-proj": {"path": "/flat"}}
        }))
        .unwrap();
        let mut out = Vec::new();
        collect_work_block(&work, &mut out);
        assert_eq!(
            out.len(),
            2,
            "multi-workspace entries only from the list shape"
        );
        assert_eq!(out[0].0, "a");
        let flat: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "projects": {"flat-proj": {"path": "/flat"}}
        }))
        .unwrap();
        let mut flat_out = Vec::new();
        collect_work_block(&flat, &mut flat_out);
        assert_eq!(flat_out.len(), 1);
        assert_eq!(flat_out[0].0, "flat-proj");
    }
}
