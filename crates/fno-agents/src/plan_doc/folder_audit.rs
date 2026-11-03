//! Precondition check for folder-reader removal, the port of
//! `cli/src/fno/plan/_folder_audit.py`. Counts folder plans (dirs holding
//! `00-INDEX.md`) whose owning graph node is NOT terminal. "Owning" is a
//! basename join on `plan_path` (the fno->fno rename plus the `internal/`
//! symlink mean absolute roots differ between the graph's recorded
//! `plan_path` and the vault's resolved plans dir - this join is the only
//! reliable match). A folder plan with no owning node, or one owned by a
//! terminal node, does not block: frontmatter status is proven stale, so
//! this keys off the graph, never off frontmatter.

use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

const TERMINAL_STATUSES: [&str; 4] = ["done", "superseded", "archived", "cancelled"];

/// A folder plan owned by a non-terminal node.
#[derive(Debug, Clone)]
pub struct NonTerminalOwner {
    pub node_id: String,
    pub status: String,
    pub plan_path: String,
    pub folder: String,
}

/// Every directory under *plans_root* that directly contains 00-INDEX.md.
fn folder_plan_dirs(plans_root: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    walk(plans_root, &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else if path
            .file_name()
            .map(|n| n == "00-INDEX.md")
            .unwrap_or(false)
        {
            out.push(entry.path().parent().unwrap_or(&entry.path()).to_path_buf());
        }
    }
    Ok(())
}

/// Return non-terminal folder-plan owners, or None if the scan is unreliable.
///
/// None signals "fail toward defer" to the caller (unreadable vault); an
/// empty list is a genuine zero.
pub fn scan(plans_root: &Path, graph_entries: &[Value]) -> Option<Vec<NonTerminalOwner>> {
    if !plans_root.is_dir() {
        return None;
    }
    let folder_dirs = match folder_plan_dirs(plans_root) {
        Ok(d) => d,
        Err(_) => return None,
    };
    let folder_basenames: HashSet<String> = folder_dirs
        .iter()
        .filter_map(|d| d.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();

    let mut owners = Vec::new();
    for entry in graph_entries {
        let plan_path = entry
            .get("plan_path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if plan_path.is_empty() {
            continue;
        }
        let base = plan_path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        if !folder_basenames.contains(&base) {
            continue;
        }
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if TERMINAL_STATUSES.contains(&status.as_str()) {
            continue;
        }
        owners.push(NonTerminalOwner {
            node_id: entry
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            status,
            plan_path,
            folder: base,
        });
    }
    Some(owners)
}

#[cfg(test)]
#[path = "folder_audit_tests.rs"]
mod tests;
