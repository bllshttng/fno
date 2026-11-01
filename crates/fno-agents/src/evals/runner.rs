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
    crate::paths_cli::worktrees_base(&cwd).join("evals")
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

/// The fixture tree a golden prompt names, carried onto an older checkout. Only
/// this tree moves: `evals/bank` holds each golden's expected answer, and a
/// worker told to read the repo would read it.
const OVERLAY_PATH: &str = "evals/fixtures/golden";

/// Commit `overlay_ref`'s fixtures on top of the checkout, so the worker sees a
/// clean tree. Returns the new head, the base a grader diffs against.
fn apply_overlay(dir: &std::path::Path, overlay_ref: &str) -> Result<String, String> {
    git_in(dir, &["checkout", overlay_ref, "--", OVERLAY_PATH])?;
    commit_all(dir, "evals: overlay golden fixtures")
}

fn git_in(dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Each path holds a golden's answer. Workers told to read the repo did read
/// them, so a golden's tree leaves them out. The loaded task keeps its grader
/// in memory.
const ANSWER_KEY_PATHS: [&str; 2] = ["evals/bank", "evals/fixtures/metrics"];

/// Where sealed trees live: the temp dir, so no `$HOME` file sits on the
/// worker's ancestor path. None of it may sit inside `$HOME`, which the fence
/// denies as a whole.
fn sealed_base(
    temp: &std::path::Path,
    home: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let base = temp.join("fno-evals");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let base = base.canonicalize().map_err(|e| e.to_string())?;
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    if base.starts_with(&home) {
        return Err(format!(
            "sealed tree base {} sits inside the home dir {}",
            base.display(),
            home.display()
        ));
    }
    Ok(base)
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

/// Write `ref_name`'s files (or only `paths` of them) into `dest`.
fn extract_ref(
    repo_root: &std::path::Path,
    ref_name: &str,
    paths: &[&str],
    dest: &std::path::Path,
) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut archive = Command::new("git")
        .args(["archive", "--format=tar", ref_name])
        .args(paths)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = archive.stdout.take().ok_or("git archive gave no stdout")?;
    let tar = Command::new("tar")
        .args(["-xf", "-", "-C"])
        .arg(dest)
        .stdin(stdout)
        .output()
        .map_err(|e| e.to_string())?;
    let archived = archive.wait_with_output().map_err(|e| e.to_string())?;
    if !archived.status.success() {
        return Err(String::from_utf8_lossy(&archived.stderr).trim().to_string());
    }
    if !tar.status.success() {
        return Err(String::from_utf8_lossy(&tar.stderr).trim().to_string());
    }
    Ok(())
}

/// A golden's tree: `ref_name`'s files as one commit in a new repo. A worktree
/// names the real repo in its `.git` file, and any parent commit holds the
/// rules and the answer keys one `git show` away, so this tree has neither.
/// Returns the path and its head, the base a grader diffs against.
fn make_sealed_tree(
    repo_root: &std::path::Path,
    ref_name: &str,
    overlay_ref: Option<&str>,
    tag: &str,
    base: &std::path::Path,
) -> Result<(std::path::PathBuf, String), String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = base.join(format!("{tag}-{}-{millis}", std::process::id()));
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    let built = fill_sealed_tree(repo_root, ref_name, overlay_ref, &path);
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&path);
    }
    built.map(|head| (path, head))
}

fn fill_sealed_tree(
    repo_root: &std::path::Path,
    ref_name: &str,
    overlay_ref: Option<&str>,
    dir: &std::path::Path,
) -> Result<String, String> {
    extract_ref(repo_root, ref_name, &[], dir)?;
    if let Some(overlay) = overlay_ref {
        let _ = std::fs::remove_dir_all(dir.join(OVERLAY_PATH));
        extract_ref(repo_root, overlay, &[OVERLAY_PATH], dir)?;
    }
    for key in ANSWER_KEY_PATHS {
        let _ = std::fs::remove_dir_all(dir.join(key));
    }
    // An empty template: a user's template can carry hooks that name real paths.
    git_in(dir, &["init", "-q", "--template="])?;
    // The fence denies the user's git config, so the worker's git reads these.
    for (key, value) in [
        ("user.name", "fno-evals"),
        ("user.email", "evals@fno.invalid"),
        ("core.hooksPath", "/dev/null"),
        ("core.excludesFile", "/dev/null"),
        ("core.attributesFile", "/dev/null"),
        ("commit.gpgsign", "false"),
    ] {
        git_in(dir, &["config", key, value])?;
    }
    git_in(dir, &["add", "-A", "-f"])?;
    commit_all(dir, "evals: golden tree")
}

fn commit_all(dir: &std::path::Path, message: &str) -> Result<String, String> {
    git_in(
        dir,
        &[
            "-c",
            "user.name=fno-evals",
            "-c",
            "user.email=evals@fno.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            message,
        ],
    )?;
    git_in(dir, &["rev-parse", "HEAD"])
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
    let _ = std::fs::remove_dir_all(trial_transcript_dir(workdir));
}

fn trial_transcript_dir(workdir: &std::path::Path) -> std::path::PathBuf {
    crate::claude_drive::claude_projects_dir().join(crate::claude_ask::claude_cwd_slug(workdir))
}

/// A failed trial keeps its transcript, the only record of why it failed.
const KEPT_TRANSCRIPT_DAYS: u64 = 14;

/// Drop kept trial transcripts past their age. The evals base slug prefixes a
/// trial's dir, so a real session's transcripts never match.
fn prune_kept_transcripts(projects: &std::path::Path, base_slug: &str, max_age_days: u64) {
    let Ok(entries) = std::fs::read_dir(projects) else {
        return;
    };
    let max_age = std::time::Duration::from_secs(max_age_days * 86_400);
    let prefix = format!("{base_slug}-");
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if old && entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Prune leftover eval worktrees whose owning run is gone. Best-effort; a
/// sweep failure never blocks a fresh run.
pub fn sweep_orphans(repo_root: &std::path::Path) -> usize {
    prune_kept_transcripts(
        &crate::claude_drive::claude_projects_dir(),
        &crate::claude_ask::claude_cwd_slug(&evals_worktree_base()),
        KEPT_TRANSCRIPT_DAYS,
    );
    let swept = sealed_base(&std::env::temp_dir(), &home_dir())
        .map(|base| {
            prune_kept_transcripts(
                &crate::claude_drive::claude_projects_dir(),
                &crate::claude_ask::claude_cwd_slug(&base),
                KEPT_TRANSCRIPT_DAYS,
            );
            sweep_sealed_trees(&base)
        })
        .unwrap_or(0);
    swept + sweep_worktrees(repo_root)
}

/// A sealed tree is not a registered worktree, so `git worktree list` never
/// names it. Its name carries the owner, the same as a worktree's.
fn sweep_sealed_trees(base: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(base) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        if !owner_alive(&entry.path().display().to_string())
            && std::fs::remove_dir_all(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

fn sweep_worktrees(repo_root: &std::path::Path) -> usize {
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

/// A claude worker's flags that make the tree under test its only source of
/// rules. Without them it loads the installed plugin, the user's settings and
/// their MCP servers, so a cohort measures today's machine and not the tree.
/// The tree's own plugin stays off too: its hooks read the live fleet, and the
/// worker then acts on that state. Only claude has these flags, so any other
/// harness runs with today's plugin context and no read fence.
fn tree_only_flags(
    harness: Option<&str>,
    tree: &std::path::Path,
    home: &std::path::Path,
) -> Vec<String> {
    if harness != Some("claude") {
        return Vec::new();
    }
    vec![
        "--setting-sources".into(),
        "project".into(),
        "--strict-mcp-config".into(),
        format!("--settings={}", read_fence(tree, home)),
    ]
}

/// The file tools stop at the block and the deny rules, and Bash stops at the
/// OS sandbox. The block alone leaves `cat` and `git` free, and a canary read
/// `~/.claude` and `~/.fno` past it, so the deny rules take all of home. The
/// sealed base is denied because a sibling trial's tree holds its
/// `decision.md`.
fn read_fence(tree: &std::path::Path, home: &std::path::Path) -> serde_json::Value {
    let base = tree.parent().unwrap_or(tree);
    json!({
        "permissions": {
            "blockReadsOutsideWorkingDirectories": true,
            "deny": ["WebFetch", "WebSearch", "Read(~/**)", "Edit(~/**)"],
        },
        "sandbox": {
            "enabled": true,
            "allowUnsandboxedCommands": false,
            "failIfUnavailable": true,
            "network": {"strictAllowlist": true, "allowedDomains": []},
            "filesystem": {
                "denyRead": [home, base],
                "allowRead": [tree],
            },
        },
    })
}

/// The env of a fenced worker. Auto memory reads the user's files. The seal
/// drops the spawn's state grants. Git would fail on the user's config, which
/// the fence denies, so it reads only the tree's own.
const SEALED_WORKER_ENV: [(&str, &str); 4] = [
    ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
    (crate::spawn_seam::EVALS_SEALED_ENV, "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
];

/// The spawn argv: the substrate's own --timeout is the first guard; a
/// lane is a complete coordinate (its harness wins over the provider
/// default, and its route/account ride along). `--` fences the prompt, or
/// with `harness_flags` fences those and the prompt sits before it.
fn spawn_argv(
    name: &str,
    workdir: &str,
    timeout_s: i64,
    harness: Option<&str>,
    lane: Option<&LaneRow>,
    prompt: &str,
    harness_flags: &[String],
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
    if harness_flags.is_empty() {
        argv.push("--".into());
        argv.push(prompt.into());
    } else {
        argv.push(prompt.into());
        argv.push("--".into());
        argv.extend_from_slice(harness_flags);
    }
    argv
}

/// True when a spawn exit means the worker ran out its budget. 124 is the
/// claude one-shot's timeout. Exit 12 is opencode's timeout and also the code
/// for a registry failure before launch, so 12 counts only with its message.
fn spawn_exit_timed_out(code: i32, message: &str) -> bool {
    code == 124 || (code == 12 && message.contains("timed out"))
}

/// The content the worker left, as a git tree id. The temporary index keeps
/// the real one untouched, and starting from a copy of it spares rehashing
/// the whole checkout. None when git cannot say, which compare reads as
/// unknown.
fn tree_hash(workdir: &std::path::Path) -> Option<String> {
    let git = |args: &[&str], index: Option<&std::path::Path>| {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args).current_dir(workdir);
        if let Some(i) = index {
            cmd.env("GIT_INDEX_FILE", i);
        }
        cmd.output().ok().filter(|o| o.status.success())
    };
    let real = git(&["rev-parse", "--git-path", "index"], None)?;
    let real = workdir.join(String::from_utf8_lossy(&real.stdout).trim());
    let scratch = tempfile::tempdir().ok()?;
    let index = scratch.path().join("index");
    std::fs::copy(real, &index).ok()?;
    git(&["add", "-A"], Some(&index))?;
    let tree = git(&["write-tree"], Some(&index))?;
    Some(String::from_utf8_lossy(&tree.stdout).trim().to_string())
}

/// The worker spawn: `fno agents spawn --once` in the workdir. A non-zero
/// exit, missing binary, or timeout is a graded failure, never a sweep
/// crash. The deadline is the backstop the Python leg kept: a wedged
/// spawner fails the leg instead of hanging the run. The worker name is the
/// typed non-node identity `ev-th-evals-<pid>-<secs>`. The pipes drain on
/// their own threads, because a worker that fills one would otherwise block
/// until the deadline and read as a stall.
fn default_spawn(
    prompt: &str,
    workdir: &std::path::Path,
    timeout_s: i64,
    provider: Option<&str>,
    lane: Option<&LaneRow>,
    harness_flags: &[String],
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
        harness_flags,
    );
    let mut cmd = std::process::Command::new(crate::scrape::fno_bin());
    if !harness_flags.is_empty() {
        cmd.envs(SEALED_WORKER_ENV);
    }
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
    fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut text);
            }
            text
        })
    }
    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());
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
                let out_text = out_reader.join().unwrap_or_default();
                let err_text = err_reader.join().unwrap_or_default();
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
                    timed_out: spawn_exit_timed_out(code, &last),
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
    lane_from_inventory(&crate::route_gather::inventory(&cwd), name)
}

fn lane_from_inventory(inv: &serde_json::Value, name: &str) -> Result<LaneRow, String> {
    let rows = inv
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let field = |row: &serde_json::Value, key: &str| {
        row.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let Some(row) = rows.iter().find(|r| field(r, "name") == name) else {
        let mut names: Vec<String> = rows.iter().map(|r| field(r, "name")).collect();
        names.sort();
        let joined: Vec<String> = names.iter().map(|n| format!("'{n}'")).collect();
        let declared = joined.join(", ");
        return Err(format!(
            "unknown lane '{name}'; declared lanes: [{declared}]"
        ));
    };
    Ok(LaneRow {
        name: name.to_string(),
        harness: field(row, "harness"),
        route: field(row, "route"),
        account: field(row, "account"),
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
    overlay_ref: Option<&str>,
) -> Vec<RunResult> {
    let checkout_ref = variant_ref.to_string();
    let bank_rev = git_rev(repo_root, &checkout_ref);
    let overlay_rev = overlay_ref.and_then(|r| git_rev(repo_root, r));
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
        let mut worker_name: Option<String> = None;
        let mut tree: Option<String> = None;
        let mut worker_sealed = false;
        let mut kept_transcript: Option<std::path::PathBuf> = None;
        let mut grade_base = checkout_ref.clone();
        let golden = task.golden.is_some();
        if golden {
            let overlay = overlay_rev.as_deref().or(overlay_ref);
            match sealed_base(&std::env::temp_dir(), &home_dir()).and_then(|base| {
                make_sealed_tree(repo_root, &checkout_ref, overlay, &task.id, &base)
            }) {
                Err(e) => {
                    reason =
                        format!("fixture checkout failed ({checkout_ref}); fixture drift? {e}");
                }
                Ok((dir, head)) => {
                    grade_base = head;
                    workdir = Some(dir);
                }
            }
        } else {
            match make_disposable_worktree(repo_root, &checkout_ref, &task.id) {
                Err(e) => {
                    reason =
                        format!("fixture checkout failed ({checkout_ref}); fixture drift? {e}");
                }
                Ok(dir) => match overlay_rev
                    .as_deref()
                    .or(overlay_ref)
                    .map(|r| apply_overlay(&dir, r))
                {
                    Some(Err(e)) => {
                        reason = format!("fixture overlay failed on {checkout_ref}: {e}");
                        remove_worktree(repo_root, &dir);
                    }
                    Some(Ok(head)) => {
                        grade_base = head;
                        workdir = Some(dir);
                    }
                    None => workdir = Some(dir),
                },
            }
        }
        if let Some(dir) = &workdir {
            if task.prompt.is_some() && !evals_enabled() {
                // The gate only bites the REAL default spawn; a caller with
                // its own worker is an explicit invocation, not autonomous.
                reason = "config.evals.enabled is false".into();
                gate_blocked = true;
            } else if let Some(prompt) = &task.prompt {
                let harness = lane
                    .map(|l| l.harness.as_str())
                    .filter(|h| !h.is_empty())
                    .or(worker_provider);
                // Other bank tasks invoke plugin skills, so only a golden drops them.
                // A leading dash needs the fence, so that prompt also keeps them.
                let flags = if golden && !prompt.starts_with('-') {
                    tree_only_flags(harness, dir, &home_dir().canonicalize().unwrap_or_default())
                } else {
                    Vec::new()
                };
                worker_sealed = !flags.is_empty();
                let res = default_spawn(prompt, dir, timeout_s, worker_provider, lane, &flags);
                if !res.ok {
                    reason = res.reason.clone();
                    worker_timed_out = res.timed_out;
                }
                if res.ok || res.timed_out {
                    spawned = true;
                    worker_name = Some(res.worker_name.clone());
                }
            }
            if !gate_blocked && (spawned || task.prompt.is_none()) {
                tree = tree_hash(dir);
            }
            if reason.is_empty() {
                let o = super::grading::grade(task, dir, Some(&grade_base));
                if !o.passed {
                    reason = o.reason();
                }
                outcome = Some(o);
            }
            if golden {
                let _ = std::fs::remove_dir_all(dir);
            } else {
                remove_worktree(repo_root, dir);
            }
            if outcome.as_ref().is_some_and(|o| o.passed) {
                forget_trial_transcript(dir);
            } else {
                kept_transcript = Some(trial_transcript_dir(dir));
            }
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
        if task.prompt.is_some() {
            obs.insert("worker_sealed".into(), json!(worker_sealed));
        }
        if let Some(dir) = kept_transcript.as_ref().filter(|d| d.is_dir()) {
            obs.insert("kept_transcript".into(), json!(dir.display().to_string()));
        }
        if gate_blocked {
            obs.insert("gate_blocked".into(), json!(true));
        }
        obs.insert("grader_ran".into(), json!(outcome.is_some()));
        if let Some(o) = &outcome {
            obs.insert("grader_passed".into(), json!(o.passed));
        }
        if let Some(name) = &worker_name {
            obs.insert("worker".into(), json!(name));
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
            "tree_hash": tree,
            "worker_provider": worker_provider,
            "variant": variant,
            "experiment_id": experiment_id,
        });
        if let Some(rev) = &overlay_rev {
            row["overlay_rev"] = json!(rev);
        }
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
    fn a_timeout_exit_reads_started_and_scores_a_failed_grade() {
        assert!(spawn_exit_timed_out(124, "claude -p timed out after 60.0s"));
        assert!(spawn_exit_timed_out(12, "opencode run timed out after 60s"));
        assert!(!spawn_exit_timed_out(12, "registry read failed: gone"));
        assert!(!spawn_exit_timed_out(1, "timed out"));

        let stall = json!({"obs": {
            "fixture_prepared": true, "worker_required": true, "worker_started": true,
            "worker_timed_out": true, "grader_ran": false,
        }});
        let verdict = crate::eval_attempt::classify(&stall, None);
        assert_eq!((verdict.status, verdict.graded), ("graded", Some(false)));
        assert!(!verdict.retryable);

        let refused = json!({"obs": {
            "fixture_prepared": true, "worker_required": true, "worker_started": false,
            "worker_timed_out": true, "grader_ran": false,
        }});
        assert_eq!(
            crate::eval_attempt::classify(&refused, None).status,
            "unavailable",
            "an old row with no started worker keeps its verdict"
        );
    }

    #[test]
    fn the_tree_hash_follows_content_and_leaves_the_real_index_alone() {
        let d = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(d.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(d.path().join("a"), "one\n").unwrap();
        git(&["add", "a"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "c",
        ]);
        let base = tree_hash(d.path()).unwrap();

        std::fs::write(d.path().join("decision.md"), "decision: A\n").unwrap();
        let answered = tree_hash(d.path()).unwrap();
        assert_ne!(base, answered);
        assert_eq!(answered, tree_hash(d.path()).unwrap());
        assert_eq!(git(&["diff", "--cached", "--name-only"]), "");
        assert!(git(&["status", "--porcelain"]).contains("?? decision.md"));
    }

    #[test]
    fn a_lane_resolves_from_the_inventory_rows_and_a_miss_names_the_declared_ones() {
        let inv = serde_json::json!({"rows": [
            {"name": "zai-flash", "harness": "claude", "route": "zai/glm", "account": "zai"},
            {"name": "codex-luna", "harness": "codex", "route": "", "account": "codex-main"},
        ]});
        let lane = lane_from_inventory(&inv, "zai-flash").unwrap();
        assert_eq!(
            (
                lane.harness.as_str(),
                lane.route.as_str(),
                lane.account.as_str()
            ),
            ("claude", "zai/glm", "zai")
        );
        let miss = lane_from_inventory(&inv, "nope").err().unwrap();
        assert!(miss.contains("['codex-luna', 'zai-flash']"), "{miss}");
    }

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
            Some("codex"),
            Some(&lane),
            "do the thing",
            &[],
        );
        let joined = argv.join(" ");
        assert!(
            joined.contains("--harness codex"),
            "the resolved lane harness rides the argv: {joined}"
        );
        assert!(joined.contains("--route openai/gpt-5.6"));
        assert!(joined.contains("--account acme"));
        assert!(joined.ends_with("-- do the thing"));
    }

    #[test]
    fn the_provider_default_rides_when_no_lane_names_a_harness() {
        let argv = spawn_argv("n", "/tmp/wt", 60, Some("codex"), None, "p", &[]);
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--harness" && w[1] == "codex"));
        let bare = spawn_argv("n", "/tmp/wt", 60, None, None, "p", &[]);
        assert!(!bare.contains(&"--harness".to_string()));
    }

    #[test]
    fn a_claude_worker_loads_only_the_tree_behind_the_read_fence() {
        let tree = std::path::Path::new("/t/fno-evals/g-1-2");
        let home = std::path::Path::new("/h");
        let flags = tree_only_flags(Some("claude"), tree, home);
        assert_eq!(
            flags[..3],
            ["--setting-sources", "project", "--strict-mcp-config"]
        );
        let settings: serde_json::Value =
            serde_json::from_str(flags[3].strip_prefix("--settings=").unwrap()).unwrap();
        assert_eq!(
            settings["permissions"]["blockReadsOutsideWorkingDirectories"],
            true
        );
        assert_eq!(settings["sandbox"]["allowUnsandboxedCommands"], false);
        assert_eq!(
            settings["sandbox"]["filesystem"]["denyRead"],
            json!(["/h", "/t/fno-evals"])
        );
        assert_eq!(
            settings["sandbox"]["filesystem"]["allowRead"],
            json!(["/t/fno-evals/g-1-2"])
        );
        assert!(tree_only_flags(Some("codex"), tree, home).is_empty());
        assert!(tree_only_flags(None, tree, home).is_empty());

        let argv = spawn_argv("n", "/tmp/wt", 60, Some("claude"), None, "go", &flags);
        let fence = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(argv[fence - 1], "go", "the prompt sits before the fence");
        assert_eq!(argv[fence + 1..], flags[..]);
    }

    #[test]
    fn a_sealed_tree_keeps_no_link_to_the_repo_and_no_answer_key() {
        let d = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let repo = d.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::create_dir_all(repo.join(OVERLAY_PATH)).unwrap();
        std::fs::create_dir_all(repo.join("evals/bank")).unwrap();
        std::fs::write(repo.join(OVERLAY_PATH).join("f.py"), "old").unwrap();
        std::fs::write(repo.join("evals/bank/g.yaml"), "expected: x").unwrap();
        std::fs::write(repo.join("AGENTS.md"), "rules").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "old"]);
        let old = git(&["rev-parse", "HEAD"]);
        std::fs::write(repo.join(OVERLAY_PATH).join("f.py"), "new").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "new"]);
        let new = git(&["rev-parse", "HEAD"]);

        let (tree, head) =
            make_sealed_tree(&repo, &old, Some(new.as_str()), "golden-x", base.path()).unwrap();

        let in_tree = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&tree)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert!(tree.join(".git").is_dir());
        assert_eq!(in_tree(&["rev-list", "--count", "HEAD"]), "1");
        assert_eq!(in_tree(&["rev-parse", "HEAD"]), head);
        assert_eq!(in_tree(&["remote"]), "");
        assert_eq!(in_tree(&["status", "--porcelain"]), "");
        assert!(!tree.join(".git/objects/info/alternates").exists());
        assert!(!tree.join("evals/bank").exists());
        assert_eq!(
            std::fs::read_to_string(tree.join(OVERLAY_PATH).join("f.py")).unwrap(),
            "new",
            "the overlay ref wins"
        );
        let config = std::fs::read_to_string(tree.join(".git/config")).unwrap();
        assert!(!config.contains(repo.to_str().unwrap()), "{config}");
    }

    #[test]
    fn a_sealed_base_inside_home_refuses() {
        let home = tempfile::tempdir().unwrap();
        let err = sealed_base(&home.path().join("tmp"), home.path()).unwrap_err();
        assert!(err.contains("inside the home dir"), "{err}");
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(sealed_base(elsewhere.path(), home.path()).is_ok());
    }

    #[test]
    fn the_sweep_clears_only_a_dead_owners_sealed_tree() {
        let base = tempfile::tempdir().unwrap();
        let live = base
            .path()
            .join(format!("golden-x-{}-1", std::process::id()));
        let dead = base.path().join(format!("golden-x-{}-1", i32::MAX));
        std::fs::create_dir(&live).unwrap();
        std::fs::create_dir(&dead).unwrap();

        assert_eq!(sweep_sealed_trees(base.path()), 1);
        assert!(live.exists());
        assert!(!dead.exists());
    }

    #[test]
    fn only_an_old_trial_transcript_dir_is_pruned() {
        let d = tempfile::tempdir().unwrap();
        let trial = d.path().join("-h-evals-golden-1-2");
        let session = d.path().join("-h-code-repo");
        std::fs::create_dir(&trial).unwrap();
        std::fs::create_dir(&session).unwrap();

        prune_kept_transcripts(d.path(), "-h-evals", 1);
        assert!(trial.exists(), "a young trial stays");

        std::thread::sleep(std::time::Duration::from_millis(5));
        prune_kept_transcripts(d.path(), "-h-evals", 0);
        assert!(!trial.exists());
        assert!(session.exists(), "a real session never matches");
    }

    #[test]
    fn overlay_carries_golden_fixtures_and_never_the_bank() {
        let d = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(d.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "old"]);
        let old = git(&["rev-parse", "HEAD"]);
        std::fs::create_dir_all(d.path().join(OVERLAY_PATH)).unwrap();
        std::fs::create_dir_all(d.path().join("evals/bank")).unwrap();
        std::fs::write(d.path().join(OVERLAY_PATH).join("f.py"), "x").unwrap();
        std::fs::write(d.path().join("evals/bank/g.yaml"), "expected: x").unwrap();
        git(&["add", "evals"]);
        git(&["commit", "-q", "-m", "new"]);
        let new = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", "--detach", &old]);

        let head = apply_overlay(d.path(), &new).unwrap();

        assert_eq!(head, git(&["rev-parse", "HEAD"]));
        assert!(d.path().join(OVERLAY_PATH).join("f.py").exists());
        assert!(!d.path().join("evals/bank/g.yaml").exists());
        assert_eq!(
            git(&["status", "--porcelain"]),
            "",
            "the worker sees a clean tree"
        );
    }

    #[test]
    fn a_lane_flags_ride_even_when_its_harness_is_empty() {
        let lane = LaneRow {
            name: "r".into(),
            harness: String::new(),
            route: "zai/glm".into(),
            account: "a".into(),
        };
        let argv = spawn_argv("n", "/tmp/wt", 60, Some("claude"), Some(&lane), "p", &[]);
        let i = argv.iter().position(|a| a == "--harness").unwrap();
        assert_eq!(argv[i + 1], "claude");
        assert!(argv.contains(&"--route".to_string()));
        assert!(argv.contains(&"zai/glm".to_string()));
        assert!(argv.contains(&"--account".to_string()));
    }
}
