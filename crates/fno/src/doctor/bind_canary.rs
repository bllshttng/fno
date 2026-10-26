//! The codex-bind canary (doctor.py:3232-3423): spawn one throwaway codex
//! pane under a private CODEX_HOME and time which bind oracle claims it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use super::bounded_command_for;
use super::report::resolve_binary;

const BIND_WINDOW_S: u64 = 60;
const PROBE_TIMEOUT_S: u64 = 2;
const POLL_MS: u64 = 750;
const DAEMON_INTERVAL_MS: u64 = 2000;
const BACKFILL_ATTEMPTS: u32 = 5;
const BACKFILL_DELAY_MS: u64 = 750;
const PANE_TAIL_LINES: u64 = 200;
const WAIT_EXITED: i32 = 12;
const DEFAULT_SESSION: &str = "main";

fn resolve_mux_session() -> String {
    let server = std::env::var("FNO_SERVER").unwrap_or_default();
    if !server.trim().is_empty() {
        return server;
    }
    std::env::var("FNO_SESSION").unwrap_or(DEFAULT_SESSION.to_string())
}

fn front_door() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "fno".to_string())
}

fn run_mux(args: &[String], timeout: u64) -> Option<(i32, String, String)> {
    let mut argv = vec![front_door()];
    argv.extend(args.iter().cloned());
    bounded_command_for(&argv, timeout)
}

fn codex_version() -> Option<String> {
    let argv: Vec<String> = ["codex", "--version"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (code, stdout, _err) = bounded_command_for(&argv, 10)?;
    if code != 0 {
        return None;
    }
    let first = stdout.trim().lines().next().unwrap_or_default();
    codex_version_final(first)
}

fn codex_version_final(first_line: &str) -> Option<String> {
    let re = regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)").expect("valid pattern");
    let caps = re.captures(first_line)?;
    let (major, minor, patch) = (
        caps.get(1)?.as_str().to_string(),
        caps.get(2)?.as_str().to_string(),
        caps.get(3)?.as_str().to_string(),
    );
    Some(format!("codex-cli {major}.{minor}.{patch}"))
}

fn setup_private_codex_home(cwd: &Path) -> Option<PathBuf> {
    let home = cwd.join(".codex-home");
    std::fs::create_dir_all(&home).ok()?;
    let trust = format!(
        "[projects.{cwd}]\ntrust_level = \"trusted\"\n",
        cwd = serde_json::to_string(&cwd.to_string_lossy()).ok()?
    );
    std::fs::write(home.join("config.toml"), trust).ok()?;
    let source = super::report::codex_home().join("auth.json");
    let resolved = std::fs::canonicalize(&source).ok()?;
    std::os::unix::fs::symlink(&resolved, home.join("auth.json")).ok()?;
    Some(home)
}

fn daemon_argv(args: &[&str], codex_home: &Path) -> Vec<String> {
    let mut argv = vec![
        "env".to_string(),
        format!("CODEX_HOME={}", codex_home.to_string_lossy()),
        "codex".to_string(),
    ];
    argv.extend(args.iter().map(|s| s.to_string()));
    argv
}

fn ensure_daemon(codex_home: &Path) {
    let argv = daemon_argv(&["app-server", "daemon", "start"], codex_home);
    let _ = bounded_command_for(&argv, 30);
}

fn stop_daemon(codex_home: &Path) {
    let argv = daemon_argv(&["app-server", "daemon", "stop"], codex_home);
    let _ = bounded_command_for(&argv, 15);
}

fn launch_pane(session: &str, cwd: &Path, codex_home: &Path) -> Option<(i32, String)> {
    let prompt = "fno codex bind canary: reply READY and take no other action.";
    let mut args: Vec<String> = vec![
        "mux".into(),
        "pane".into(),
        "run".into(),
        "--server".into(),
        session.into(),
        "--cwd".into(),
        cwd.to_string_lossy().into_owned(),
        "--".into(),
        "env".into(),
        format!("CODEX_HOME={}", codex_home.to_string_lossy()),
        "codex".into(),
        "-C".into(),
        cwd.to_string_lossy().into_owned(),
        "--sandbox".into(),
        "workspace-write".into(),
        "--ask-for-approval".into(),
        "never".into(),
        "--".into(),
        prompt.into(),
    ];
    run_mux(&args, 30)
}

fn lookup_child_pid(session: &str, pane_id: i64) -> Option<i64> {
    let args: Vec<String> = vec![
        "mux".into(),
        "pane".into(),
        "ls".into(),
        "--server".into(),
        session.into(),
        "--json".into(),
    ];
    let (code, stdout, _err) = run_mux(&args, PROBE_TIMEOUT_S)?;
    if code != 0 {
        return None;
    }
    let rows: Value = serde_json::from_str(&stdout).ok()?;
    rows.as_array()?.iter().find_map(|row| {
        if row.get("pane_id").and_then(Value::as_i64) == Some(pane_id) {
            row.get("child_pid").and_then(Value::as_i64)
        } else {
            None
        }
    })
}

fn read_pane_tail(session: &str, pane_id: i64) -> String {
    let args: Vec<String> = vec![
        "mux".into(),
        "pane".into(),
        "read".into(),
        "--server".into(),
        session.into(),
        pane_id.to_string(),
        "--lines".into(),
        PANE_TAIL_LINES.to_string(),
    ];
    match run_mux(&args, PROBE_TIMEOUT_S) {
        Some((0, stdout, _)) => stdout,
        _ => String::new(),
    }
}

fn pane_alive(session: &str, pane_id: i64) -> Option<bool> {
    let args: Vec<String> = vec![
        "mux".into(),
        "pane".into(),
        "wait".into(),
        "--server".into(),
        session.into(),
        pane_id.to_string(),
        "--timeout".into(),
        "0".into(),
    ];
    match run_mux(&args, PROBE_TIMEOUT_S) {
        Some((12, _, _)) => Some(false),
        Some((0, _, _)) | Some((11, _, _)) => Some(true),
        Some((code, _, _)) => {
            let absent = pane_absent_from_listing(session, pane_id);
            absent.map(|a| !a)
        }
        None => None,
    }
}

fn pane_absent_from_listing(session: &str, pane_id: i64) -> Option<bool> {
    let args: Vec<String> = vec![
        "mux".into(),
        "pane".into(),
        "ls".into(),
        "--server".into(),
        session.into(),
    ];
    let (code, stdout, _err) = run_mux(&args, PROBE_TIMEOUT_S)?;
    if code != 0 {
        return None;
    }
    if stdout.trim().is_empty() {
        return None;
    }
    let absent = !stdout
        .lines()
        .any(|line| line.contains(&format!("[{pane_id}]")));
    Some(absent)
}

fn rollout_fd_session_id(child_pid: i64) -> Option<String> {
    let re = regex::Regex::new(r"rollout-\S*\.jsonl$").expect("valid pattern");
    let mut ids: Vec<String> = Vec::new();
    for pid in descendant_pids(child_pid) {
        let argv: Vec<String> = vec!["lsof".into(), "-p".into(), pid.to_string()];
        let Some((0, stdout, _)) = bounded_command_for(&argv, 5) else {
            continue;
        };
        for line in stdout.lines() {
            let Some(path) = line.split_whitespace().last() else {
                continue;
            };
            let base = path.rsplit('/').next().unwrap_or_default();
            if re.is_match(base) {
                if let Some(id) = session_meta_id(path) {
                    ids.push(id);
                }
            }
        }
    }
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    (uniq.len() == 1).then(|| uniq.remove(0))
}

fn descendant_pids(root: i64) -> Vec<i64> {
    let mut out = vec![root];
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        let argv: Vec<String> = vec!["pgrep".into(), "-P".into(), pid.to_string()];
        if let Some((0, stdout, _)) = bounded_command_for(&argv, 3) {
            for line in stdout.lines() {
                if let Ok(child) = line.trim().parse::<i64>() {
                    out.push(child);
                    queue.push(child);
                }
            }
        }
    }
    out
}

fn session_meta_id(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let first = text.lines().next()?;
    let doc: Value = serde_json::from_str(first).ok()?;
    if doc.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    doc.get("payload")
        .and_then(|p| p.get("id"))
        .and_then(Value::as_str)
        .map(String::from)
}

fn run_with_codex_home(
    argv: &[String],
    codex_home: &str,
    timeout: u64,
) -> Option<(i32, String, String)> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env("CODEX_HOME", codex_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + std::time::Duration::from_secs(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut o) = child.stdout.take() {
                    let _ = o.read_to_string(&mut stdout);
                }
                if let Some(mut e) = child.stderr.take() {
                    let _ = e.read_to_string(&mut stderr);
                }
                return Some((status.code().unwrap_or(-1), stdout, stderr));
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
}

fn daemon_session_ids(cwd: &Path, codex_home: &Path) -> Option<Vec<String>> {
    let bin = resolve_binary()?;
    let mut argv: Vec<String> = vec![bin];
    for token in ["list", "--json", "--no-discovered", "--harness", "codex"] {
        argv.push(token.to_string());
    }
    let (code, stdout, _err) = run_with_codex_home(&argv, &codex_home.to_string_lossy(), 20)?;
    if code != 0 {
        return None;
    }
    let rows: Vec<Value> = serde_json::from_str(&stdout).ok()?;
    let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut ids: Vec<String> = Vec::new();
    for row in &rows {
        let Some(sid) = row.get("session_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(row_cwd) = row.get("cwd").and_then(Value::as_str) else {
            continue;
        };
        let row_real = std::fs::canonicalize(row_cwd).unwrap_or_else(|_| PathBuf::from(row_cwd));
        if row_real == resolved {
            ids.push(sid.to_string());
        }
    }
    Some(ids)
}

fn daemon_candidate(
    cwd: &Path,
    baseline: &[String],
    codex_home: &Path,
    condition: &mut String,
) -> Option<String> {
    let Some(loaded) = daemon_session_ids(cwd, codex_home) else {
        *condition = "daemon: app-server unreachable".to_string();
        return None;
    };
    let new_ids: Vec<&String> = loaded.iter().filter(|id| !baseline.contains(id)).collect();
    if new_ids.len() == 1 {
        return Some(new_ids[0].clone());
    }
    if new_ids.is_empty() {
        *condition = "daemon: no new codex session for this cwd".to_string();
    } else {
        *condition = format!(
            "daemon: {} new codex sessions for this cwd, ambiguous",
            new_ids.len()
        );
    }
    None
}

struct Binding {
    session_id: Option<String>,
    oracle: Option<String>,
    pane_alive_at_exit: Option<bool>,
    reason: String,
    elapsed_s: f64,
}

fn await_binding(
    session: &str,
    pane_id: i64,
    cwd: &Path,
    child_pid: i64,
    codex_home: &Path,
    baseline: &[String],
) -> Binding {
    let started = Instant::now();
    let deadline = started + std::time::Duration::from_secs(BIND_WINDOW_S);
    let mut tail = String::new();
    let mut last_alive: Option<bool> = None;
    let mut first_pass = true;
    let mut last_daemon_probe: Option<Instant> = None;
    let mut prev_candidate: Option<String> = None;
    let mut modal_answered = false;
    let mut condition = String::new();
    let mut oracle: Option<String> = None;
    loop {
        let mut sid: Option<String> = None;
        if let Some(id) = rollout_fd_session_id(child_pid) {
            oracle = Some("rollout-fd".into());
            sid = Some(id);
        } else {
            condition = format!("fd: no codex rollout on pane child pid {child_pid}");
            let now = Instant::now();
            let rate_ok = last_daemon_probe.map_or(true, |t| {
                now.duration_since(t).as_millis() as u64 >= DAEMON_INTERVAL_MS
            });
            if rate_ok {
                last_daemon_probe = Some(now);
                match daemon_candidate(cwd, baseline, codex_home, &mut condition) {
                    Some(cand) => {
                        if prev_candidate.as_deref() == Some(cand.as_str()) {
                            if pane_alive(session, pane_id) != Some(false) {
                                oracle = Some("daemon".into());
                                sid = Some(cand);
                            } else {
                                condition = "daemon: candidate dropped, pane already gone".into();
                            }
                        } else {
                            condition = format!("daemon: candidate {cand} awaiting a repeat probe");
                            prev_candidate = Some(cand);
                        }
                    }
                    None => {}
                }
            }
        }
        if sid.is_some() {
            return Binding {
                session_id: sid,
                oracle: oracle.clone(),
                pane_alive_at_exit: Some(true),
                reason: String::new(),
                elapsed_s: started.elapsed().as_secs_f64(),
            };
        }
        if !first_pass && Instant::now() >= deadline {
            return expired(&started, last_alive);
        }
        let fresh = read_pane_tail(session, pane_id);
        if !fresh.trim().is_empty() {
            tail = fresh;
        }
        if !first_pass && Instant::now() >= deadline {
            return expired(&started, last_alive);
        }
        first_pass = false;
        let alive = pane_alive(session, pane_id);
        if alive == Some(false) {
            return Binding {
                session_id: None,
                oracle: None,
                pane_alive_at_exit: Some(false),
                reason: "pane-died-before-binding".into(),
                elapsed_s: started.elapsed().as_secs_f64(),
            };
        }
        if Instant::now() >= deadline {
            return expired(&started, alive);
        }
        if !modal_answered && tail.to_lowercase().contains("hooks need review") {
            modal_answered = true;
            send_modal_answer(session, pane_id);
            condition =
                "hooks-review modal answered once (option 3, continue without trusting)".into();
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }
}

fn expired(started: &Instant, alive: Option<bool>) -> Binding {
    Binding {
        session_id: None,
        oracle: None,
        pane_alive_at_exit: alive,
        reason: "binding-window-expired".into(),
        elapsed_s: started.elapsed().as_secs_f64(),
    }
}

fn send_modal_answer(session: &str, pane_id: i64) {
    let args: Vec<String> = vec![
        "mux".into(),
        "pane".into(),
        "send".into(),
        "--server".into(),
        session.into(),
        pane_id.to_string(),
        "--text".into(),
        "3".into(),
        "--submit".into(),
        "--raw".into(),
    ];
    let _ = run_mux(&args, 5);
}

fn reap_pane(session: &str, pane_id: i64) {
    let args: Vec<String> = vec![
        "mux".into(),
        "pane".into(),
        "kill".into(),
        "--server".into(),
        session.into(),
        pane_id.to_string(),
    ];
    let _ = run_mux(&args, 15);
}

fn hex8() -> String {
    use sha2::{Digest, Sha256};
    let seed = format!(
        "{}{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let digest = Sha256::digest(seed.as_bytes());
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn codex_bind_report() -> Value {
    let version = codex_version();
    let failed = |err: String| -> Value {
        json!({
            "bound": false, "session_id": Value::Null, "oracle": Value::Null,
            "elapsed_s": 0.0,
            "codex_version": version.clone(),
            "error": err,
        })
    };
    let Some(scratch) = tempfile::Builder::new()
        .prefix("fno-codex-bind-")
        .tempdir()
        .ok()
    else {
        return failed("could not create the canary scratch directory".into());
    };
    let cwd = scratch.path().to_path_buf();
    let Some(codex_home) = setup_private_codex_home(&cwd) else {
        return failed("could not create the private CODEX_HOME".into());
    };
    ensure_daemon(&codex_home);
    let session = resolve_mux_session();
    let _name = format!("codex-bind-canary-{}", hex8());
    let Some((code, out)) = launch_pane(&session, &cwd, &codex_home) else {
        stop_daemon(&codex_home);
        return failed("pane run failed to run".into());
    };
    if code != 0 {
        stop_daemon(&codex_home);
        return failed(out.trim().to_string());
    }
    let pane_id: i64 = out
        .trim()
        .lines()
        .next_back()
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0);
    if pane_id <= 0 {
        stop_daemon(&codex_home);
        return failed(format!(
            "unparseable pane run output {out:?}; a pane may exist without a registry row - inspect with 'fno mux pane ls --session {session}'"
        ));
    }
    let Some(child_pid) = lookup_child_pid(&session, pane_id) else {
        reap_pane(&session, pane_id);
        stop_daemon(&codex_home);
        return failed("no child pid found for the canary pane".into());
    };
    let baseline = daemon_session_ids(&cwd, &codex_home).unwrap_or_default();
    let binding = await_binding(&session, pane_id, &cwd, child_pid, &codex_home, &baseline);
    reap_pane(&session, pane_id);
    stop_daemon(&codex_home);
    let sid = binding.session_id.as_deref().unwrap_or_default();
    if is_canonical_uuid(sid) {
        return json!({
            "bound": true,
            "session_id": sid,
            "oracle": binding.oracle.clone(),
            "elapsed_s": round2(binding.elapsed_s),
            "codex_version": version,
            "error": Value::Null,
        });
    }
    let error = if sid.is_empty() {
        "neither oracle bound within the window".to_string()
    } else {
        let which = binding
            .oracle
            .clone()
            .unwrap_or_else(|| "binding oracle".into());
        format!("{which} returned {sid:?}, not a full canonical session id")
    };
    json!({
        "bound": false,
        "session_id": Value::Null,
        "oracle": Value::Null,
        "elapsed_s": round2(binding.elapsed_s),
        "codex_version": version,
        "error": error,
    })
}

fn is_canonical_uuid(s: &str) -> bool {
    let re = regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        .expect("valid pattern");
    re.is_match(s)
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
