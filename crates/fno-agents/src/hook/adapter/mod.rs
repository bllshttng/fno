//! Per-harness hook-payload adapters: `(event, payload) -> one [`HookEvent`]`.
//!
//! Event names and payload shapes differ per harness, so each `adapter/<h>.rs`
//! owns exactly one `normalize` and nothing else: it knows the payload's shape,
//! never what the payload means. The session-state job consumes the normalized
//! event, so a later job (or a later harness) plugs in beside it without a
//! second parse. Nothing here imports from `super::super::session_state`.

pub mod agy;
pub mod claude;
pub mod codex;
pub mod opencode;
pub mod pi;

/// One normalized hook fire: the fields every harness payload carries once the
/// adapter is done with it. Empty string = the payload did not carry the field;
/// `posture` is `"<sandbox>:<approval>"` when the harness exposed one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookEvent {
    pub harness: String,
    /// The native event name the harness fired (the CLI's `<event>` argument).
    pub event: String,
    /// The daemon-pinned session id the report files under. Empty on a
    /// malformed payload: markers still emit (presence-gate degrade), the
    /// report does not.
    pub session_id: String,
    /// `tool_name` on a PreToolUse fire, else empty.
    pub tool: String,
    /// The human-readable "what it is waiting on" text: the Notification
    /// message, or the question text a picker tool carries in its input.
    pub message: String,
    /// The harness-side model id the payload named (`to_model`), if any.
    pub model: String,
    /// The effort level, from a bare string or an `{"level": ...}` object.
    pub effort: String,
    /// The observed sandbox posture, `"<sandbox>:<approval>"`. Read off the
    /// codex rollout's last turn_context on a Stop fire; `None` elsewhere.
    pub posture: Option<String>,
}

/// Normalize one payload through its harness's adapter. An unknown harness
/// yields an empty event (no session id): the caller's row map is the gate
/// that refuses it, and the entry stays exit-0 fire-and-forget.
pub fn normalize(harness: &str, event: &str, payload: &serde_json::Value) -> HookEvent {
    let mut ev = match harness {
        "agy" => agy::normalize(event, payload),
        "claude" => claude::normalize(event, payload),
        "codex" => codex::normalize(event, payload),
        "opencode" => opencode::normalize(event, payload),
        "pi" => pi::normalize(event, payload),
        _ => HookEvent {
            harness: harness.to_string(),
            event: event.to_string(),
            ..Default::default()
        },
    };
    ev.harness = harness.to_string();
    ev.event = event.to_string();
    ev
}

/// The payload a plugin-hosted harness (opencode, pi) sends: the plugin owns
/// the event translation and builds claude-shaped JSON, so one reader serves
/// both. Neither names a posture.
fn plugin_shaped(harness: &str, event: &str, payload: &serde_json::Value) -> HookEvent {
    HookEvent {
        harness: harness.into(),
        event: event.into(),
        session_id: str_field(payload, "session_id"),
        tool: str_field(payload, "tool_name"),
        message: str_field(payload, "message"),
        model: str_field(payload, "model"),
        effort: effort_field(payload),
        posture: None,
    }
}

/// Extract a string field, tab/newline-free is NOT enforced here: the report
/// rides JSON-RPC, not the shell producer's TSV handoff, so values pass
/// through verbatim.
fn str_field(payload: &serde_json::Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The effort level from either spelling: a bare `"high"` or `{"level": "high"}`.
fn effort_field(payload: &serde_json::Value) -> String {
    match payload.get("effort") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v @ serde_json::Value::Object(_)) => v
            .get("level")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// AC12: a claude and a codex payload normalize to the same HookEvent
    /// shape - every field present, the adapter decides nothing about state.
    #[test]
    fn claude_and_codex_yield_the_same_event_shape() {
        let claude = normalize(
            "claude",
            "UserPromptSubmit",
            &json!({"session_id": "sid-1", "prompt": "go"}),
        );
        let codex = normalize(
            "codex",
            "UserPromptSubmit",
            &json!({"session_id": "sid-1", "prompt": "go"}),
        );
        assert_eq!(claude.harness, "claude");
        assert_eq!(codex.harness, "codex");
        for ev in [&claude, &codex] {
            assert_eq!(ev.event, "UserPromptSubmit");
            assert_eq!(ev.session_id, "sid-1");
            assert_eq!(ev.tool, "");
            assert_eq!(ev.message, "");
            assert_eq!(ev.model, "");
            assert_eq!(ev.effort, "");
            assert_eq!(ev.posture, None);
        }
    }

    /// The plugin-hosted harnesses carry the served `provider/model`, the
    /// permission text and the tool name through the one shared reader.
    #[test]
    fn plugin_payloads_carry_model_message_and_tool() {
        for harness in ["opencode", "pi"] {
            let ev = normalize(
                harness,
                "PreToolUse",
                &json!({"session_id": "s-1", "tool_name": "Bash", "model": "zai/glm-5.3",
                        "message": "Run: ls", "effort": "high"}),
            );
            assert_eq!(ev.harness, harness);
            assert_eq!(ev.session_id, "s-1");
            assert_eq!(ev.tool, "Bash");
            assert_eq!(ev.model, "zai/glm-5.3");
            assert_eq!(ev.message, "Run: ls");
            assert_eq!(ev.effort, "high");
            assert_eq!(ev.posture, None);
        }
    }
}
