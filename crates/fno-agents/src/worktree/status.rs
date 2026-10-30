//! The worktree status read, ported from the deleted
//! `cli/src/fno/worktree_status.py`: the porcelain listing, the registry join,
//! the text and JSON renders, and the fno-bases-filtered listing the
//! provenance scan consumed via `runtime/worktree.py`. Characterized by the
//! `status_*` and `list_*` goldens.

use super::{git_out, resolve_lenient, sorted_json, OWNERSHIP_LIVE_STATUSES};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The one occupancy join: cwd -> (best name, status); plus the ok flag. A
/// missing registry is a legitimate empty fleet (ok); an unreadable one is
/// not. Mirrors the deleted `_load_registry`.
pub fn registry_join() -> (JoinMap, bool) {
    registry_join_at(&registry_path())
}

/// The registry file: the `WORKTREE_STATUS_REGISTRY` pin, else the one
/// resolver every surface reads ([`AgentsHome::shared_registry_json`]).
fn registry_path() -> PathBuf {
    if let Some(over) = std::env::var_os("WORKTREE_STATUS_REGISTRY") {
        if !over.is_empty() {
            return PathBuf::from(over);
        }
    }
    crate::paths::AgentsHome::shared_registry_json()
}

/// The porcelain listing via the shared parser.
fn porcelain(repo: &Path) -> Option<Vec<(Option<String>, String)>> {
    let out = git_out(repo, &["worktree", "list", "--porcelain"])?;
    if !out.status.success() {
        return None;
    }
    Some(super::parse_porcelain_rows(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

/// git log -1 --format=%cr output stripped, or "unknown" when the read
/// fails or answers nothing.
fn last_commit_age(path: &str) -> String {
    match Command::new("git")
        .args(["log", "-1", "--format=%cr"])
        .current_dir(path)
        .output()
    {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if text.is_empty() {
                "unknown".to_string()
            } else {
                text
            }
        }
        _ => "unknown".to_string(),
    }
}

/// One classified row, shared by both renders.
pub struct StatusRow {
    pub branch: Option<String>,
    pub path: String,
    pub last_commit: String,
    pub target: String,
    pub session_name: String,
}

/// The classified rows for `repo`.
pub fn status_rows(repo: &Path) -> Vec<StatusRow> {
    let (join, _registry_ok) = registry_join();
    status_rows_join(repo, &join)
}

/// The text render the bash lifecycle script's shim relays.
pub fn render_text(rows: &[StatusRow]) -> String {
    let mut out = String::from("Worktrees:\n");
    for row in rows {
        let branch_label = row
            .branch
            .clone()
            .unwrap_or_else(|| "(detached)".to_string());
        out.push_str(&format!(
            "  {branch_label:<30} | {last:<15} | target: {target:<20} | {path}\n",
            last = row.last_commit,
            target = row.target,
            path = row.path
        ));
    }
    out
}

/// The JSON render the scripts shim and pr_watch consume.
pub fn render_json(rows: &[StatusRow]) -> serde_json::Value {
    let (total, live_n, dead_n, none_n) = counts(rows);
    json!({
        "worktrees": rows.iter().map(|r| json!({
            "branch": r.branch,
            "path": r.path,
            "last_commit": r.last_commit,
            "target": r.target,
            "session_name": r.session_name,
        })).collect::<Vec<_>>(),
        "summary": {"total": total, "live": live_n, "dead": dead_n, "no_session": none_n}
    })
}

fn counts(rows: &[StatusRow]) -> (usize, usize, usize, usize) {
    let mut total = 0;
    let mut live_n = 0;
    let mut dead_n = 0;
    let mut none_n = 0;
    for row in rows {
        total += 1;
        if row.target == "none" {
            none_n += 1;
        } else if row.target.starts_with("live:") {
            live_n += 1;
        } else {
            dead_n += 1;
        }
    }
    (total, live_n, dead_n, none_n)
}

/// The `worktree-status` op: `--repo R [--json]`.
pub fn run_client(args: &[String]) -> i32 {
    let mut repo = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" if i + 1 < args.len() => {
                repo = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            a if crate::json_output::is_flag(a) => {
                as_json = true;
                i += 1;
            }
            other => {
                eprintln!("fno-agents worktree-status: unexpected argument {other}");
                return 2;
            }
        }
    }
    run_status_print(&repo, as_json)
}

fn run_status_print(repo: &Path, as_json: bool) -> i32 {
    let (join, ok) = registry_join();
    if !ok {
        eprintln!(
            "worktree-status: registry read unavailable (unreadable file or missing deps); \
             every session reads as none"
        );
    }
    let rows = status_rows_join(repo, &join);
    if as_json {
        // The Python leg printed insertion order (no sort_keys), summary last.
        println!(
            "{}",
            serde_json::to_string(&render_json(&rows)).unwrap_or_default()
        );
    } else {
        print!("{}", render_text(&rows));
    }
    0
}

/// Classify with an already-resolved join so the sweep and the op share it.
fn status_rows_join(repo: &Path, join: &JoinMap) -> Vec<StatusRow> {
    let mut rows = Vec::new();
    for (branch, wt_path) in porcelain(repo).unwrap_or_default() {
        let (name, status) = join
            .get(&wt_path)
            .cloned()
            .unwrap_or((String::new(), String::new()));
        let (target, _live) = classify_target(&name, &status);
        rows.push(StatusRow {
            branch,
            path: wt_path.clone(),
            last_commit: last_commit_age(&wt_path),
            target,
            session_name: name,
        });
    }
    rows
}

fn classify_target(name: &str, status: &str) -> (String, bool) {
    if name.is_empty() {
        ("none".to_string(), false)
    } else if OWNERSHIP_LIVE_STATUSES.contains(&status) {
        (format!("live:{name}"), true)
    } else {
        (
            format!(
                "{}:{name}",
                if status.is_empty() { "exited" } else { status }
            ),
            false,
        )
    }
}

/// The `worktree-list` op: rows under the fno bases only, the contract the
/// provenance scan and the evals orphans sweep consumed.
pub fn run_list_client(args: &[String]) -> i32 {
    let Some(repo) = repo_arg(args) else {
        eprintln!("fno-agents worktree-list: --repo-root is required");
        return 2;
    };
    let rows = list_worktrees(Path::new(&repo));
    let payload: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "name": r.name,
                "worktree_path": r.worktree_path,
                "branch": r.branch,
            })
        })
        .collect();
    println!("{}", sorted_json(&Value::Array(payload)));
    0
}

fn repo_arg(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--repo-root" && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        i += 1;
    }
    None
}

pub struct ListRow {
    pub name: String,
    pub worktree_path: String,
    pub branch: Option<String>,
}

/// Worktrees under the canonical worktrees base or the legacy
/// `<repo>/.claude/worktrees`, matching the deleted list_worktrees.
pub fn list_worktrees(repo_root: &Path) -> Vec<ListRow> {
    let repo_root = resolve_lenient(repo_root);
    let base = super::worktrees_base();
    let legacy = repo_root.join(".claude").join("worktrees");
    let rows = porcelain(&repo_root).unwrap_or_default();
    let mut out = Vec::new();
    for (branch, path) in rows {
        let p = PathBuf::from(&path);
        let under = is_under(&p, &base) || is_under(&p, &legacy);
        if !under {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(ListRow {
            name,
            worktree_path: path.clone(),
            branch,
        });
    }
    out
}

fn is_under(candidate: &Path, base: &Path) -> bool {
    candidate.starts_with(base)
}

fn registry_join_at(path: &PathBuf) -> (JoinMap, bool) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (Default::default(), true);
    };
    let doc: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return (Default::default(), false),
    };
    let agents = doc
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    best_rows(&agents)
}

/// The best-row fold: live status outranks, then the freshest timestamp.
fn best_rows(agents: &[Value]) -> (JoinMap, bool) {
    let mut by_cwd: std::collections::HashMap<String, Vec<&Value>> = Default::default();
    for row in agents {
        let Some(cwd) = row
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        by_cwd.entry(cwd.to_string()).or_default().push(row);
    }
    let mut best: JoinMap = Default::default();
    for (cwd, group) in by_cwd {
        let mut chosen: Option<(i64, String, String, String)> = None;
        for row in group {
            let status = row
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let rank = if OWNERSHIP_LIVE_STATUSES.contains(&status) {
                1
            } else {
                0
            };
            let timestamp = row
                .get("last_reconciled_at")
                .or_else(|| row.get("exited_at"))
                .or_else(|| row.get("created_at"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let take = match &chosen {
                None => true,
                Some(cur) => (rank, timestamp.clone()) > (cur.0, cur.2.clone()),
            };
            if take {
                chosen = Some((rank, name, timestamp, status.to_string()));
            }
        }
        if let Some((_rank, name, _ts, status)) = chosen {
            best.insert(cwd, (name, status));
        }
    }
    (best, true)
}

pub type JoinMap = std::collections::HashMap<String, (String, String)>;
