//! The pi hook payload: the claude-shaped JSON the extension
//! (`cli/src/fno/setup/assets/pi/footnote.ts`) builds from pi's extension
//! events (`before_agent_start`, `tool_call`, `agent_settled`).
//!
//! The extension owns the event translation, so this adapter only reads
//! fields. `model` arrives as `provider/model` when pi names the provider.
//! pi exposes no permission-prompt event, no effort field and no sandbox
//! posture, so those reads stay tolerant and the row's blocked state keeps
//! coming from the screen backstop.

use super::{effort_field, str_field, HookEvent};
use serde_json::Value;

pub(crate) fn normalize(event: &str, payload: &Value) -> HookEvent {
    HookEvent {
        harness: "pi".into(),
        event: event.into(),
        session_id: str_field(payload, "session_id"),
        tool: str_field(payload, "tool_name"),
        message: str_field(payload, "message"),
        model: str_field(payload, "model"),
        effort: effort_field(payload),
        posture: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pi_payloads_normalize_to_the_hook_event_shape() {
        let start = normalize(
            "UserPromptSubmit",
            &json!({"session_id": "pi-1", "model": "zai/glm-5.3"}),
        );
        assert_eq!(start.session_id, "pi-1");
        assert_eq!(start.model, "zai/glm-5.3");
        assert_eq!(start.posture, None);

        let tool = normalize(
            "PreToolUse",
            &json!({"session_id": "pi-1", "tool_name": "Bash"}),
        );
        assert_eq!(tool.tool, "Bash");
        assert_eq!(tool.model, "");
    }
}
