//! Candidate enumeration: sidecars, the projects store, the Codex daemon and
//! rollouts, opencode's store, the daemon roster and the registry. No source
//! here can prove death; the truth reader decides later.

use super::{now_epoch, Candidate, Sources};
use crate::identity::canonical_handle;
use crate::spawn_context::session_identity_key;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// How far past the recency window an opencode session still gets the deeper
/// per-message look: one turn can stream for minutes, not for hours.
const OPENCODE_LONG_TURN_SLACK_S: f64 = 6.0 * 3600.0;

fn mtime_of(path: &Path) -> Option<f64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or(0.0),
    )
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `^\d+\.json$`: a real sidecar. A sessions dir can hold thousands of synced
/// exports and conflict copies, none of which may be parsed.
pub fn is_pid_file(name: &str) -> bool {
    name.strip_suffix(".json")
        .is_some_and(|stem| !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()))
}

/// Python's `int(value)` over what a sidecar can hold.
fn lenient_int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|f| f as i64)),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        Value::Bool(flag) => Some(i64::from(*flag)),
        _ => None,
    }
}

/// Python's `str(value)` on a truthy scalar; None for anything falsy.
fn truthy_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) if number.as_f64() != Some(0.0) => Some(number.to_string()),
        Value::Bool(true) => Some("True".to_string()),
        _ => None,
    }
}

fn sidecar_rows(dirs: &[PathBuf], exclude_short: &HashSet<String>) -> Vec<Candidate> {
    let mut rows = Vec::new();
    for dir in dirs {
        for name in names_in(dir).iter().filter(|name| is_pid_file(name)) {
            let path = dir.join(name);
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let Some(session_id) = data
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|sid| !sid.is_empty())
            else {
                continue;
            };
            let Some(pid) = lenient_int(data.get("pid")).or_else(|| stem_of(&path).parse().ok())
            else {
                continue;
            };
            // A hex mailbox handle, never a friendly name: the drain is
            // handle-keyed, so a name would strand mail on the bus.
            let short_id =
                truthy_text(data.get("jobId")).unwrap_or_else(|| canonical_handle(session_id));
            if exclude_short.contains(&short_id) {
                continue;
            }
            rows.push(Candidate {
                session_id: session_id.to_string(),
                short_id,
                pid,
                cwd: data
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                status: truthy_text(data.get("status")),
                agent: truthy_text(data.get("agent")).unwrap_or_else(|| "claude".to_string()),
                ..Candidate::default()
            });
        }
    }
    rows
}

/// A project dir name is a lossy encoding of the cwd, and whether `_` survives
/// is version-specific, so both forms are tried.
pub fn candidate_dir_names(cwd: &str) -> Vec<String> {
    let collapsed: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let kept: String = cwd
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if collapsed == kept {
        vec![collapsed]
    } else {
        vec![collapsed, kept]
    }
}

fn is_claude_session_argv(argv: &[String]) -> bool {
    let Some(arg0) = argv.first() else {
        return false;
    };
    let base = arg0.rsplit('/').next().unwrap_or(arg0);
    (base == "claude" || arg0.contains("/claude/versions/"))
        && !argv.iter().any(|arg| arg.starts_with("--bg-"))
}

fn cwd_of_pid(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        crate::orphans::read_cwd(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .map(|path| path.display().to_string())
    }
}

/// `(pid, cwd)` for each running Claude Code CLI, daemon infra dropped. This
/// bounds the projects scan to live dirs, never the whole store.
pub(crate) fn live_claude_procs() -> Vec<(u32, String)> {
    let mut table = crate::census::process_table().0;
    table.sort_by_key(|row| row.pid);
    let mut out = Vec::new();
    for row in &table {
        let pid = row.pid;
        // Only libproc reads an argv without a spawn; the `ps` table already
        // carries it.
        let argv: Vec<String> = if cfg!(target_os = "macos") {
            crate::census::process_argv(pid).unwrap_or_default()
        } else {
            row.command.split_whitespace().map(str::to_string).collect()
        };
        if !is_claude_session_argv(&argv) {
            continue;
        }
        if let Some(cwd) = cwd_of_pid(pid).filter(|cwd| !cwd.is_empty()) {
            out.push((pid, cwd));
        }
    }
    out
}

fn newest_transcript(dir: &Path) -> Option<String> {
    let mut best: Option<(f64, String)> = None;
    for name in names_in(dir) {
        if name.contains(".sync-conflict-") || !name.ends_with(".jsonl") {
            continue;
        }
        let path = dir.join(&name);
        if !path.is_file() {
            continue;
        }
        let Some(mtime) = mtime_of(&path) else {
            continue;
        };
        if best.as_ref().is_some_and(|(top, _)| mtime < *top) {
            continue;
        }
        let sid = name.strip_suffix(".jsonl").unwrap_or(&name).to_string();
        best = Some((mtime, sid));
    }
    best.map(|(_, sid)| sid).filter(|sid| !sid.is_empty())
}

fn project_rows(
    projects_dir: &Path,
    procs: &[(u32, String)],
    exclude_ids: &HashSet<String>,
) -> Vec<Candidate> {
    let mut rows = Vec::new();
    let mut seen_cwd: HashSet<&str> = HashSet::new();
    for (pid, cwd) in procs {
        if !seen_cwd.insert(cwd.as_str()) {
            continue;
        }
        let sid = candidate_dir_names(cwd)
            .iter()
            .find_map(|name| newest_transcript(&projects_dir.join(name)));
        let Some(sid) = sid.filter(|sid| !exclude_ids.contains(sid)) else {
            continue;
        };
        rows.push(Candidate {
            short_id: canonical_handle(&sid),
            session_id: sid,
            pid: i64::from(*pid),
            cwd: cwd.clone(),
            agent: "claude".to_string(),
            ..Candidate::default()
        });
    }
    rows
}

/// Threads the app-server reports, or none on any failure. Any runtime context
/// the caller holds must not host this one.
pub(crate) fn codex_loaded_threads() -> Vec<(String, String)> {
    fn read() -> Vec<(String, String)> {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return Vec::new();
        };
        runtime
            .block_on(crate::codex_inject::discover_loaded_threads())
            .map(|threads| {
                threads
                    .into_iter()
                    .map(|thread| (thread.session_id, thread.cwd))
                    .collect::<Vec<(String, String)>>()
            })
            .unwrap_or_default()
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::spawn(read).join().unwrap_or_default();
    }
    read()
}

fn loaded_thread_rows(
    threads: &[(String, String)],
    exclude_short: &HashSet<String>,
    exclude_ids: &HashSet<String>,
) -> Vec<Candidate> {
    let mut rows = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (sid, cwd) in threads {
        if sid.is_empty() || !seen.insert(sid.as_str()) {
            continue;
        }
        let short_id = canonical_handle(sid);
        if exclude_short.contains(&short_id) || exclude_ids.contains(sid) {
            continue;
        }
        rows.push(Candidate {
            session_id: sid.clone(),
            short_id,
            cwd: cwd.clone(),
            agent: "codex".to_string(),
            ..Candidate::default()
        });
    }
    rows
}

/// Rollouts inside the recency window, newest first. The mtime bounds the
/// enumeration only; the truth reader owns every verdict.
fn codex_rows(
    root: &Path,
    recency_s: f64,
    exclude_short: &HashSet<String>,
    exclude_ids: &HashSet<String>,
) -> Vec<Candidate> {
    let cutoff = now_epoch() - recency_s;
    let Ok(paths) = crate::store_heal::scan_files(root, None, &|name| {
        name.starts_with("rollout-") && name.ends_with(".jsonl")
    }) else {
        return Vec::new();
    };
    let mut dated: Vec<(f64, PathBuf)> = paths
        .into_iter()
        .filter_map(|path| mtime_of(&path).map(|mtime| (mtime, path)))
        .filter(|(mtime, _)| *mtime >= cutoff)
        .collect();
    dated.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut rows = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (_, path) in dated {
        let Some((sid, cwd)) = crate::store_heal::codex_meta(&path) else {
            continue;
        };
        if seen.contains(&sid) || exclude_ids.contains(&sid) {
            continue;
        }
        seen.insert(sid.clone());
        let short_id = canonical_handle(&sid);
        if exclude_short.contains(&short_id) {
            continue;
        }
        rows.push(Candidate {
            session_id: sid,
            short_id,
            cwd,
            agent: "codex".to_string(),
            transcript_path: Some(path.display().to_string()),
            ..Candidate::default()
        });
    }
    rows
}

/// Every session in opencode's SQLite store, newest first; a lock or schema
/// drift degrades to no rows. Age is the truth reader's question, never this one.
fn opencode_db_rows(
    db_path: &Path,
    exclude_short: &HashSet<String>,
    exclude_ids: &HashSet<String>,
) -> Vec<Candidate> {
    let Some(conn) = crate::graph_store::open_read_only_wal_safe(db_path) else {
        return Vec::new();
    };
    let Ok(mut statement) =
        conn.prepare("SELECT id, directory FROM session ORDER BY time_updated DESC, id DESC")
    else {
        return Vec::new();
    };
    let Ok(mapped) = statement.query_map([], |row| {
        let id: rusqlite::types::Value = row.get(0)?;
        let directory: rusqlite::types::Value = row.get(1)?;
        Ok((id, directory))
    }) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for pair in mapped {
        let Ok((rusqlite::types::Value::Text(sid), directory)) = pair else {
            continue;
        };
        if sid.is_empty() || seen.contains(&sid) || exclude_ids.contains(&sid) {
            continue;
        }
        seen.insert(sid.clone());
        let short_id = canonical_handle(&sid);
        if exclude_short.contains(&short_id) {
            continue;
        }
        let cwd = match directory {
            rusqlite::types::Value::Text(text) => text,
            _ => String::new(),
        };
        rows.push(Candidate {
            session_id: sid,
            short_id,
            cwd,
            agent: "opencode".to_string(),
            ..Candidate::default()
        });
    }
    rows
}

/// Newest activity for one legacy-tree session, or None when the info file is
/// unreadable. A streaming turn writes into `part/<msg>/`, which moves neither
/// the info file nor the message dir, so a recent session gets the deeper look.
fn opencode_activity_mtime(
    info: &Path,
    msg_root: &Path,
    part_root: &Path,
    deep_cutoff: f64,
) -> Option<f64> {
    let mut mtime = mtime_of(info)?;
    let message_dir = msg_root.join(stem_of(info));
    let Some(dir_mtime) = mtime_of(&message_dir) else {
        return Some(mtime);
    };
    mtime = mtime.max(dir_mtime);
    if mtime < deep_cutoff {
        return Some(mtime);
    }
    let mut newest_mtime = 0.0_f64;
    let mut newest_name = String::new();
    for name in names_in(&message_dir) {
        if let Some(entry_mtime) = mtime_of(&message_dir.join(&name)) {
            if entry_mtime > newest_mtime {
                newest_mtime = entry_mtime;
                newest_name = name;
            }
        }
    }
    if newest_name.is_empty() {
        return Some(mtime);
    }
    mtime = mtime.max(newest_mtime);
    if let Some(part_mtime) = mtime_of(&part_root.join(stem_of(Path::new(&newest_name)))) {
        mtime = mtime.max(part_mtime);
    }
    Some(mtime)
}

/// `(session_id, cwd)` from a session-info JSON. The cwd key is `directory`.
fn opencode_session_info(path: &Path) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let data: Value = serde_json::from_str(&raw).ok()?;
    let sid = data.get("id")?.as_str().filter(|sid| !sid.is_empty())?;
    let cwd = data.get("directory").and_then(Value::as_str).unwrap_or("");
    Some((sid.to_string(), cwd.to_string()))
}

/// The legacy tree: `session/<project>/<ses>.json`, `message/<ses>/`,
/// `part/<msg>/`. Activity only orders the rows; it never excludes one.
fn opencode_tree_rows(
    storage_dir: &Path,
    recency_s: f64,
    exclude_short: &HashSet<String>,
    exclude_ids: &HashSet<String>,
) -> Vec<Candidate> {
    let deep_cutoff = now_epoch() - recency_s - OPENCODE_LONG_TURN_SLACK_S;
    let msg_root = storage_dir.join("message");
    let part_root = storage_dir.join("part");
    let session_root = storage_dir.join("session");
    let mut dated: Vec<(f64, PathBuf)> = Vec::new();
    for project in names_in(&session_root) {
        let project_dir = session_root.join(project);
        for name in names_in(&project_dir) {
            if !name.ends_with(".json") {
                continue;
            }
            let path = project_dir.join(name);
            if let Some(mtime) = opencode_activity_mtime(&path, &msg_root, &part_root, deep_cutoff)
            {
                dated.push((mtime, path));
            }
        }
    }
    dated.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut rows = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (_, path) in dated {
        let Some((sid, cwd)) = opencode_session_info(&path) else {
            continue;
        };
        if seen.contains(&sid) || exclude_ids.contains(&sid) {
            continue;
        }
        seen.insert(sid.clone());
        let short_id = canonical_handle(&sid);
        if exclude_short.contains(&short_id) {
            continue;
        }
        rows.push(Candidate {
            session_id: sid,
            short_id,
            cwd,
            agent: "opencode".to_string(),
            ..Candidate::default()
        });
    }
    rows
}

/// Lenient by design: a `claude --bg` worker leaves no sidecar, so the roster
/// is the only source that surfaces it, and a torn roster yields no rows. A
/// recorded bg pid names the PTY host, so it joins to the session's own process.
fn roster_rows(
    roster_path: &Path,
    socket_pids: &[(String, u32)],
    exclude_short: &HashSet<String>,
    exclude_ids: &HashSet<String>,
) -> Vec<Candidate> {
    let Ok(raw) = std::fs::read_to_string(roster_path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let Some(workers) = parsed.get("workers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for worker in workers.values() {
        let Some(sid) = worker
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|sid| !sid.is_empty())
        else {
            continue;
        };
        if !seen.insert(sid) || exclude_ids.contains(sid) {
            continue;
        }
        let short_id = canonical_handle(sid);
        if exclude_short.contains(&short_id) {
            continue;
        }
        let recorded = lenient_int(worker.get("pid")).unwrap_or(0);
        let pid = socket_pids
            .iter()
            .find(|(key, hit)| key == &short_id && *hit != 0)
            .map(|(_, hit)| i64::from(*hit))
            .unwrap_or(recorded);
        rows.push(Candidate {
            session_id: sid.to_string(),
            short_id,
            pid,
            cwd: truthy_text(worker.get("cwd")).unwrap_or_default(),
            agent: "claude".to_string(),
            ..Candidate::default()
        });
    }
    rows
}

/// Registered sessions, resolvable by canonical handle. A claude row keeps the
/// full uuid for dedup while its `short_id` stays the authoritative job id; the
/// provisional flag keeps a legacy short projection from merging with a full id.
fn registry_rows(
    registry_path: &Path,
    exclude_short: &HashSet<String>,
    exclude_ids: &HashSet<String>,
) -> Vec<Candidate> {
    let Ok(registry) = crate::state::load_registry(registry_path) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let mut seen: HashSet<(String, String, bool)> = HashSet::new();
    for entry in &registry.entries {
        let Some(harness) = entry
            .harness
            .as_deref()
            .filter(|harness| crate::provider::KNOWN_PROVIDERS.contains(harness))
        else {
            continue;
        };
        let full = entry
            .harness_session_id
            .as_deref()
            .filter(|sid| !sid.is_empty());
        let (sid, short, provisional): (Option<String>, Option<String>, bool) =
            if harness == "claude" {
                let short_value = Some(entry.short_id.clone()).filter(|short| !short.is_empty());
                let sid = full.map(str::to_string).or_else(|| short_value.clone());
                let short = short_value.or_else(|| sid.as_deref().map(canonical_handle));
                (sid, short, entry.harness_session_id.is_none())
            } else {
                let sid = full
                    .map(str::to_string)
                    .or_else(|| entry.session_id.clone().filter(|sid| !sid.is_empty()));
                let short = sid.as_deref().map(canonical_handle);
                (sid, short, false)
            };
        let Some(sid) = sid.filter(|sid| !exclude_ids.contains(sid)) else {
            continue;
        };
        let key = (harness.to_string(), session_identity_key(&sid), provisional);
        if !seen.insert(key) {
            continue;
        }
        let short_id = short.unwrap_or_default();
        if exclude_short.contains(&short_id) {
            continue;
        }
        rows.push(Candidate {
            session_id: sid,
            short_id,
            cwd: entry.cwd.clone(),
            agent: harness.to_string(),
            name: Some(entry.name.clone()),
            registry_row: Some(entry.clone()),
            ..Candidate::default()
        });
    }
    rows
}

/// The union, in source order. Sources are unioned always: a host runs several
/// harnesses at once, and a stale sidecar must not suppress a live session.
pub fn enumerate(
    sources: &Sources,
    exclude_short: &HashSet<String>,
    exclude_ids: &HashSet<String>,
) -> Vec<Candidate> {
    let mut rows = sidecar_rows(&sources.sessions_dirs, exclude_short);
    rows.extend(
        project_rows(&sources.projects_dir, &sources.live_procs, exclude_ids)
            .into_iter()
            .filter(|row| !exclude_short.contains(&row.short_id)),
    );
    rows.extend(loaded_thread_rows(
        &sources.loaded_threads,
        exclude_short,
        exclude_ids,
    ));
    rows.extend(codex_rows(
        &sources.codex_sessions_dir,
        sources.recency_s,
        exclude_short,
        exclude_ids,
    ));
    // Branch on the database existing, never on it returning no rows: a locked
    // or drifted store must not resurrect the legacy tree's dead sessions.
    let opencode_db = sources
        .opencode_storage_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join("opencode.db");
    if opencode_db.exists() {
        rows.extend(opencode_db_rows(&opencode_db, exclude_short, exclude_ids));
    } else {
        rows.extend(opencode_tree_rows(
            &sources.opencode_storage_dir,
            sources.recency_s,
            exclude_short,
            exclude_ids,
        ));
    }
    rows.extend(roster_rows(
        &sources.roster_path,
        &sources.bg_socket_pids,
        exclude_short,
        exclude_ids,
    ));
    rows.extend(registry_rows(
        &sources.registry_path,
        exclude_short,
        exclude_ids,
    ));
    rows
}

/// A session cwd to a settings project, worktree-aware: a `.claude/worktrees`
/// session and a conductor workspace attribute to the parent repo.
pub fn resolve_project_for_cwd(cwd: &str) -> Option<String> {
    use crate::backlog::settings::{detect_project, expand_home, normalize, work_map_pairs};
    if cwd.is_empty() {
        return None;
    }
    let path = normalize(&expand_home(cwd)).display().to_string();
    if let Some((root, _)) = path.split_once("/.claude/worktrees/") {
        if let Some(project) = detect_project(Some(root)) {
            return Some(project);
        }
    }
    let parts: Vec<&str> = path.split('/').collect();
    if let Some(at) = parts.iter().position(|part| *part == "workspaces") {
        if let Some(repo) = parts.get(at + 1) {
            let hit = work_map_pairs()
                .into_iter()
                .find(|(_, abs)| abs.rsplit('/').next() == Some(*repo));
            if let Some((name, _)) = hit {
                return Some(name);
            }
        }
    }
    detect_project(Some(&path))
}
