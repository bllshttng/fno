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
        && digits.starts_with(|c: char| c.is_ascii_digit() && c != '0')
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
    let pid = std::process::id();
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("{tag}-{pid}-{millis}");
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

/// Every trial worktree is a fresh cwd, so claude keeps one projects dir
/// per trial and they pile up in its session list; the history row is the
/// record. Best-effort: the cleanup never masks the run's verdict.
fn forget_trial_transcript(workdir: &std::path::Path) {
    let slug = crate::claude_ask::claude_cwd_slug(workdir);
    let root = crate::claude_drive::claude_projects_dir();
    let _ = std::fs::remove_dir_all(root.join(slug));
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

/// The spawn argv: the substrate's own --timeout is the first guard; a
/// lane is a complete coordinate (its harness wins over the provider
/// default, and its route/account ride along). `--` fences the prompt.
fn spawn_argv(
    name: &str,
    workdir: &str,
    timeout_s: i64,
    harness: Option<&str>,
    lane: Option<&LaneRow>,
    prompt: &str,
) -> Vec<String> {
    let mut argv: Vec<String> = ["agents", "spawn"].iter().map(|s| s.to_string()).collect();
    argv.push("--name".into());
    argv.push(name.into());
    argv.push("--once".into());
    argv.push("--cwd".into());
    argv.push(workdir.into());
    argv.push("--timeout".into());
    argv.push(timeout_s.to_string());
    if let Some(h) = harness {
        argv.push("--harness".into());
        argv.push(h.into());
    }
    if let Some(l) = lane {
        if !l.route.is_empty() {
            argv.push("--route".into());
            argv.push(l.route.clone());
        }
        if !l.account.is_empty() {
            argv.push("--account".into());
            argv.push(l.account.clone());
        }
    }
    argv.push("--".into());
    argv.push(prompt.into());
    argv
}

/// The worker spawn: `fno agents spawn --once` in the workdir. A non-zero
/// exit, missing binary, or timeout is a graded failure, never a sweep
/// crash. The deadline is the backstop the Python leg kept: a wedged
/// spawner fails the leg instead of hanging the run. The worker name is the
/// typed non-node identity `ev-th-evals-<pid>-<secs>`.
fn default_spawn(
    prompt: &str,
    workdir: &std::path::Path,
    timeout_s: i64,
    provider: Option<&str>,
    lane: Option<&LaneRow>,
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
    let harness = lane
        .map(|l| l.harness.as_str())
        .filter(|h| !h.is_empty())
        .or(provider);
    let argv = spawn_argv(
        &name,
        &workdir.display().to_string(),
        timeout_s,
        harness,
        lane,
        prompt,
    );
    let mut cmd = std::process::Command::new("fno");
    cmd.args(&argv)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(timeout_s.max(1) as u64 + 30);
    let mut child = match cmd.spawn() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return SpawnResult {
                ok: false,
                reason: "spawn failed: `fno` binary not found".into(),
                worker_name: name,
                timed_out: false,
                lane_note: String::new(),
            };
        }
        Err(e) => {
            return SpawnResult {
                ok: false,
                reason: format!("spawn error: {e}"),
                worker_name: name,
                timed_out: false,
                lane_note: String::new(),
            };
        }
        Ok(child) => child,
    };
    loop {
        match child.try_wait() {
            Err(e) => {
                return SpawnResult {
                    ok: false,
                    reason: format!("spawn error: {e}"),
                    worker_name: name,
                    timed_out: false,
                    lane_note: String::new(),
                };
            }
            Ok(Some(status)) => {
                use std::io::Read as _;
                let mut out_text = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out_text);
                }
                let mut err_text = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err_text);
                }
                let code = status.code().unwrap_or(-1);
                if code == 0 {
                    return SpawnResult {
                        ok: true,
                        reason: String::new(),
                        worker_name: name,
                        timed_out: false,
                        lane_note: String::new(),
                    };
                }
                let tail = if err_text.trim().is_empty() {
                    out_text
                } else {
                    err_text
                };
                let last = tail.trim().lines().last().unwrap_or("").to_string();
                return SpawnResult {
                    ok: false,
                    reason: format!("spawn exit {code}: {last}"),
                    worker_name: name,
                    timed_out: false,
                    lane_note: String::new(),
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return SpawnResult {
                        ok: false,
                        reason: format!("spawn timed out after {timeout_s}s"),
                        worker_name: name,
                        timed_out: true,
                        lane_note: String::new(),
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
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
        let declared = joined.join(", ");
        return Err(format!(
            "unknown lane '{name}'; declared lanes: [{declared}]"
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
                let res = default_spawn(prompt, dir, timeout_s, worker_provider, lane);
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
            forget_trial_transcript(dir);
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
        let status = verdict.status.to_string();
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
    let _ = getrandom::fill(&mut b);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spawn_carries_the_lane_coordinate() {
        let lane = LaneRow {
            name: "acme".into(),
            harness: "codex".into(),
            route: "openai/gpt-5.6".into(),
            account: "acme".into(),
        };
        let argv = spawn_argv(
            "ev-th-evals-1-2",
            "/tmp/wt",
            60,
            Some("claude"),
            Some(&lane),
            "do the thing",
        );
        let joined = argv.join(" ");
        assert!(
            joined.contains("--harness codex"),
            "the lane harness wins over the provider: {joined}"
        );
        assert!(joined.contains("--route openai/gpt-5.6"));
        assert!(joined.contains("--account acme"));
        assert!(joined.ends_with("-- do the thing"));
    }

    #[test]
    fn the_provider_default_rides_when_no_lane_names_a_harness() {
        let argv = spawn_argv("n", "/tmp/wt", 60, Some("codex"), None, "p");
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--harness" && w[1] == "codex"));
        let bare = spawn_argv("n", "/tmp/wt", 60, None, None, "p");
        assert!(!bare.contains(&"--harness".to_string()));
    }

    #[test]
    fn a_lane_with_an_empty_harness_falls_back_to_the_provider() {
        let lane = LaneRow {
            name: "r".into(),
            harness: String::new(),
            route: "zai/glm".into(),
            account: "a".into(),
        };
        let argv = spawn_argv("n", "/tmp/wt", 60, Some("claude"), Some(&lane), "p");
        let i = argv.iter().position(|a| a == "--harness").unwrap();
        assert_eq!(argv[i + 1], "claude");
    }
}
