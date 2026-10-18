//! Config/path resolution and team-scope compilation (lead/lane.py, projects/resolve.py).
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::SourceRead;

// ---------------------------------------------------------------------------
// Config + path resolution (mirrors fno.paths / fno.claims.io defaults)
// ---------------------------------------------------------------------------

pub(crate) fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

pub(crate) fn home_dot_fno() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fno")
}

/// The graph store path is rooted under the configured state directory.
pub(crate) fn graph_json_path(_cwd: &Path) -> PathBuf {
    let root = match std::env::var_os("FNO_HOME") {
        Some(home) => PathBuf::from(home),
        None => home_dot_fno(),
    };
    crate::state_layout::place(&root, "graph.json")
}

/// `paths.operator_lane()`: pinned global like the ledger - one file per
/// person, never per checkout.
pub(crate) fn operator_lane_path(cwd: &Path) -> PathBuf {
    if let Some(v) = crate::agents_config::config_lookup(cwd, &["paths", "operator_lane"])
        .and_then(|v| v.as_str().map(str::to_string))
    {
        let expanded = expand_home(&v);
        if expanded.is_absolute() {
            return expanded;
        }
        return home_dot_fno().join(expanded);
    }
    let state_dir = crate::agents_config::config_lookup(cwd, &["state_dir"])
        .and_then(|v| v.as_str().map(str::to_string))
        .map(|s| expand_home(&s));
    match state_dir {
        Some(dir) if dir.is_absolute() => dir.join("my-priorities.md"),
        _ => home_dot_fno().join("my-priorities.md"),
    }
}

/// `config.lead.autonomous_merge`, fail-safe to off: an unreadable config
/// resolves an outward, hard-to-reverse action to off, which is the invariant
/// every gate resolver applies to itself.
pub(crate) fn autonomous_merge_enabled(cwd: &Path) -> bool {
    crate::agents_config::config_lookup(cwd, &["lead", "autonomous_merge"])
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// `config.lead.blocked_child_grace_minutes`, fail-safe to the documented
/// default (30): an unreadable or non-integer config must never shrink the
/// grace window a distressed child gets before the board surfaces it.
pub(crate) fn blocked_child_grace_minutes(cwd: &Path) -> i64 {
    crate::agents_config::config_lookup(cwd, &["lead", "blocked_child_grace_minutes"])
        .and_then(|v| v.as_integer())
        .unwrap_or(30)
}

/// The {alias: canonical} project map from `work.workspaces.*.projects[]`
/// (projects/resolve.py's cache builder). `Err` names why the map is absent so
/// a scope spelling that is neither project nor epic can be refused with the
/// Python resolver's wording.
pub(crate) fn project_map(cwd: &Path) -> Result<HashMap<String, String>, String> {
    map_from_work(crate::agents_config::config_lookup(
        cwd,
        &["work", "workspaces"],
    ))
}

/// The map builder, split from the config read so the missing-table refusal is
/// testable without a config on disk. The refusal names the table to add,
/// because the lead board and the lead verdict both surface this text.
pub(crate) fn map_from_work(work: Option<toml::Value>) -> Result<HashMap<String, String>, String> {
    let Some(work) = work else {
        return Err("no work.workspaces in any candidate config.toml; \
             add [work.workspaces.<slug>] with projects = [{ name = \"<project>\", \
             path = \"<checkout>\" }] to ~/.fno/config.toml"
            .to_string());
    };
    let Some(table) = work.as_table() else {
        return Ok(HashMap::new());
    };
    let mut map: HashMap<String, String> = HashMap::new();
    for (_ws, ws_data) in table {
        let Some(projects) = ws_data.get("projects").and_then(|p| p.as_array()) else {
            continue;
        };
        for project in projects {
            let Some(project) = project.as_table() else {
                continue;
            };
            let Some(canonical) = project.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            if canonical.is_empty() {
                continue;
            }
            map.entry(canonical.to_string())
                .or_insert_with(|| canonical.to_string());
            if let Some(short) = project.get("short_name").and_then(|s| s.as_str()) {
                if !short.is_empty() && short != canonical {
                    map.entry(short.to_string())
                        .or_insert_with(|| canonical.to_string());
                }
            }
        }
    }
    Ok(map)
}
// ---------------------------------------------------------------------------
// Scope compilation moved to `crate::territory` (port): the board and
// the drain call the same home. This module keeps the config/path resolvers
// the board shares with it.
// ---------------------------------------------------------------------------

/// The canonical repo roots the board's stranded-tree read should scan: every
/// `work.workspaces.*.projects[].path` that exists on disk, plus this cwd's
/// own canonical root, deduplicated. `project_map`'s values are project
/// NAMES, not paths, so the stranded read needs its own path collector.
pub(crate) fn project_repo_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(work) = crate::agents_config::config_lookup(cwd, &["work", "workspaces"]) {
        if let Some(table) = work.as_table() {
            for (_ws, ws_data) in table {
                let Some(projects) = ws_data.get("projects").and_then(|p| p.as_array()) else {
                    continue;
                };
                for project in projects {
                    let Some(raw) = project.get("path").and_then(|p| p.as_str()) else {
                        continue;
                    };
                    let path = expand_home(raw);
                    // A non-git project (a vault, worktree.policy = never)
                    // would make every `git worktree list` there fail and
                    // read the whole source unreadable; the stranded read
                    // only means anything at a repository root.
                    if path.is_dir()
                        && crate::paths::canonical_repo_root(&path).is_some()
                        && !out.contains(&path)
                    {
                        out.push(path);
                    }
                }
            }
        }
    }
    if let Some(root) = crate::paths::canonical_repo_root(cwd) {
        if !out.contains(&root) {
            out.push(root);
        }
    }
    out
}

/// Lead manifest frontmatter fields (lead/state.parse_manifest): an unreadable
/// manifest reads as absent.
pub(crate) fn parse_manifest(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for line in text.lines() {
        if line.trim() == "---" {
            continue;
        }
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        let raw = raw.trim();
        let raw = if raw.starts_with('"') {
            serde_json::from_str::<String>(raw)
                .unwrap_or_else(|_| raw.trim_matches('"').to_string())
        } else {
            raw.to_string()
        };
        out.insert(key.trim().to_string(), raw);
    }
    out
}

/// The nodes a live team other than `scope` owns: the owner read the org fold
/// and the check-in's blueprint starts answer from, so a lead's stop gate and
/// its check-in cannot disagree about whose node it is. A payload of node ids
/// on success; a registry fault or an uncompilable live scope reads as an
/// error, never as an empty list. No agents home (a hermetic process) holds
/// no teams, the same positive empty the driver feed gives.
pub(crate) fn foreign_owned_read(
    scope: &str,
    entries: &[Value],
    projects: &Result<HashMap<String, String>, String>,
) -> SourceRead {
    let Some(home) = crate::paths::AgentsHome::from_env_opt() else {
        return SourceRead::ok(Value::Array(Vec::new()));
    };
    let mine = crate::territory::canonical_scope(scope);
    match crate::territory::owner_read(&home.registry_json(), entries, projects) {
        Ok((owners, _)) => {
            let mut ids: Vec<&String> = owners
                .iter()
                .filter(|(_, owner)| **owner != mine)
                .map(|(id, _)| id)
                .collect();
            ids.sort();
            SourceRead::ok(Value::Array(
                ids.into_iter()
                    .map(|id| Value::String(id.clone()))
                    .collect(),
            ))
        }
        Err(e) => SourceRead::err(e),
    }
}
