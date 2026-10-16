//! pi's headless lane: one `pi --print --mode json` turn run to completion.
//!
//! STATELESS like the opencode and agy one-shots: `--no-session` keeps pi from
//! writing a session file, no session id is minted, and no registry row is
//! created. The route (`--model`, `--provider`, `--thinking`) is the one
//! [`crate::pi::pi_route`] already builds for the keeper and pane lanes.
//!
//! pi exits 0 when the model call fails. The failure is only in the event
//! stream: the final assistant message carries `stopReason: "error"` and an
//! `errorMessage`. The exit code alone would report a dead worker as a clean
//! reply, so [`parse_pi_stream`] reads the stream and the exit code is the
//! second check.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

/// Ceiling for a one-shot turn when the caller passes no `--timeout`.
const DEFAULT_PI_TIMEOUT: Duration = Duration::from_secs(600);

/// Stdout/stderr/exit triple returned to the client (each provider module
/// owns its own nominal type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl AskOutcome {
    fn ok_reply(reply: String) -> Self {
        Self {
            stdout: format!("{reply}\n"),
            stderr: String::new(),
            exit_code: 0,
        }
    }
    fn err(msg: impl Into<String>, code: i32) -> Self {
        Self {
            stdout: String::new(),
            stderr: format!("{}\n", msg.into()),
            exit_code: code,
        }
    }
}

/// A footnote verb seed (`/fno:verb`, either sigil) is a command, so it rides
/// bare; a prose message keeps the `[from:]` envelope.
fn pi_prompt(message: &str, from_name: &str) -> String {
    let seed =
        crate::provider::render_verb_seed(if message.is_empty() { "hello" } else { message }, "pi");
    let first = seed.split_whitespace().next().unwrap_or("");
    if crate::provider::parse_verb_token(first).is_some() {
        seed
    } else {
        format!("[from: {from_name}]\n\n{seed}")
    }
}

/// `pi --print --mode json --no-session <route> <fenced args> <prompt>`.
fn build_pi_argv(
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
    harness_args: &[String],
) -> Vec<String> {
    let route = crate::pi::pi_route(model.unwrap_or(""), effort.unwrap_or(""), "", "");
    let mut argv: Vec<String> = ["pi", "--print", "--mode", "json", "--no-session"]
        .iter()
        .map(|t| t.to_string())
        .collect();
    argv.extend(route.tokens);
    argv.extend(harness_args.iter().cloned());
    argv.push(prompt.to_string());
    argv
}

/// The reply text of the last assistant message in a `--mode json` stream, or
/// the model's own error when that message stopped on one.
fn parse_pi_stream(stdout: &str) -> Result<String, String> {
    let mut last: Option<Value> = None;
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let assistant = |m: &Value| m.get("role").and_then(Value::as_str) == Some("assistant");
        match event.get("type").and_then(Value::as_str) {
            Some("turn_end") => {
                if let Some(m) = event.get("message").filter(|m| assistant(m)) {
                    last = Some(m.clone());
                }
            }
            Some("agent_end") => {
                let found = event
                    .get("messages")
                    .and_then(Value::as_array)
                    .and_then(|ms| ms.iter().rev().find(|m| assistant(m)));
                if let Some(m) = found {
                    last = Some(m.clone());
                }
            }
            _ => {}
        }
    }
    let message = last.ok_or("pi emitted no assistant message")?;
    let stop = message
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or("");
    if matches!(stop, "error" | "aborted") {
        let detail = message
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or("no errorMessage in the stream");
        return Err(format!("pi stopped with {stop}: {detail}"));
    }
    let text: String = message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    match text.trim() {
        "" => Err("pi ended its turn with no text".to_string()),
        reply => Ok(reply.to_string()),
    }
}

/// Last `n` (> 0) characters of `s`, for the stderr tail of a failed run.
fn tail_chars(s: &str, n: usize) -> &str {
    s.char_indices()
        .rev()
        .nth(n - 1)
        .map_or(s, |(idx, _)| &s[idx..])
}

/// Drive one `pi` subprocess: stdin `/dev/null`, stdout teed to the worker log
/// and parsed, stderr drained on a thread, the whole call bounded by the shared
/// watchdog. Returns the reply, or `(exit_code, message)`.
fn run_pi(
    argv: &[String],
    log_path: &Path,
    timeout: Option<Duration>,
    cwd: &Path,
    agent_self: &str,
) -> Result<String, (i32, String)> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let tee = crate::subprocess_ask::open_tee(log_path).ok();
    let argv = crate::spawn_gate::qos_wrap(cwd, argv.to_vec());
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.current_dir(cwd);
    // pi resolves its project from $PWD, not the process cwd.
    cmd.env("PWD", cwd);
    crate::claims::stamp_command_env(&mut cmd, Some(agent_self), "pi", None);
    // Own process group so SIGTERM/SIGKILL/SIGINT reach pi's subshells.
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err((13, "pi binary not found on PATH".to_string()));
        }
        Err(e) => return Err((2, format!("OSError invoking pi: {e}"))),
    };
    let pid = child.id();
    // RAII guard held for the whole function: do NOT discard to `_`.
    let _sigint_guard = crate::subprocess_ask::SigintForwarder::install(pid);

    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let stderr_pipe = child.stderr.take().expect("stderr piped");
    let stderr_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let cap = stderr_buf.clone();
    let stderr_handle = std::thread::spawn(move || {
        let mut r = stderr_pipe;
        let mut s = String::new();
        let _ = r.read_to_string(&mut s);
        if let Ok(mut g) = cap.lock() {
            *g = s;
        }
    });

    let mut watchdog = crate::subprocess_ask::AskWatchdog::spawn(pid, timeout);
    let mut stdout_bytes: Vec<u8> = Vec::new();
    let _ = stdout_pipe.read_to_end(&mut stdout_bytes);
    let stdout_text = String::from_utf8_lossy(&stdout_bytes).into_owned();
    if let Some(mut fh) = tee {
        let _ = fh.write_all(stdout_text.as_bytes());
    }

    watchdog.cancel();
    let (exit_code, sigkill_escalated) =
        crate::subprocess_ask::wait_with_grace(pid, &mut child, 5.0);
    watchdog.join();
    let _ = stderr_handle.join();
    let stderr_text = stderr_buf.lock().map(|s| s.clone()).unwrap_or_default();

    if crate::subprocess_ask::ask_interrupted() {
        return Err((130, "interrupted".to_string()));
    }
    if watchdog.timed_out() || sigkill_escalated {
        return Err((
            12,
            format!(
                "pi --print timed out after {:.0}s",
                timeout.unwrap_or(DEFAULT_PI_TIMEOUT).as_secs_f64()
            ),
        ));
    }
    if exit_code != 0 {
        return Err((
            exit_code,
            format!(
                "pi --print exited {exit_code}: {}",
                tail_chars(stderr_text.trim(), 400)
            ),
        ));
    }
    parse_pi_stream(&stdout_text).map_err(|msg| (1, msg))
}

/// Orchestrate one `spawn --substrate headless --harness pi`: validate, check
/// the registry (fail-closed) for a name collision, then run the turn.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_pi_once(
    home: &crate::paths::AgentsHome,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    timeout: Option<Duration>,
    model: Option<&str>,
    effort: Option<&str>,
    harness_args: &[String],
) -> AskOutcome {
    use crate::claude_ask::{emit_event, py_repr, validate_spawn_inputs};

    if let Err(msg) = validate_spawn_inputs(name, from_name) {
        return AskOutcome::err(msg, 2);
    }
    let events = home.events_jsonl();
    let registry = match crate::state::load_registry(&home.registry_json()) {
        Ok(r) => r,
        Err(e) => {
            emit_event(
                &events,
                "agent_ask_failed",
                &[
                    ("stage", "registry-read".into()),
                    ("name", name.into()),
                    ("provider", "pi".into()),
                    ("error", e.to_string().into()),
                ],
            );
            return AskOutcome::err(format!("registry read failed: {e}"), 12);
        }
    };
    if registry.find(name).is_some() {
        return AskOutcome::err(
            format!(
                "agent {} already exists; use 'fno agents rm {name}' first or pick another name",
                py_repr(name)
            ),
            2,
        );
    }

    let argv = build_pi_argv(&pi_prompt(message, from_name), model, effort, harness_args);
    let log_path = home
        .root()
        .join("agents")
        .join("logs")
        .join(format!("{name}.jsonl"));
    let timeout = timeout.or(Some(DEFAULT_PI_TIMEOUT));
    match run_pi(&argv, &log_path, timeout, cwd, name) {
        Ok(reply) => AskOutcome::ok_reply(reply),
        Err((code, msg)) => {
            emit_event(
                &events,
                "agent_ask_failed",
                &[
                    ("stage", "pi-once".into()),
                    ("name", name.into()),
                    ("provider", "pi".into()),
                    ("error", msg.clone().into()),
                ],
            );
            AskOutcome::err(msg, code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape pi 0.99 prints for a model failure, measured 2026-10-16: exit
    // 0, empty content, and the cause only in `stopReason` + `errorMessage`.
    const ERROR_STREAM: &str = concat!(
        r#"{"type":"agent_start"}"#,
        "\n",
        r#"{"type":"turn_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"Codex error: model not supported"}}"#,
        "\n",
        r#"{"type":"agent_end","messages":[{"role":"user","content":[{"type":"text","text":"Reply pong"}]},{"role":"assistant","content":[],"stopReason":"error","errorMessage":"Codex error: model not supported"}]}"#,
        "\n",
        r#"{"type":"agent_settled"}"#,
    );

    #[test]
    fn a_model_error_exiting_zero_is_not_a_reply() {
        let err = parse_pi_stream(ERROR_STREAM).unwrap_err();
        assert_eq!(
            err,
            "pi stopped with error: Codex error: model not supported"
        );
    }

    #[test]
    fn the_reply_is_the_last_assistant_text_across_tool_turns() {
        let stream = concat!(
            r#"{"type":"turn_end","message":{"role":"assistant","content":[{"type":"toolCall","name":"read"}],"stopReason":"toolUse"}}"#,
            "\n",
            r#"{"type":"turn_end","message":{"role":"assistant","content":[{"type":"text","text":"po"},{"type":"thinking","thinking":"x"},{"type":"text","text":"ng\n"}],"stopReason":"stop"}}"#,
        );
        assert_eq!(parse_pi_stream(stream).as_deref(), Ok("pong"));
    }

    #[test]
    fn argv_carries_the_pi_route_and_the_prompt_last() {
        let argv = build_pi_argv(
            "go",
            Some("zai/glm-5.3-flash"),
            Some("high"),
            &["--x".into()],
        );
        assert_eq!(
            argv,
            [
                "pi",
                "--print",
                "--mode",
                "json",
                "--no-session",
                "--model",
                "zai/glm-5.3-flash",
                "--thinking",
                "high",
                "--x",
                "go",
            ]
        );
    }
}
