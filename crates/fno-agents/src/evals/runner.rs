//! The bank task runner: disposable worktree -> optional worker -> mechanical
//! grade (the deleted Python leg's runner.py, ported). One run = one task
//! executed once; --repeat K is K runs of the same task, each in its own
//! fresh disposable worktree. A bank task never executes in the user's
//! working copy. The worker step routes through `fno agents spawn --once`
//! so provider rotation and the spawn cap stay in play.

use serde_json::json;

/// The worker spawn's answer.
pub struct SpawnResult {
    pub ok: bool,
    pub reason: String,
    pub worker_name: String,
    pub timed_out: bool,
    pub lane_note: String,
}

/// One attempt's outcome and its history-row evidence.
pub struct RunResult {
    pub task_id: String,
    pub tier: String,
    pub passed: bool,
    pub reason: String,
    pub duration_s: f64,
    pub repeat_index: u32,
    pub variant: String,
    pub attempt_index: u32,
    pub attempt_id: serde_json::Value,
    pub status: String,
}

/// Variant must match baseline|v[1-9][0-9]*.
pub fn variant_ok(variant: &str) -> bool {
    if variant == "baseline" {
        return true;
    }
    let Some(digits) = variant.strip_prefix('v') else {
        return false;
    };
    !digits.is_empty()
        && digits.starts_with(|c: char| (c.is_ascii_digit() && c != '0'))
        && digits.chars().all(|c| c.is_ascii_digit())
}

/// Resolve whether the headless grading-worker spawn is armed:
/// config.autonomy.enabled gates first (the wave-3 master switch), then
/// config.evals.enabled (default true; an unreadable config reads OFF).
pub fn evals_enabled() -> bool {
    if !crate::backlog::advance_settings::autonomy_master_enabled(None) {
        return false;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match crate::paths_cli::cfg_str(&cwd, &["evals", "enabled"]) {
        Some(v) => v == "true",
        None => true,
    }
}

/// The run's checkout ref resolved to a sha; a missing ref grades as a
/// failure, never a crash.
fn git_rev(repo_root: &std::path::Path, ref_name: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", ref_name])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// A live owner is a concurrent run; its tree is in use, not orphaned.
/// Tree names end in -<pid>-<ms>.
fn owner_alive(worktree: &str) -> bool {
    let name = std::path::Path::new(worktree)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut parts: Vec<&str> = name.rsplit('-').collect();
    parts.reverse();
    // [<tag words...>, pid, ms]
    if parts.len() < 3 {
        return false;
    }
    match parts[parts.len() - 2].parse::<i32>() {
        Ok(pid) => match unsafe { libc::kill(pid, 0) } {
            0 => true,
            // EPERM: the process exists but belongs to another user.
            -1 => std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM),
            _ => false,
        },
        Err(_) => false,
    }
}

/// The disposable eval worktree base, honoring the worktree-base override.
fn evals_worktree_base() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let base = crate::paths_cli::cfg_str(&cwd, &["paths", "worktrees_base"])
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = dirs_home();
            p.push(".fno");
            p.push("worktrees");
            p
        });
    base.join("evals")
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// One fresh disposable worktree: clean detached checkout at *ref* under
/// the evals base, with an untracked in-flight marker so a concurrent
/// merged-cleanup sweep reads it as a reap candidate and holds.
fn make_disposable_worktree(
    repo_root: &std::path::Path,
    ref_name: &str,
    tag: &str,
) -> Result<std::path::PathBuf, String> {
    let base = evals_worktree_base();
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let name = format!(
        "{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let path = base.join(name);
    let out = std::process::Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&path)
        .arg(ref_name)
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    std::fs::write(path.join(".fno-evals-tree"), "in-flight eval worktree\n")
        .map_err(|e| e.to_string())?;
    Ok(path)
}

fn remove_worktree(repo_root: &std::path::Path, path: &std::path::Path) {
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .current_dir(repo_root)
        .output();
    let _ = std::process::Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(repo_root)
        .output();
}

/// Prune leftover eval worktrees whose owning run is gone. Best-effort; a
/// sweep failure never blocks a fresh run.
pub fn sweep_orphans(repo_root: &std::path::Path) -> usize {
    let Ok(out) = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .output()
    else {
        return 0;
    };
    let evals_base = evals_worktree_base();
    let mut removed = 0usize;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(wt) = line.strip_prefix("worktree ") else {
            continue;
        };
        if !wt.contains(&evals_base.display().to_string()) {
            continue;
        }
        if owner_alive(wt) {
            continue;
        }
        remove_worktree(repo_root, std::path::Path::new(wt));
        removed += 1;
    }
    removed
}

/// The worker spawn: `fno agents spawn --once` in the workdir. A non-zero
/// exit, missing binary, or timeout is a graded failure, never a sweep
/// crash. The worker name is the typed non-node identity
/// `ev-th-evals-<pid>-<secs>`.
fn default_spawn(
    prompt: &str,
    workdir: &std::path::Path,
    timeout_s: i64,
    provider: Option<&str>,
) -> SpawnResult {
    let tag = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    let name = format!("ev-th-evals-{tag}");
    let mut cmd = std::process::Command::new("fno");
    cmd.args(["agents", "spawn"])
        .args(["--name", &name])
        .arg("--once")
        .args(["--cwd", &workdir.display().to_string()])
        .args(["--timeout", &timeout_s.to_string()])
        .arg("--")
        .arg(prompt);
    match cmd.output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SpawnResult {
            ok: false,
            reason: "spawn failed: `fno` binary not found".into(),
            worker_name: name,
            timed_out: false,
            lane_note: String::new(),
        },
        Err(e) => SpawnResult {
            ok: false,
            reason: format!("spawn error: {e}"),
            worker_name: name,
            timed_out: false,
            lane_note: String::new(),
        },
        Ok(out) if out.status.code() != Some(0) => {
            let tail = String::from_utf8_lossy(&out.stderr);
            let tail = if tail.trim().is_empty() {
                String::from_utf8_lossy(&out.stdout).to_string()
            } else {
                tail.to_string()
            };
            let last = tail.trim().lines().last().unwrap_or("").to_string();
            SpawnResult {
                ok: false,
                reason: format!("spawn exit {}: {last}", out.status.code().unwrap_or(-1)),
                worker_name: name,
                timed_out: false,
                lane_note: String::new(),
            }
        }
        Ok(out) => {
            let _ = out;
            SpawnResult {
                ok: true,
                reason: String::new(),
                worker_name: name,
                timed_out: false,
                lane_note: String::new(),
            }
        }
    }
}

/// A resolved lane coordinate from the native inventory (route_gather):
/// the same fold `agents.profiles.*.lanes` joins against, never a second
/// model/effort enum.
pub struct LaneRow {
    pub name: String,
    pub harness: String,
    pub route: String,
    pub account: String,
}

/// Resolve a named lane through the native inventory; the refusal names the
/// declared lanes (the Python repr shape, single quotes).
pub fn resolve_lane(name: &str) -> Result<LaneRow, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let inv = crate::route_gather::inventory(&cwd);
    let rows = inv.get("rows").and_then(serde_json::Value::as_object);
    let row = rows.and_then(|r| r.get(name)).cloned();
    let Some(row) = row else {
        let mut names: Vec<String> = rows
            .map(|r| r.keys().cloned().collect())
            .unwrap_or_default();
        names.sort();
        let joined: Vec<String> = names.iter().map(|n| format!("'{n}'")).collect();
        return Err(format!(
            "unknown lane '{name}'; declared lanes: [{}]",
            joined.join(", ")
        ));
    };
    Ok(LaneRow {
        name: name.to_string(),
        harness: row
            .get("harness")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        route: row
            .get("route")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        account: row
            .get("account")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// The run loop: per (repeat, attempt): fresh disposable worktree ->
/// optional worker -> mechanical grade -> history row -> worktree removed.
/// Every attempt persists with a unique identity and structured
/// observations; the native verdict labels it graded/infrastructure/
/// unavailable/ungraded. (max_retries stays 0 at the CLI surface, the same
/// 0 the Python caller passed.)
pub fn run_task(
    task: &super::bank::TaskSpec,
    repo_root: &std::path::Path,
    repeat: u32,
    history_path: &std::path::Path,
    worker_provider: Option<&str>,
    variant: &str,
    variant_ref: &str,
    lane: Option<&LaneRow>,
    experiment_id: Option<&str>,
) -> Vec<RunResult> {
    let checkout_ref = variant_ref.to_string();
    let bank_rev = git_rev(repo_root, &checkout_ref);
    let timeout_s = std::cmp::max(1, task.timeout_minutes.saturating_mul(60));
    let mut results: Vec<RunResult> = Vec::new();
    let run_id = uuid_v4();
    for i in 0..repeat {
        let started = std::time::Instant::now();
        let started_wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut reason = String::new();
        let mut gate_blocked = false;
        let mut spawned = false;
        let mut worker_timed_out = false;
        let mut outcome: Option<super::grading::GradeOutcome> = None;
        let mut workdir: Option<std::path::PathBuf> = None;
        let mut worker_name = String::new();
        match make_disposable_worktree(repo_root, &checkout_ref, &task.id) {
            Err(e) => {
                reason = format!("fixture checkout failed ({checkout_ref}); fixture drift? {e}");
            }
            Ok(dir) => {
                workdir = Some(dir);
            }
        }
        if let Some(dir) = &workdir {
            if task.prompt.is_some() && !evals_enabled() {
                // The gate only bites the REAL default spawn; a caller with
                // its own worker is an explicit invocation, not autonomous.
                reason = "config.evals.enabled is false".into();
                gate_blocked = true;
            } else if let Some(prompt) = &task.prompt {
                let res = default_spawn(prompt, dir, timeout_s, worker_provider);
                worker_name = res.worker_name.clone();
                if !res.ok {
                    reason = res.reason.clone();
                    worker_timed_out = res.timed_out;
                } else {
                    spawned = true;
                }
            }
            if reason.is_empty() {
                let o = super::grading::grade(task, dir, Some(&checkout_ref));
                if !o.passed {
                    reason = o.reason();
                }
                outcome = Some(o);
            }
            remove_worktree(repo_root, dir);
        }
        let duration = started.elapsed().as_secs_f64().round_ties_even();
        let _ = started_wall;
        let passed = outcome.as_ref().map(|o| o.passed).unwrap_or(false);
        let mut obs = serde_json::Map::new();
        obs.insert("fixture_prepared".into(), json!(workdir.is_some()));
        obs.insert("worker_required".into(), json!(task.prompt.is_some()));
        if task.prompt.is_some() {
            obs.insert("worker_started".into(), json!(spawned));
        }
        if task.prompt.is_some() && worker_timed_out {
            obs.insert("worker_timed_out".into(), json!(true));
        }
        if gate_blocked {
            obs.insert("gate_blocked".into(), json!(true));
        }
        obs.insert("grader_ran".into(), json!(outcome.is_some()));
        if let Some(o) = &outcome {
            obs.insert("grader_passed".into(), json!(o.passed));
        }
        let attempt_id = uuid_v4();
        let row_payload = json!({"obs": obs.clone(), "bank_rev": bank_rev});
        let verdict = crate::eval_attempt::classify(&row_payload, bank_rev.as_deref());
        let status = verdict.status.clone();
        let retryable = false;
        let _ = retryable;
        let mut row = json!({
            "ts": now_iso(),
            "task_id": task.id,
            "tier": task.tier,
            "pass": passed,
            "reason": if passed { "" } else { reason.as_str() },
            "duration_s": duration,
            "repeat_index": i,
            "attempt_index": 0,
            "attempt_id": attempt_id,
            "run_id": run_id,
            "obs": obs,
            "bank_rev": bank_rev,
            "worker_provider": worker_provider,
            "variant": variant,
            "experiment_id": experiment_id,
        });
        if !status.is_empty() {
            row["status"] = json!(status);
        }
        let _ = super::history::append_attempt(history_path, &row);
        if lane.is_some() {
            // Lane evidence rides the observe door as its own arm call; the
            // row records the requested coordinate so the read side can
            // re-ask natively over the batch.
            row["requested_account"] = json!(lane.map(|l| l.account.clone()));
        }
        results.push(RunResult {
            task_id: task.id.clone(),
            tier: task.tier.clone(),
            passed,
            reason: if passed {
                String::new()
            } else {
                reason.clone()
            },
            duration_s: duration,
            repeat_index: i,
            variant: variant.to_string(),
            attempt_index: 0,
            attempt_id: attempt_id.into(),
            status,
        });
    }
    results
}

/// A uuid4-shaped hex id from getrandom (16 bytes, version/variant bits).
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut out = String::with_capacity(32);
    for byte in b {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The row timestamp: Python isoformat's Z form, microseconds.
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}
