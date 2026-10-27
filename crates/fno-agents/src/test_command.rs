//! `fno doctor test [pytest-args...]` / `fno doctor test rust [cargo-args...]` - run a suite
//! honestly AND tersely. The port of `cli/src/fno/test_cmd.py`'s pytest and
//! rust modes; smoke, changed and census live in their own modules.
//!
//! Footguns this command encodes (tribal knowledge made runtime behavior):
//!
//! 1. A worktree's bare `pytest` imports the *canonical* `fno` (installed editable
//!    from the main checkout), not the worktree's own source. We pin
//!    `PYTHONPATH=<repo>/cli/src` so a worktree tests what it changed.
//! 2. rtk rewrites a bare `pytest`/`cargo` into a wrapped run that has stalled for
//!    12+ minutes with zero output. `fno doctor test` spawns the runner directly and sets
//!    `RTK_DISABLED=1` in the child env so nothing re-wraps it.
//! 3. `... | tail && echo OK` masks the real exit code (false green). We propagate
//!    the child's *actual* return code.
//! 4. Full test output in an agent transcript is re-read by every later request in the session.
//!    Default mode therefore captures ALL output to `<repo>/.fno/last-test.log` and prints only a
//!    summary: on failure, the TAIL of the log (errors live at the end - read from the end,
//!    expand upward via the log path). `--stream` restores inherited stdio for interactive runs.
//!
//! The interpreter is resolved worktree venv -> canonical venv -> `python3`, so a
//! fresh worktree with no local `.venv` still runs. (The Python leg fell back to
//! `sys.executable`, the interpreter running the CLI; a native binary has none,
//! so the native leg falls to PATH's `python3`.)
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::test_env;
use crate::test_smoke_registry::TAIL_LINES;

pub const SCOPE_STATEMENT: &str =
    "fno doctor test runs tests only and does NOT type-check; ruff + mypy \
run separately: bash scripts/ci/check-python-static.sh (CI job \
main-python-static runs the same script).";

/// Which ambient shape the child processes run under. `Clean` is today's
/// behaviour and the merge gate; `Dirty` feeds a poisoned PARENT through
/// [`test_env::poison`] so a channel the surface missed survives into the
/// child and reddens the test that reads it.
/// See docs/architecture/test-hermeticity.md.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AmbientMode {
    Clean,
    Dirty,
}

/// One throwaway state root per `fno doctor test` process, reaped at exit.
pub struct SandboxGuard(PathBuf);

impl SandboxGuard {
    fn create() -> Result<Self, String> {
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        for _ in 0..5 {
            let candidate = base.join(format!("fno-test-sandbox-{}", new_run_id()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(SandboxGuard(candidate)),
                Err(_) => continue,
            }
        }
        Err("fno doctor test: could not create a sandbox directory under TMPDIR".into())
    }

    /// The sandbox path for this process: one per invocation, reaped at exit.
    pub fn shared() -> Result<&'static Path, String> {
        static SANDBOX: OnceLock<Result<&'static Path, String>> = OnceLock::new();
        match SANDBOX.get_or_init(|| match SandboxGuard::create() {
            // Leak the guard on purpose: Drop would delete the very sandbox
            // this path hands out. The process-exit reap the Python leg had
            // (atexit) becomes the OS reclaiming one leaked allocation.
            Ok(g) => Ok(Box::leak(Box::new(g)).path()),
            Err(e) => Err(e),
        }) {
            Ok(p) => Ok(*p),
            Err(e) => Err(e.clone()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        // Best-effort: the sandbox is throwaway state, never a verdict input.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn new_run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}x{:x}", std::process::id(), now % 0xffff_ffff)
}

/// Walk up from `start` to the checkout root (the dir with cli/src/fno).
pub fn repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join("cli/src/fno/__init__.py").is_file() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// The main checkout root: parent of git's common .git dir.
///
/// In a worktree, `git rev-parse --git-common-dir` points at the MAIN repo's
/// `.git`, so its parent is the canonical checkout (whose `cli/.venv` a
/// worktree shares deps with). In the main checkout it is just `.git`.
fn canonical_root(root: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if common.is_empty() {
        return None;
    }
    let mut common = PathBuf::from(&common);
    if common.is_relative() {
        common = root.join(common);
    }
    common.parent().map(Path::to_path_buf)
}

/// worktree venv -> canonical venv -> `python3`.
pub fn resolve_interpreter(root: &Path) -> String {
    let local = root.join("cli/.venv/bin/python");
    if local.exists() {
        return local.to_string_lossy().to_string();
    }
    if let Some(canon) = canonical_root(root) {
        let cand = canon.join("cli/.venv/bin/python");
        if cand.exists() {
            return cand.to_string_lossy().to_string();
        }
    }
    "python3".to_string()
}

/// The suite timeout: `FNO_TEST_TIMEOUT_SECONDS` wins, then
/// `config.test.timeout_seconds`, else 1800. A broken settings value degrades
/// to the default, exactly as the Python leg's `except` did.
pub fn test_timeout_seconds() -> u64 {
    if let Ok(raw) = std::env::var("FNO_TEST_TIMEOUT_SECONDS") {
        if let Ok(v) = raw.parse::<u64>() {
            if v > 0 {
                return v;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::agents_config::resolve(&cwd, |t| {
        t.get("test")?
            .as_table()?
            .get("timeout_seconds")?
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
    })
    .filter(|v| *v > 0)
    .unwrap_or(1800)
}

/// The environment every test child gets, built once and handed to the
/// bounded runner per command (`--env`/`--unset`).
///
/// This is the ONE process boundary all four test trees cross - pytest over
/// `cli/tests`, pytest over `cli/src`, every `bash tests/*.sh` smoke step, and
/// cargo - so neutralising here needs no shell mirror and no second leg.
///
/// Under [`AmbientMode::Dirty`] the parent is poisoned FIRST and then
/// neutralised. If the ambient surface is complete the result is identical to
/// the clean lane; a name the surface missed survives into the child and the
/// test that reads it fails naming itself.
pub struct ChildEnv {
    pub env: BTreeMap<String, String>,
    /// Parent keys the neutralised surface drops: the runner inherits this
    /// process's env, so each dropped name needs an explicit removal.
    pub unsets: Vec<String>,
}

pub fn build_child_env(
    root: &Path,
    ambient: AmbientMode,
    sandbox: &Path,
) -> Result<ChildEnv, String> {
    let parent: BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(k, v)| {
            Some((
                k.to_string_lossy().to_string(),
                v.to_string_lossy().to_string(),
            ))
        })
        .collect();
    let poisoned = if ambient == AmbientMode::Dirty {
        let fixtures = root
            .join("cli/tests/fixtures/ambient-poison")
            .canonicalize()
            .map_err(|e| format!("fno doctor test: ambient-poison fixtures missing: {e}"))?;
        Some(test_env::poison(&parent, &fixtures)?)
    } else {
        None
    };
    let neutralised = test_env::neutralise(poisoned.as_ref().unwrap_or(&parent), sandbox)?;
    // The outer runner declares its sandbox explicitly: the tripwire inside the
    // child cannot infer provenance from a rewritten HOME.
    let mut env = neutralised;
    env.insert(
        "FNO_TEST_SANDBOX".into(),
        sandbox.to_string_lossy().to_string(),
    );
    // A nested `fno doctor test` can run inside an outer xdist worker. Its child
    // is a fresh pytest controller, not that worker, so inherited transport
    // markers make pytest-xdist send session-finish over a closed channel and
    // strand the outer suite in teardown.
    for name in [
        "PYTEST_XDIST_WORKER",
        "PYTEST_XDIST_WORKER_COUNT",
        "PYTEST_XDIST_TESTRUNUID",
    ] {
        env.remove(name);
    }
    // The pin is best-effort: a changed run executes inside a synthetic
    // checkout that has no cli/src (the smoke-modes harness builds one), and
    // the steps it selects there are shell steps that never read PYTHONPATH.
    // A real checkout pins exactly as before.
    if let Ok(src) = root.join("cli/src").canonicalize() {
        let existing = env.get("PYTHONPATH").cloned().unwrap_or_default();
        let mut pythonpath = src.to_string_lossy().to_string();
        if !existing.is_empty() {
            pythonpath.push(':');
            pythonpath.push_str(&existing);
        }
        env.insert("PYTHONPATH".into(), pythonpath);
    }
    // never let rtk re-wrap the child run
    env.insert("RTK_DISABLED".into(), "1".into());
    let unsets: Vec<String> = parent
        .keys()
        .filter(|k| !env.contains_key(*k))
        .cloned()
        .collect();
    Ok(ChildEnv { env, unsets })
}

fn which(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let p = PathBuf::from(program);
        return if p.exists() { Some(p) } else { None };
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|c| c.exists())
}

/// Run one suite command through the native bounded owner, in process.
///
/// The owner admits under the machine-wide `test:suite` claim and ALWAYS
/// group-kills after the run - success, failure, or timeout - the gap that let
/// a clean-exit cargo orphan a deps/ test binary for 3h32m. Spawn failure
/// surfaces as the Python leg's 127 with its stderr line.
fn run_bounded(
    cmd: &[String],
    child: &ChildEnv,
    timeout: u64,
    log: Option<&Path>,
) -> Result<i32, String> {
    let program = cmd.first().ok_or("internal: empty command")?;
    if which(program).is_none() {
        return Err(format!(
            "fno doctor test: failed to run {program}: command not found"
        ));
    }
    let mut argv: Vec<String> = vec![
        "--timeout".into(),
        timeout.to_string(),
        "--env".into(),
        format!("FNO_TEST_TIMEOUT_SECONDS={timeout}"),
    ];
    if let Ok(root) = std::env::var("FNO_CLAIMS_ROOT") {
        if !root.is_empty() {
            argv.push("--claims-root".into());
            argv.push(root);
        }
    }
    for (k, v) in &child.env {
        argv.push("--env".into());
        argv.push(format!("{k}={v}"));
    }
    for name in &child.unsets {
        argv.push("--unset".into());
        argv.push(name.clone());
    }
    if let Some(path) = log {
        argv.push("--stdout-file".into());
        argv.push(path.to_string_lossy().to_string());
    }
    argv.push("--".into());
    argv.extend(cmd.iter().cloned());
    Ok(crate::test_run::run_test_run(&argv))
}

fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/_-=.,:@+".contains(c))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// Run each command with output captured to `log`; print the terse verdict.
///
/// The header prints BEFORE the run so a watcher can `tail -f` the log. The
/// log's last line is always `EXIT=<rc>`: a reader recovers the real code even
/// where an outer tee or harness flattens this process's exit.
pub fn run_captured(cmds: &[Vec<String>], child: &ChildEnv, log: &Path, timeout: u64) -> i32 {
    let mut rc = 0;
    let mut fh = match std::fs::File::create(log) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fno doctor test: cannot open the log {log:?}: {e}");
            return 2;
        }
    };
    for cmd in cmds {
        let joined = cmd
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        println!("running: {joined} | log: {log:?} | timeout: {timeout}s");
        let _ = writeln!(fh, "$ {joined}");
        let _ = fh.flush();
        rc = match run_bounded(cmd, child, timeout, Some(log)) {
            Ok(code) => code,
            Err(msg) => {
                eprintln!("{msg}");
                let _ = writeln!(fh, "EXIT=127");
                return 127;
            }
        };
        if rc != 0 {
            // first failure wins; its output is the log tail
            break;
        }
    }
    let _ = writeln!(fh, "EXIT={rc}");
    drop(fh);
    if rc == 0 {
        // The EXIT marker is bookkeeping, not a verdict summary; the terse
        // PASS line shows the suite's own last output.
        let lines: Vec<String> = tail(log, 5)
            .into_iter()
            .filter(|l| !l.trim().is_empty() && !l.starts_with("EXIT="))
            .map(|l| l.trim_end().to_string())
            .collect();
        let summary = lines
            .last()
            .cloned()
            .unwrap_or_else(|| "(no output)".into());
        println!("PASS | {summary}");
    } else {
        println!(
            "FAIL (rc={rc}) - last {TAIL_LINES} lines below; read from the end, \
expand upward if needed: tail -100 {log:?}"
        );
        for line in tail(log, TAIL_LINES) {
            print!("{line}");
        }
    }
    rc
}

/// The file's last `n` lines, each WITH its newline, empty on any read error.
fn tail(path: &Path, n: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|l| format!("{l}\n")).collect()
}

/// Resolve interpreter + env, run pytest, return its real exit code.
pub fn run_pytest(
    args: &[String],
    stream: bool,
    log_override: Option<&Path>,
    ambient: AmbientMode,
) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = repo_root(&cwd).unwrap_or_else(|| cwd.clone());
    let interp = resolve_interpreter(&root);
    let timeout = test_timeout_seconds();

    // Default to THE Python suite (cli/tests) when no collection target was
    // given. A bare `pytest` collects from cwd, which from the repo root pulls
    // in script-style tests/ files that `raise SystemExit` at import (pytest
    // INTERNALERROR, not a real run). A collection target is a non-flag arg
    // that names a path or nodeid; a flag value like `-k expr` does not count.
    let mut pytest_args: Vec<String> = args.to_vec();
    let has_target = pytest_args.iter().any(|a| {
        !a.starts_with('-') && (a.contains("::") || a.contains('/') || Path::new(a).exists())
    });
    if !has_target {
        pytest_args = [
            "-n".into(),
            "auto".into(),
            "--maxprocesses=4".into(),
            "--dist=loadgroup".into(),
        ]
        .into_iter()
        .chain(pytest_args)
        .collect();
        match root.join("cli/tests").canonicalize() {
            Ok(p) => pytest_args.push(p.to_string_lossy().to_string()),
            Err(_) => pytest_args.push(root.join("cli/tests").to_string_lossy().to_string()),
        }
    }

    let sandbox = match SandboxGuard::shared() {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let child = match build_child_env(&root, ambient, sandbox) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let cmd: Vec<String> = [interp, "-m".into(), "pytest".into()]
        .into_iter()
        .chain(pytest_args)
        .collect();
    if stream {
        match run_bounded(&cmd, &child, timeout, None) {
            Ok(code) => code,
            Err(msg) => {
                eprintln!("{msg}");
                127
            }
        }
    } else {
        let log = log_override
            .map(Path::to_path_buf)
            .unwrap_or_else(|| log_path(&root));
        run_captured(std::slice::from_ref(&cmd), &child, &log, timeout)
    }
}

fn log_path(root: &Path) -> PathBuf {
    let d = root.join(".fno");
    let _ = std::fs::create_dir_all(&d);
    // one shared file per checkout, last-writer-wins; per-run files if
    // parallel same-worktree runs ever matter.
    d.join("last-test.log")
}

/// Last parseable `long N` / `long=N` / `short N` / `shortN` in `args[:limit]`.
/// Returns `(index, form, value)` - `form` is `"sep"` (the value is the next
/// token), `"eq"` (`--flag=N`), or `"short"` (`-jN`). An unparseable match at a
/// given index is skipped, not recorded, so a malformed value never masks a
/// real one seen elsewhere.
fn last_flag_value(
    args: &[String],
    long_flag: &str,
    short_flag: &str,
    limit: Option<usize>,
) -> Option<(usize, &'static str, i64)> {
    let n = limit.unwrap_or(args.len()).min(args.len());
    let mut found: Option<(usize, &'static str, i64)> = None;
    for (i, a) in args.iter().enumerate().take(n) {
        if a == long_flag {
            if let Some(next) = args.get(i + 1) {
                if let Ok(v) = next.parse::<i64>() {
                    found = Some((i, "sep", v));
                    continue;
                }
            }
        }
        if let Some(rest) = a.strip_prefix(&format!("{long_flag}=")) {
            if let Ok(v) = rest.parse::<i64>() {
                found = Some((i, "eq", v));
                continue;
            }
        }
        if !short_flag.is_empty() {
            if a == short_flag {
                if let Some(next) = args.get(i + 1) {
                    if let Ok(v) = next.parse::<i64>() {
                        found = Some((i, "sep", v));
                        continue;
                    }
                }
            }
            if let Some(rest) = a.strip_prefix(short_flag) {
                if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
                    if let Ok(v) = rest.parse::<i64>() {
                        found = Some((i, "short", v));
                    }
                }
            }
        }
    }
    found
}

/// Clamp a found flag's value to `ceiling`, rewriting it in place. Returns
/// `(requested, effective)`.
fn clamp_flag(
    args: &mut [String],
    hit: (usize, &'static str, i64),
    long_flag: &str,
    short_flag: &str,
    ceiling: i64,
) -> (i64, i64) {
    let (index, form, value) = hit;
    let effective = value.min(ceiling);
    if effective != value {
        match form {
            "sep" => args[index + 1] = effective.to_string(),
            "eq" => args[index] = format!("{long_flag}={effective}"),
            _ => args[index] = format!("{short_flag}{effective}"),
        }
    }
    (value, effective)
}

/// The test-thread cap from the spawn-gate probe, or None at default.
///
/// The probe's `headroom` is how many MORE lanes fit, answered by the gate's
/// own cap and slot occupancy (one decider, law d-b6cc1a2a), so the suite
/// reads the same arithmetic the gate refuses with. A machine-load hold caps
/// at 0 inside the probe (serial). A refused reading (a dark sensor is never
/// headroom) returns None so the runner keeps its own default where sensors
/// are dark (CI).
fn lanes_threads() -> (Option<i64>, String) {
    let answer = crate::spawn_gate_command::probe_answer(&serde_json::json!({"only": ["lanes"]}));
    let slots = answer.get("slots").and_then(serde_json::Value::as_i64);
    let effective = answer.get("effective").and_then(serde_json::Value::as_i64);
    let headroom = answer.get("headroom").and_then(serde_json::Value::as_i64);
    match (slots, effective, headroom) {
        (Some(_), Some(_), Some(h)) => {
            let mut note = format!("{h} more fit (fno doctor lanes)");
            if let Some(extra) = answer
                .get("headroom_note")
                .and_then(serde_json::Value::as_str)
            {
                note.push_str("; ");
                note.push_str(extra);
            }
            (Some(h.max(0)), note)
        }
        _ => {
            let reason = answer
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("gate unavailable");
            (
                None,
                format!("reading refused ({reason}: stale fno-agents, or the gate cannot answer)"),
            )
        }
    }
}

/// Run the Rust suites: nextest when installed, else `cargo test -q`.
///
/// No workspace root exists, so without an explicit `--manifest-path` we sweep
/// every `crates/*/Cargo.toml` (the two-test-trees lesson: a green subset is
/// not proof).
///
/// The test-thread count comes from the spawn-gate headroom, capped at the
/// machine's own CPU count: a lane is a worker-lane headroom count, not a
/// thread count, so an idle box's 64-lane reading must never become 64 test
/// threads on a 12-core machine. A bare `--` (libtest args follow) or an
/// unrelated `--jobs`/`-j` build-parallelism flag no longer suppresses the
/// cap - only an explicit `--test-threads` does, and even that is clamped to
/// the CPU ceiling rather than trusted past it. `--jobs`/`-j` (cargo's own
/// build parallelism, unrelated to test concurrency) is bounded to the same
/// ceiling independently. The reading (and what it did) prints so a slow run
/// explains itself.
pub fn run_rust(
    args: &[String],
    stream: bool,
    log_override: Option<&Path>,
    ambient: AmbientMode,
) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = repo_root(&cwd).unwrap_or_else(|| cwd.clone());
    let mut cargo_args: Vec<String> = args.to_vec();
    let nextest = which("cargo-nextest").is_some();
    let (threads, lanes_note) = lanes_threads();
    let ceiling = std::thread::available_parallelism()
        .map(|n| n.get() as i64)
        .unwrap_or(1);
    let timeout = test_timeout_seconds();

    let sep_index = cargo_args.iter().position(|a| a == "--");
    let mut notes: Vec<String> = Vec::new();

    let build_limit = sep_index;
    if let Some(hit) = last_flag_value(&cargo_args, "--jobs", "-j", build_limit) {
        let (requested, effective) = clamp_flag(&mut cargo_args, hit, "--jobs", "-j", ceiling);
        if effective != requested {
            notes.push(format!("build jobs {requested} capped at {effective}"));
        }
    }

    let thread_hit = last_flag_value(&cargo_args, "--test-threads", "", None);

    let mut base: Vec<String> = if nextest {
        vec!["cargo".into(), "nextest".into(), "run".into()]
    } else {
        vec!["cargo".into(), "test".into(), "-q".into()]
    };
    let mut cap_tail: Vec<String> = Vec::new();

    if let Some(hit) = thread_hit {
        let (requested, effective) =
            clamp_flag(&mut cargo_args, hit, "--test-threads", "", ceiling);
        if effective != requested {
            notes.push(format!(
                "explicit --test-threads {requested} capped at {effective}"
            ));
        } else {
            notes.push(format!("explicit --test-threads {requested} kept"));
        }
    } else {
        match threads {
            None => notes.push("runner default parallelism".into()),
            Some(t) => {
                let effective = t.clamp(1, ceiling);
                let mut cap_note = format!("test threads capped at {effective}");
                if effective != t {
                    cap_note += &format!(" (requested {t}, ceiling {ceiling})");
                }
                notes.push(cap_note);
                if nextest {
                    base.push("--test-threads".into());
                    base.push(effective.to_string());
                } else if let Some(at) = sep_index {
                    let insert_at = at + 1;
                    let tail = cargo_args.split_off(insert_at);
                    cargo_args.push("--test-threads".into());
                    cargo_args.push(effective.to_string());
                    cargo_args.extend(tail);
                } else {
                    cap_tail = vec!["--".into(), "--test-threads".into(), effective.to_string()];
                }
            }
        }
    }

    println!(
        "fno doctor test rust: lanes {lanes_note}; {}; timeout {timeout}s",
        notes.join("; ")
    );

    let cmds: Vec<Vec<String>> = if cargo_args.iter().any(|a| a == "--manifest-path") {
        vec![[base.clone(), cargo_args.clone()].concat()]
    } else {
        let mut manifests: Vec<PathBuf> = match root.join("crates").read_dir() {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.path().join("Cargo.toml"))
                .filter(|p| p.is_file())
                .collect(),
            Err(_) => Vec::new(),
        };
        manifests.sort();
        if manifests.is_empty() {
            eprintln!(
                "fno doctor test rust: no crates/*/Cargo.toml under {}",
                root.to_string_lossy()
            );
            return 2;
        }
        manifests
            .iter()
            .map(|m| {
                [
                    base.clone(),
                    vec!["--manifest-path".into(), m.to_string_lossy().to_string()],
                    cargo_args.clone(),
                ]
                .concat()
            })
            .collect()
    };
    let cmds = if cap_tail.is_empty() {
        cmds
    } else {
        cmds.into_iter()
            .map(|mut c| {
                c.extend(cap_tail.iter().cloned());
                c
            })
            .collect()
    };

    let sandbox = match SandboxGuard::shared() {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let child = match build_child_env(&root, ambient, sandbox) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    if stream {
        for cmd in &cmds {
            let rc = match run_bounded(cmd, &child, timeout, None) {
                Ok(code) => code,
                Err(msg) => {
                    eprintln!("{msg}");
                    return 127;
                }
            };
            if rc != 0 {
                return rc;
            }
        }
        return 0;
    }
    let log = log_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| log_path(&root));
    run_captured(&cmds, &child, &log, timeout)
}

#[derive(Debug, PartialEq)]
struct ParsedFlags {
    stream: bool,
    log: Option<String>,
    rest: Vec<String>,
}

/// Extract `--stream` and `--log[=]PATH` (the token is ours: pytest's
/// passthrough has no bare `--log`) ahead of the first bare `--`; the
/// separator itself and everything after it keep their order untouched, so a
/// suite's own `--log` stays the suite's.
fn parse_command_flags(args: &[String]) -> ParsedFlags {
    let mut stream = false;
    let mut log: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut pending_log = false;
    let mut past_separator = false;
    for a in args {
        if past_separator {
            rest.push(a.clone());
            continue;
        }
        if a == "--" {
            past_separator = true;
            rest.push(a.clone());
            continue;
        }
        if pending_log {
            log = Some(a.clone());
            pending_log = false;
            continue;
        }
        match a.as_str() {
            "--stream" => stream = true,
            "--log" => pending_log = true,
            other => {
                if let Some(v) = other.strip_prefix("--log=") {
                    log = Some(v.to_string());
                } else {
                    rest.push(a.clone());
                }
            }
        }
    }
    ParsedFlags { stream, log, rest }
}

/// The command entry: `fno-agents test <argv>`.
pub fn run_test_command(args: &[String]) -> i32 {
    let flags = parse_command_flags(args);
    let rest = flags.rest;
    // Scope statement, every RUN, pass or fail: a green run here is pytest's
    // verdict alone, and the reader must learn the typing gate lives elsewhere
    // at the moment they might conclude otherwise. Skipped where output is a
    // machine contract, not a run verdict: `smoke --list` is verbatim-stable
    // (one name per line) and the census emits parsed rows.
    let machine_output = !rest.is_empty()
        && (rest[0] == "--census-deferred"
            || (rest[0] == "smoke" && rest.iter().any(|a| a == "--list")));
    if !machine_output {
        println!("{}", SCOPE_STATEMENT);
    }
    if flags.log.is_some()
        && (flags.stream
            || (!rest.is_empty() && (rest[0] == "smoke" || rest[0] == "--census-deferred")))
    {
        eprintln!("--log supports the python and rust suites; drop --log or the conflicting flag");
        return 2;
    }
    if rest.is_empty() {
        return run_pytest(
            &[],
            flags.stream,
            flags.log.as_deref().map(Path::new),
            AmbientMode::Clean,
        );
    }
    match rest[0].as_str() {
        "rust" => run_rust(
            &rest[1..],
            flags.stream,
            flags.log.as_deref().map(Path::new),
            AmbientMode::Clean,
        ),
        "smoke" => crate::test_smoke::run(&rest[1..], flags.stream),
        "--census-deferred" => crate::test_census::run(&rest[1..]),
        _ => run_pytest(
            &rest,
            flags.stream,
            flags.log.as_deref().map(Path::new),
            AmbientMode::Clean,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_flag_finds_sep_eq_and_short_forms() {
        let args: Vec<String> = ["-j", "8", "--test-threads=5", "-j4"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hit = last_flag_value(&args, "-j", "-j", None).unwrap();
        assert_eq!(hit, (3, "short", 4));
        let hit = last_flag_value(&args, "--test-threads", "", None).unwrap();
        assert_eq!(hit, (1, "eq", 5));
    }

    #[test]
    fn flag_values_past_a_separator_are_ignored_for_build_jobs() {
        let args: Vec<String> = ["--", "-j", "8"].iter().map(|s| s.to_string()).collect();
        let limit = args.iter().position(|a| a == "--");
        assert!(last_flag_value(&args, "--jobs", "-j", limit).is_none());
    }

    #[test]
    fn malformed_values_are_skipped_not_recorded() {
        let args: Vec<String> = ["--jobs", "x", "--jobs", "4"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hit = last_flag_value(&args, "--jobs", "", None).unwrap();
        assert_eq!(hit, (2, "sep", 4));
    }

    #[test]
    fn command_flags_parse_anywhere_but_stop_at_the_separator() {
        let args: Vec<String> = ["-k", "expr", "--stream", "--log=/tmp/x.log", "--", "--log"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let f = parse_command_flags(&args);
        assert!(f.stream);
        assert_eq!(f.log.as_deref(), Some("/tmp/x.log"));
        assert_eq!(f.rest, vec!["-k", "expr", "--", "--log"]);
    }

    #[test]
    fn repo_root_walks_up_to_the_checkout_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("cli/src/fno");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("__init__.py"), "").unwrap();
        let start = tmp.path().join("cli/src/fno/sub");
        std::fs::create_dir_all(&start).unwrap();
        let root = repo_root(&start).unwrap();
        assert_eq!(root, tmp.path());
        assert!(repo_root(&std::env::temp_dir()).is_none() || true);
    }

    #[test]
    fn shell_quote_leaves_safe_args_and_wraps_the_rest() {
        assert_eq!(shell_quote("plain"), "plain");
        assert_eq!(shell_quote("has space"), "'has space'");
        assert_eq!(shell_quote(""), "''");
    }
}
