//! Native `fno doctor`: the report the deleted `cli/src/fno/doctor.py`
//! answered, the bundle and plugin-file leaves, and the four opt-in flag
//! arms. The per-leaf front-door claim extends the `doctor_cost.rs`
//! classify shape: this module claims the bare report, `bundle`,
//! `plugin-file`, and the hidden op arms; `cost`, `update`, `event`,
//! `lint style|menu-caps`, `skill-diff` and `builds` are claimed by their
//! own classifiers earlier in `decide_role`; every other leaf
//! (`codemap`, `evals`, `test`, ...) returns None so the argv falls
//! through to the Python wheel byte-verbatim until its family ports.
//! `--context-audit` also falls through: the diag family owns
//! `fno.context_audit` (epic change 23) and `context_run.rs` still spawns
//! the Python spelling.

mod bind_canary;
mod bundle;
mod human;
mod report;

use std::ffi::OsString;
use std::io::Write;

use serde_json::Value;

pub use bundle::run_bundle;
pub(crate) use report::{find_transcript_op, Blockers, Report};

/// What the front door should do with a `doctor ...` argv.
pub enum Claim {
    /// The native report / bundle / plugin-file / flag arms; the payload is
    /// the argv after `doctor`.
    Home(Vec<OsString>),
    /// A pure transport leaf: exec the fno-agents binary with this argv
    /// (label names the leaf for the exec refusal).
    AgentsExec(&'static str, Vec<OsString>),
}

/// The leaves this classifier never claims: their families port in their
/// own children and the wheel keeps answering them.
const FOREIGN_LEAVES: &[&str] = &[
    "codemap",
    "evals",
    "test",
    "scratch",
    "footprint",
    "lanes",
    "bash-census",
    "reclaim",
    "route",
    "observer",
    "fanout",
];

pub fn classify(args: &[OsString]) -> Option<Claim> {
    if args.first()?.to_str()? != "doctor" {
        return None;
    }
    let second = args.get(1).and_then(|a| a.to_str());
    match second {
        None => Some(Claim::Home(vec![])),
        Some("bundle") | Some("plugin-file") => Some(Claim::Home(args[1..].to_vec())),
        Some(word) if FOREIGN_LEAVES.contains(&word) => None,
        Some("intel") => Some(Claim::AgentsExec(
            "fno doctor intel",
            vec![OsString::from("intel")],
        )),
        Some("stalls") => {
            let mut argv = vec![OsString::from("stalls")];
            argv.extend(args[2..].iter().cloned());
            // The deleted Python wrapper populated the event journals when the
            // caller named none (doctor_cli.py stalls_command); the fold
            // refuses an argv with no --events.
            let has_events = args[2..].iter().any(
                |a| matches!(a.to_str(), Some(w) if w == "--events" || w.starts_with("--events=")),
            );
            if !has_events {
                for path in stall_event_journals() {
                    argv.push(OsString::from("--events"));
                    argv.push(path.into_os_string());
                }
            }
            Some(Claim::AgentsExec("fno doctor stalls", argv))
        }
        Some("harness") => {
            // `harness-probe rubric <harness> [--live] [--json]` (the
            // Python leaf was a pure argv rewriter onto the binary).
            if args.len() < 3 {
                return None;
            }
            let mut argv = vec![OsString::from("harness-probe"), OsString::from("rubric")];
            argv.push(args[2].clone());
            for a in &args[3..] {
                match a.to_str() {
                    Some("--live") => argv.push(OsString::from("--live")),
                    Some("--json") | Some("-J") => argv.push(OsString::from("--json")),
                    _ => return None,
                }
            }
            Some(Claim::AgentsExec("fno doctor harness", argv))
        }
        Some("harness-matrix") => {
            let mut argv = vec![OsString::from("harness-matrix")];
            for a in &args[2..] {
                match a.to_str() {
                    Some("--write") => argv.push(OsString::from("--write")),
                    _ => return None,
                }
            }
            Some(Claim::AgentsExec("fno doctor harness-matrix", argv))
        }
        // A leading flag: native unless the context-audit family rides along.
        Some(word) if word.starts_with('-') => {
            let context = args[1..]
                .iter()
                .any(|a| a.to_str() == Some("--context-audit"));
            if context {
                return None;
            }
            let claimed = args[1..].iter().any(|a| match a.to_str() {
                Some("--fix")
                | Some("--blockers")
                | Some("--json")
                | Some("-J")
                | Some("--source")
                | Some("--cost-check")
                | Some("--codex-hooks")
                | Some("--codex-bind")
                | Some("--codex-app-server")
                | Some("--find-transcript") => true,
                Some(w) if w.starts_with("--source=") || w.starts_with("--find-transcript=") => {
                    true
                }
                _ => false,
            });
            claimed.then(|| Claim::Home(args[1..].to_vec()))
        }
        // Any other leaf word: not ours until its family ports.
        Some(_) => None,
    }
}

fn usage_error(message: &str) -> i32 {
    eprintln!("Usage: fno doctor [OPTIONS]\nTry 'fno doctor --help' for help.\n\nError: {message}");
    2
}

/// The three live event journals for the stalls fold's default
/// (`fno.paths.event_journals`' live set, without the retained rotations: the
/// fold's window is days and an explicit --events names archives). Ephemeral
/// siblings ride when they exist, resolved and de-duplicated like Python's.
fn stall_event_journals() -> Vec<std::path::PathBuf> {
    let mut live: Vec<std::path::PathBuf> = vec![
        report::global_events_path(),
        match std::env::var_os("FNO_AGENTS_HOME") {
            Some(home) => std::path::PathBuf::from(home).join("events.jsonl"),
            None => report::home_dir().join(".fno/agents/events.jsonl"),
        },
        report::project_events_path(),
    ];
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let mut seen: Vec<std::path::PathBuf> = Vec::new();
    for base in live.drain(..) {
        let resolved = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
        let mut bases = vec![resolved.clone()];
        let ephemeral = resolved.with_file_name(format!(
            "{}.ephemeral",
            resolved.file_name().unwrap_or_default().to_string_lossy()
        ));
        if ephemeral.exists() {
            bases.push(ephemeral);
        }
        for path in bases {
            if !seen.contains(&path) {
                seen.push(path.clone());
                out.push(path);
            }
        }
    }
    out
}

/// `python_json`: the report's JSON stdout keeps
/// byte shape the goldens froze (", " separators, ": " after keys,
/// ensure_ascii escapes, insertion order via serde_json preserve_order).
pub fn python_json(value: &Value) -> String {
    fn esc(s: &str, out: &mut String) {
        out.push('"');
        for unit in s.encode_utf16() {
            match unit {
                0x22 => out.push_str("\\\""),
                0x5c => out.push_str("\\\\"),
                0x0a => out.push_str("\\n"),
                0x0d => out.push_str("\\r"),
                0x09 => out.push_str("\\t"),
                0x08 => out.push_str("\\b"),
                0x0c => out.push_str("\\f"),
                0x20..=0x7e => out.push(unit as u8 as char),
                _ => out.push_str(&format!("\\u{unit:04x}")),
            }
        }
        out.push('"');
    }
    fn render(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => esc(s, out),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Null => out.push_str("null"),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    render(item, out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    esc(k, out);
                    out.push_str(": ");
                    render(v, out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    render(value, &mut out);
    out
}

fn print_json(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", python_json(value));
}

fn echo_stderr(line: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

fn echo_stdout(line: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

/// One bounded subprocess capture (the `_bounded_command` shape: a hung
/// probe returns None instead of wedging the report). The pipes drain only
/// after exit, so every caller must run a command with bounded output: a
/// child that fills the OS pipe buffer before exiting blocks on write and
/// dies at the deadline, reading as None.
pub(crate) fn bounded_command_for(argv: &[String], secs: u64) -> Option<(i32, String, String)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    run_bounded(argv, deadline)
}

/// The 5s default the collectors share.
pub(crate) fn bounded_command(argv: &[String]) -> Option<(i32, String, String)> {
    bounded_command_for(argv, 5)
}

fn run_bounded(argv: &[String], deadline: std::time::Instant) -> Option<(i32, String, String)> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The child leads its own process group, so the deadline kill below
        // can take its descendants with it (the deleted Python leg's
        // start_new_session + killpg contract).
        .process_group(0)
        .spawn()
        .ok()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = child.stdout.take().unwrap();
                let mut err = child.stderr.take().unwrap();
                use std::io::Read;
                let mut stdout = String::new();
                let mut stderr = String::new();
                let _ = out.read_to_string(&mut stdout);
                let _ = err.read_to_string(&mut stderr);
                return Some((status.code().unwrap_or(-1), stdout, stderr));
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
}

/// The entry the front door dispatches: the argv after `doctor`.
pub fn run(tail: &[OsString]) -> i32 {
    let words: Vec<String> = tail
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    match words.first().map(String::as_str) {
        Some("bundle") => run_bundle(&words[1..]),
        Some("plugin-file") => run_plugin_file(&words[1..]),
        _ => run_home(&words),
    }
}

/// `fno doctor plugin-file <path> [--json]` (doctor.py:218-237).
fn run_plugin_file(args: &[String]) -> i32 {
    let mut json = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" | "-J" => json = true,
            w if w.starts_with('-') => {
                return usage_error(&format!("Got unexpected extra argument ({w})"))
            }
            w => positional.push(w),
        }
        i += 1;
    }
    let Some(active) = positional.first() else {
        return usage_error("Missing argument 'ACTIVE_SKILL_PATH'.");
    };
    let rep = report::plugin_file_report(active);
    if json {
        // The Python leg froze `json.dumps(report, sort_keys=True)`.
        let mut sorted = rep.clone();
        sort_keys(&mut sorted);
        print_json(&sorted);
    } else {
        echo_stdout(&report::plugin_file_text(&rep));
    }
    match rep.get("record").and_then(|v| v.as_str()) {
        Some("PLUGIN_FILE_STALE") => 3,
        Some("PLUGIN_FILE_UNKNOWN") => 4,
        _ => 0,
    }
}

fn sort_keys(v: &mut Value) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            let rebuilt: serde_json::Map<String, Value> = keys
                .into_iter()
                .filter_map(|k| {
                    map.remove(&k).map(|mut val| {
                        sort_keys(&mut val);
                        (k, val)
                    })
                })
                .collect();
            *map = rebuilt;
        }
        Value::Array(items) => {
            for item in items {
                sort_keys(item);
            }
        }
        _ => {}
    }
}

/// The bare-doctor flag arms (the `doctor_command` body, in its own arm
/// order: context-audit already fell through at classify).
fn run_home(args: &[String]) -> i32 {
    let mut fix = false;
    let mut blockers_only = false;
    let mut json = false;
    let mut source: Option<String> = None;
    let mut cost_check = false;
    let mut codex_hooks = false;
    let mut codex_bind = false;
    let mut codex_app_server = false;
    let mut find_transcript: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let word = args[i].as_str();
        match word {
            "--fix" => fix = true,
            "--blockers" => blockers_only = true,
            "--json" | "-J" => json = true,
            "--cost-check" => cost_check = true,
            "--codex-hooks" => codex_hooks = true,
            "--codex-bind" => codex_bind = true,
            "--codex-app-server" => codex_app_server = true,
            "--find-transcript" => {
                i += 1;
                match args.get(i) {
                    Some(v) => find_transcript = Some(v.clone()),
                    None => return usage_error("Option '--find-transcript' requires an argument"),
                }
            }
            "--source" => {
                i += 1;
                match args.get(i) {
                    Some(v) => source = Some(v.clone()),
                    None => return usage_error("Option '--source' requires an argument"),
                }
            }
            // Accepted-and-ignored context arms: the Python command declared
            // them, so a bare run with one set still answers the report.
            "--context-harness" | "--context-entry" | "--context-budget" | "--context-host" => {
                i += 1;
                if args.get(i).is_none() {
                    return usage_error(&format!("Option '{word}' requires an argument"));
                }
            }
            w if w.starts_with("--source=") => source = Some(w["--source=".len()..].to_string()),
            w if w.starts_with("--find-transcript=") => {
                find_transcript = Some(w["--find-transcript=".len()..].to_string())
            }
            w if w.starts_with("--context-harness=")
                || w.starts_with("--context-entry=")
                || w.starts_with("--context-budget=")
                || w.starts_with("--context-host=") => {}
            other => return usage_error(&format!("Got unexpected extra argument ({other})")),
        }
        i += 1;
    }

    // The hidden op doors the surviving Python callers read (change 4).
    if codex_app_server {
        print_json(&report::codex_app_server_report());
        return 0;
    }
    if let Some(session_id) = find_transcript {
        // The op door already prints {"path": ...} in the parity format; the
        // flag and the Python importers share one impl.
        return find_transcript_op(&session_id);
    }

    if codex_hooks {
        if fix || cost_check || codex_bind {
            return usage_error("--codex-hooks may only be combined with --json");
        }
        let result = report::codex_hooks_report();
        if json {
            print_json(&result);
            human::emit_codex_hooks_report(&result, true);
        } else {
            human::emit_codex_hooks_report(&result, false);
        }
        return 0;
    }
    if codex_bind {
        if fix || cost_check {
            return usage_error("--codex-bind may only be combined with --json");
        }
        let result = bind_canary::codex_bind_report();
        if json {
            print_json(&result);
        } else {
            human::emit_codex_bind_report(&result);
        }
        let bound = result
            .get("bound")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return if bound { 0 } else { 1 };
    }
    if cost_check {
        return cost_check_report();
    }

    let rep = Report::build(source.as_deref());
    let blockers = Blockers::of(&rep.value);
    let source_blocked = rep
        .value
        .get("source_checkout_sync")
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        == Some("behind");
    let src = report::resolve_source(source.as_deref());
    let cargo_present = report::cargo_bin_present();

    if blockers_only {
        human::emit_blockers(&blockers.0, false);
        return if blockers.0.is_empty() { 0 } else { 1 };
    }

    if json {
        print_json(&rep.value);
        if !blockers.0.is_empty() {
            human::emit_blockers(&blockers.0, true);
            echo_stderr("fno doctor: advisory findings follow.");
        }
        human::emit_human(&rep.value, src.as_deref(), true, cargo_present);
    } else {
        if !blockers.0.is_empty() {
            human::emit_blockers(&blockers.0, false);
            echo_stdout("fno doctor: advisory findings follow.");
        }
        human::emit_human(&rep.value, src.as_deref(), false, cargo_present);
        if !fix {
            if let Some(line) = report::preamble_budget_line(src.as_ref().and_then(|p| p.to_str()))
            {
                echo_stdout(&line);
                if let Some(line) = report::session_start_bytes_line(&line) {
                    echo_stdout(&line);
                }
            }
        }
    }

    let dead_agents = rep
        .value
        .get("launch_agents")
        .map(|a| {
            a.get("dead")
                .map(|d| !d.is_null() && !d.as_array().map(|a| a.is_empty()).unwrap_or(true))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let stage_stale = rep
        .value
        .get("plugin_cache")
        .map(|pc| {
            pc.get("kind").and_then(Value::as_str) == Some("stage")
                && pc.get("status").and_then(Value::as_str) == Some("stale")
        })
        .unwrap_or(false);

    if fix && source_blocked {
        echo_stderr(
            "fno doctor: --fix refused: resolved source checkout is behind origin/main; sync the checkout before repairing.",
        );
        return 1;
    }
    if fix {
        // Heal a dead/wedged pr-watch first; the stale --fix path below
        // execs `fno doctor update` and never returns (doctor.py:4526+).
        let pw_verdict = rep
            .value
            .get("pr_watch")
            .and_then(|pw| pw.get("verdict"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if matches!(pw_verdict.as_deref(), Some("dead") | Some("wedged")) && !json {
            let (cure, argv): (&str, Vec<String>) = if pw_verdict.as_deref() == Some("wedged") {
                (
                    "refresh",
                    vec![
                        "refresh".into(),
                        "--force-bounce".into(),
                        "--caller".into(),
                        "doctor-fix".into(),
                    ],
                )
            } else {
                (
                    "heal",
                    vec!["heal".into(), "--caller".into(), "doctor-fix".into()],
                )
            };
            let binary = report::resolve_binary();
            let said = match binary {
                None => "skipped; fno-agents binary not found".to_string(),
                Some(bin) => {
                    let mut argv_full = vec![bin.clone()];
                    argv_full.push("pr-watch".to_string());
                    argv_full.extend(argv);
                    match std::process::Command::new(&argv_full[0])
                        .args(&argv_full[1..])
                        .output()
                    {
                        Ok(out) => {
                            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                            if text.is_empty() {
                                format!("rc={}", out.status.code().unwrap_or(-1))
                            } else {
                                text
                            }
                        }
                        Err(_) => "rc=-1".to_string(),
                    }
                }
            };
            echo_stderr(&format!("fno doctor: --fix pr-watch {cure}: {said}"));
        }
        let gr = rep.value.get("groom").cloned().unwrap_or(Value::Null);
        let gr_stale = gr.get("stale").and_then(Value::as_bool).unwrap_or(false);
        let gr_agent = gr
            .get("agent_installed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if cfg!(target_os = "macos") && gr_stale && !gr_agent && !json {
            let receipt = report::groom_install_agent();
            let status = receipt.get("status").and_then(Value::as_str).unwrap_or("");
            let detail = receipt
                .get("detail")
                .and_then(Value::as_str)
                .or_else(|| receipt.get("plist").and_then(Value::as_str))
                .unwrap_or("");
            echo_stderr(&format!(
                "fno doctor: --fix groom agent: {status} ({detail})"
            ));
        }
        if json {
            echo_stderr(
                "fno doctor: --fix skipped under --json (would pollute the JSON stdout); run `fno doctor --fix` without --json to repair.",
            );
        } else {
            let python_stale = rep
                .value
                .get("python_stale")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let rust_stale = rep
                .value
                .get("rust_stale")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if python_stale || rust_stale {
                match report::front_door() {
                    None => {
                        echo_stderr(
                            "fno doctor: --fix needs the native front door `fno` on PATH; none found.",
                        );
                        return 1;
                    }
                    Some(door) => {
                        echo_stderr("fno doctor: --fix running `fno doctor update`...");
                        let mut argv =
                            vec![door.clone(), "doctor".to_string(), "update".to_string()];
                        if let Some(src) = &source {
                            argv.push("--source".to_string());
                            argv.push(src.clone());
                        }
                        match std::process::Command::new(&argv[0])
                            .args(&argv[1..])
                            .status()
                        {
                            Ok(status) => return status.code().unwrap_or(1),
                            Err(_) => return 1,
                        }
                    }
                }
            } else {
                echo_stderr("fno doctor: nothing to fix.");
            }
        }
    }

    if rep.value.get("status").and_then(Value::as_str) == Some("stale")
        || source_blocked
        || dead_agents
        || stage_stale
    {
        1
    } else {
        0
    }
}

/// `--cost-check`: cross-check our session cost against the reference tool
/// for one recent session (doctor.py:978-1016).
fn cost_check_report() -> i32 {
    let skip = |reason: &str| -> i32 {
        echo_stdout(&format!("fno doctor: cost-check skipped ({reason})."));
        0
    };
    let Some((session_id, _)) = report::find_recent_session_with_transcript() else {
        return skip("no completed session with a surviving transcript");
    };
    let Some(ours) = report::run_session_cost(&session_id) else {
        return skip(&format!(
            "session cost leg unavailable or failed for {session_id}"
        ));
    };
    let (theirs, reason) = report::run_ref_cost_tool(&session_id);
    let Some(theirs) = theirs else {
        return skip(
            reason
                .as_deref()
                .unwrap_or("reference cost tool unavailable"),
        );
    };
    let divergence = if theirs == 0.0 {
        if ours == 0.0 {
            0.0
        } else {
            f64::INFINITY
        }
    } else {
        (ours - theirs).abs() / theirs
    };
    const THRESHOLD: f64 = 0.10;
    let pct = if divergence.is_infinite() {
        "inf".to_string()
    } else {
        format!("{:.1}%", divergence * 100.0)
    };
    if divergence <= THRESHOLD {
        echo_stdout(&format!(
            "fno doctor: cost-check OK: session {session_id} ours=${ours:.2} reference=${theirs:.2} divergence={pct}"
        ));
        return 0;
    }
    echo_stdout(&format!(
        "fno doctor: cost-check WARN: session {session_id} ours=${ours:.2} reference=${theirs:.2} divergence={pct} (> 10% - catalog pricing or dedup drift; see crates/fno-agents/src/model_price.rs)"
    ));
    1
}

#[cfg(test)]
mod tests {
    use super::bounded_command_for;

    /// The deleted Python transport's contract (its
    /// test_doctor_timeout_kills_gate_descendants fixture): a timed-out
    /// gate's descendant dies with the process group, not orphaned past the
    /// leader.
    #[test]
    fn bounded_command_kills_gate_descendants() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let pid_path = tmp.path().join("child.pid");
        let result = bounded_command_for(
            &[
                "bash".to_string(),
                "-c".to_string(),
                "sleep 30 & child=$!; printf \"%s\" \"$child\" > \"$1\"; wait \"$child\""
                    .to_string(),
                "bash".to_string(),
                pid_path.to_string_lossy().into_owned(),
            ],
            2,
        );
        assert!(result.is_none());
        let child_pid: i32 = std::fs::read_to_string(&pid_path)
            .expect("the grandchild wrote its pid")
            .trim()
            .parse()
            .expect("the pid file holds an int");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let alive = unsafe { libc::kill(child_pid, 0) } == 0;
            if !alive {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed-out gate descendant {child_pid} is still alive"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
