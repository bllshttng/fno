//! One Bash PreToolUse entry for the guards that share the same payload.
//!
//! Keep the former registration order. Rust guards run in-process; the three
//! Python guards keep their existing owners and run as children with the same
//! stdin and environment. A denial from any guard is returned after all six
//! have recorded their decision. The guardrail preset owns whether a guard
//! runs at all: a preset-disabled guard answers nothing and emits no row.

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn run(_args: &[String]) -> i32 {
    let raw = super::read_stdin();
    let payload: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let process_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let plugin_root = std::env::var_os("FNO_REPO_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .or_else(crate::provider::plugin_root);
    let mut refusals = Vec::new();

    // One config resolution for the whole chain: guard_enabled re-walks the
    // config candidates per call, and this hook runs on every Bash turn.
    let preset = crate::agents_config::guard_preset(&cwd);
    // Resolved once for the chain; the Python guards receive it as
    // FNO_EVENTS_PATH so guard_mark skips its own resolver subprocess.
    let events_pin = crate::state_path::resolve("events", &cwd);

    if crate::agents_config::preset_runs(preset, "bg-process") {
        if let Some(root) = plugin_root.as_deref() {
            if let Some(reason) =
                run_python_guard(&root, "bg-process-guard.py", &raw, events_pin.as_deref())
            {
                refusals.push(reason);
            }
        } else {
            eprintln!("pretooluse-bash: plugin root unavailable; skipping bg-process-guard");
        }
    }

    if crate::agents_config::preset_runs(preset, "bin-install") {
        let bin_refusal = super::bin_install_guard::judge(&payload);
        super::emit_guard_decision(&cwd, "bin-install-guard", "Bash", bin_refusal.is_some());
        refusals.extend(bin_refusal);
    }

    if crate::agents_config::preset_runs(preset, "git-protection") {
        if let Some(root) = plugin_root.as_deref() {
            if let Some(reason) =
                run_python_guard(&root, "git-protection.py", &raw, events_pin.as_deref())
            {
                refusals.push(reason);
            }
        } else {
            eprintln!("pretooluse-bash: plugin root unavailable; skipping git-protection guard");
        }
    }

    if crate::agents_config::preset_runs(preset, "pipe") {
        let pipe_refusal = super::pipe_guard::judge(&payload);
        super::emit_guard_decision(&cwd, "pipe-guard", "Bash", pipe_refusal.is_some());
        refusals.extend(pipe_refusal);
    }

    if crate::agents_config::preset_runs(preset, "recursive-grep") {
        if let Some(root) = plugin_root.as_deref() {
            if let Some(reason) = run_python_guard(
                &root,
                "recursive-grep-guard.py",
                &raw,
                events_pin.as_deref(),
            ) {
                refusals.push(reason);
            }
        } else {
            eprintln!("pretooluse-bash: plugin root unavailable; skipping recursive-grep-guard");
        }
    }

    let test = if crate::agents_config::preset_runs(preset, "test-run") {
        super::test_run_guard::evaluate(&payload)
    } else {
        super::test_run_guard::Evaluation {
            should_log: false,
            refusal: None,
            stage: "preset-disabled",
        }
    };
    if test.should_log {
        super::emit_guard_decision(
            &process_cwd,
            "test-run-guard",
            "Bash",
            test.refusal.is_some(),
        );
    }
    refusals.extend(test.refusal);

    if crate::agents_config::preset_runs(preset, "effect") {
        let effect_refusal = crate::effect_gate::judge(&payload, &cwd);
        super::emit_guard_decision(&cwd, "effect-guard", "Bash", effect_refusal.is_some());
        refusals.extend(effect_refusal);
    }

    if crate::agents_config::preset_runs(preset, "refusal-streak") {
        let streak_refusal = super::refusal_streak::judge_pre(&payload);
        super::emit_guard_decision(&cwd, "refusal-streak", "Bash", streak_refusal.is_some());
        refusals.extend(streak_refusal);
    }

    if refusals.is_empty() {
        if let Some(reason) = gate_native_launch(&payload, &cwd) {
            refusals.push(reason);
        }
    }
    if refusals.is_empty() {
        record_launch(&payload, &cwd);
        super::emit_allow()
    } else {
        super::emit_block(&refusals.join("\n\n"))
    }
}

/// Decision D1: a native launch line an agent types passes the same spawn
/// gate a spawn passes, before its launch record is written. A person typing
/// in a plain terminal runs no hook, so a human launch is never gated. The
/// hook holds nothing across the launch: the verdict is taken and the keys
/// released in the same call. A gate that cannot answer is not a refusal -
/// the hook is foreground-bounded and fails open, never wedging the Bash
/// call on a daemon that is down.
fn gate_native_launch(payload: &Value, cwd: &Path) -> Option<String> {
    let Some(cmd) = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
    else {
        return None;
    };
    if !crate::launch_record::may_launch(cmd) {
        return None;
    }
    // `fno agents spawn` runs the gate itself; gating it here too would
    // double-count the slot and double-queue the lane. The floor setter is
    // the exception: the learned-cap floor is the user's setting, so an
    // agent tool call that carries `--floor` is refused here. A person
    // typing the same line in a shell runs no PreToolUse hook and writes.
    if cmd.contains("agents spawn") {
        return refuse_floor_setter(cmd);
    }
    let launches = crate::launch_record::parse_command(cmd, &cwd.to_string_lossy());
    if launches.is_empty() {
        return None;
    }
    let home = crate::paths::AgentsHome::from_env();
    let caller_session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    for launch in launches {
        let input = crate::spawn_gate::GateInput {
            name: launch.name.clone().unwrap_or_default(),
            substrate: crate::launch_record::native_substrate_word(&launch.harness).into(),
            flags: crate::spawn_gate::GateFlags {
                force: false,
                no_wait: true,
            },
            route_provider: None,
            node: launch.node.clone(),
            account: None,
            caller_session: caller_session.clone(),
            succession_scope: None,
            holder_pid: Some(std::process::id()),
            seed: None,
            session_phase: None,
        };
        let config_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match crate::spawn_gate::run_gate(&config_cwd, &home.registry_json(), input) {
            Ok(mut guard) => guard.release(),
            Err(refusal) => {
                if refusal.exit_code == crate::spawn_gate::EXIT_GATE_UNAVAILABLE {
                    eprintln!("pretooluse-bash: spawn gate unavailable; launch allowed");
                    continue;
                }
                let receipt = refusal.receipt.as_ref();
                let reason = receipt
                    .and_then(|r| r.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let remedy = receipt
                    .and_then(|r| r.get("remedy"))
                    .and_then(Value::as_str)
                    .unwrap_or("check fno agents gate-status");
                let remedy = crate::capacity::native_remedy(remedy, receipt);
                let place = crate::spawn_gate_queue::place_note(receipt);
                return Some(format!(
                    "spawn gate refused the native launch ({reason}{place}): {remedy}"
                ));
            }
        }
    }
    None
}

/// The host word a natively launched session reports as. `claude --bg` is
/// Not a guard. An allowed command that starts a harness session leaves a
/// launch record, which the child claims at SessionStart: a `claude --bg`
/// child cannot see its launcher any other way (see launch_record.rs).
fn record_launch(payload: &Value, cwd: &Path) {
    let Some(cmd) = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
    else {
        return;
    };
    if !crate::launch_record::may_launch(cmd) {
        return;
    }
    let (ambient_session, harness, _) = crate::claims::ambient_parent_edge();
    let session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(ambient_session);
    let home = crate::paths::AgentsHome::from_env();
    if let Err(error) = crate::launch_record::record_command(
        home.root(),
        cmd,
        &cwd.to_string_lossy(),
        session.as_deref(),
        harness.as_deref(),
        crate::claims::now_ms(),
    ) {
        eprintln!("pretooluse-bash: launch record not written: {error}");
    }
}

fn run_python_guard(
    root: &Path,
    script: &str,
    input: &str,
    events_pin: Option<&Path>,
) -> Option<String> {
    let path = root.join("hooks").join(script);
    let mut command = Command::new("python3");
    command.arg(&path);
    // The shared guard_mark resolves the events journal with a
    // `fno-agents state path events` subprocess per guard; handing the pin
    // we already resolved saves one process spin per Python guard on every
    // Bash turn.
    if let Some(pin) = events_pin {
        command.env("FNO_EVENTS_PATH", pin);
    }
    let mut child = match command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "pretooluse-bash: could not start {}: {error}; allowing",
                path.display()
            );
            return None;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(input.as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!(
                "pretooluse-bash: could not send payload to {}: {error}; allowing",
                path.display()
            );
            return None;
        }
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!(
                "pretooluse-bash: could not read {}: {error}; allowing",
                path.display()
            );
            return None;
        }
    };
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    if !output.status.success() {
        eprintln!(
            "pretooluse-bash: {} exited {}; allowing",
            path.display(),
            output.status
        );
        return None;
    }
    python_refusal(&output.stdout)
}

fn python_refusal(stdout: &[u8]) -> Option<String> {
    let response: Value = serde_json::from_slice(stdout).ok()?;
    let specific = response.get("hookSpecificOutput").unwrap_or(&response);
    let decision = specific
        .get("permissionDecision")
        .or_else(|| response.get("decision"))
        .and_then(Value::as_str);
    if !matches!(decision, Some("deny" | "block")) {
        return None;
    }
    specific
        .get("permissionDecisionReason")
        .or_else(|| response.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| Some("a Bash PreToolUse guard denied this command".to_string()))
}

/// The learned-cap floor is the user's setting: the hook refuses an agent
/// tool call that runs the setter. The user's own shell line runs no
/// PreToolUse hook, so the same line still works from a person. The scan is
/// a token sweep like every probe here: `--floor` or `--floor=` before the
/// first `--` fence on an `agents spawn` line is the setter.
fn refuse_floor_setter(cmd: &str) -> Option<String> {
    let mut at_spawn = false;
    for tok in cmd.split_whitespace() {
        if tok == "--" {
            return None;
        }
        if at_spawn && (tok == "--floor" || tok.starts_with("--floor=")) {
            return Some(
                "spawn-gate: the spawn cap floor is the user's setting, so an agent tool call \
                 cannot run it. The user sets it in a shell: fno agents spawn --floor N"
                    .to_string(),
            );
        }
        if tok == "spawn" {
            at_spawn = true;
        }
    }
    None
}

#[cfg(test)]
mod floor_setter_tests {
    use super::refuse_floor_setter;

    #[test]
    fn hook_refuses_a_floor_tool_call_and_passes_the_rest() {
        assert!(refuse_floor_setter("fno agents spawn --floor 8").is_some());
        assert!(refuse_floor_setter("fno agents spawn --floor=8 w1 hi").is_some());
        assert!(refuse_floor_setter("fno agents spawn w1 hi --floor 2").is_some());
        // Past the fence the flag is the harness's, and other lines never
        // refuse.
        assert!(refuse_floor_setter("fno agents spawn -- --floor 3").is_none());
        assert!(refuse_floor_setter("fno agents spawn w1 hi").is_none());
        assert!(refuse_floor_setter("fno agents gate-status").is_none());
        assert!(refuse_floor_setter("git status").is_none());
    }
}
