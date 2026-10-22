//! The opencode hook payload: the claude-shaped JSON the plugin
//! (`cli/src/fno/setup/assets/opencode/footnote.js`) builds from opencode's
//! plugin events, the same convention it already uses for hooks.json.
//!
//! The plugin owns the event translation (`chat.message` -> UserPromptSubmit,
//! `tool.execute.before` -> PreToolUse, idle -> Stop, a permission prompt ->
//! Notification), so this adapter only reads fields. `model` arrives as
//! `provider/model`, opencode's own `--model` spelling, so the daemon's
//! served-model diff compares like with like. opencode names no effort or
//! posture field, so those reads stay tolerant.

use super::{effort_field, str_field, HookEvent};
use serde_json::Value;

pub(crate) fn normalize(event: &str, payload: &Value) -> HookEvent {
    HookEvent {
        harness: "opencode".into(),
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

    /// A prompt fire carries the served model; a permission Notification
    /// carries the text the operator is asked; a bare Stop carries neither.
    #[test]
    fn opencode_payloads_normalize_to_the_hook_event_shape() {
        let prompt = normalize(
            "UserPromptSubmit",
            &json!({"session_id": "ses_1", "model": "zai/glm-5.3", "effort": "high"}),
        );
        assert_eq!(prompt.session_id, "ses_1");
        assert_eq!(prompt.model, "zai/glm-5.3");
        assert_eq!(prompt.effort, "high");
        assert_eq!(prompt.posture, None);

        let ask = normalize(
            "Notification",
            &json!({"session_id": "ses_1", "message": "Run: rm -rf build"}),
        );
        assert_eq!(ask.message, "Run: rm -rf build");

        let stop = normalize("Stop", &json!({"session_id": "ses_1"}));
        assert_eq!(stop.model, "");
        assert_eq!(stop.tool, "");
    }
}
