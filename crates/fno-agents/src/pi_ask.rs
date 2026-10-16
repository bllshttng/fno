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

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::opencode_ask::{AskOutcome, DEFAULT_ONE_SHOT_TIMEOUT};

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
    // pi's stop reasons are stop | length | toolUse | error | aborted | deferred.
    // Only `stop` is a finished reply: `length` is a cut-off answer.
    if stop != "stop" {
        let detail = message
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or("the turn did not end on stop");
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
    let timeout = timeout.or(Some(DEFAULT_ONE_SHOT_TIMEOUT));
    let run = crate::opencode_ask::run_one_shot("pi", &argv, &log_path, timeout, cwd, name)
        .and_then(|stdout| parse_pi_stream(&stdout).map_err(|msg| (1, msg)));
    match run {
        Ok(reply) => AskOutcome::ok_reply(format!("{reply}\n")),
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
        let cut_off = r#"{"type":"turn_end","message":{"role":"assistant","content":[{"type":"text","text":"par"}],"stopReason":"length"}}"#;
        assert_eq!(
            parse_pi_stream(cut_off).unwrap_err(),
            "pi stopped with length: the turn did not end on stop"
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
