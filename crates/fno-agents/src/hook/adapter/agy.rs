//! The agy (Antigravity) hook payload: claude-shaped event names over a
//! Gemini-family camelCase wire (agy 1.2.7's embedded hook contract, the
//! same shape `hook::lead_guard_wire::agy_to_claude` translates).
//!
//! Measured payload fields (the vendor hooks reference): every event
//! carries `conversationId` and `workspacePaths`; PreInvocation adds
//! `invocationNum`, Stop adds `executionNum` / `terminationReason` /
//! `fullyIdle` / `transcriptPath`. The contract names NO model, effort or
//! posture field, so those reads are tolerant: a payload that carries one
//! verifies the row's axis, one that does not leaves it to the axis that
//! owns it.

use super::{effort_field, str_field, HookEvent};
use serde_json::Value;

pub(crate) fn normalize(event: &str, payload: &Value) -> HookEvent {
    HookEvent {
        harness: "agy".into(),
        event: event.into(),
        session_id: str_field(payload, "conversationId"),
        tool: payload
            .get("toolCall")
            .and_then(|tc| tc.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
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

    /// The three payload shapes: PreInvocation (agy's turn start) and Stop
    /// both file under the camelCase conversation id, the tolerant axes ride
    /// only when a payload actually names them, and a PreToolUse tool name
    /// rides verbatim (the mapping to claude tool names stays with the lead
    /// guard's translation).
    #[test]
    fn agy_payloads_normalize_to_the_hook_event_shape() {
        let pre = normalize(
            "PreInvocation",
            &json!({
                "invocationNum": 3,
                "initialNumSteps": 10,
                "conversationId": "ec33ebf9-0cba-4100-8142-c61503f6c587",
                "workspacePaths": ["/workspace/project"]
            }),
        );
        assert_eq!(pre.session_id, "ec33ebf9-0cba-4100-8142-c61503f6c587");
        assert_eq!(pre.tool, "");
        assert_eq!(pre.model, "");
        assert_eq!(pre.effort, "");
        assert_eq!(pre.posture, None);

        let bare = normalize(
            "Stop",
            &json!({
                "executionNum": 1,
                "terminationReason": "model_stop",
                "fullyIdle": true,
                "conversationId": "conv-2"
            }),
        );
        assert_eq!(bare.session_id, "conv-2");
        assert_eq!(bare.model, "");
        assert_eq!(bare.effort, "");

        let axes = normalize(
            "Stop",
            &json!({"conversationId": "conv-2", "model": "glm-5.3", "effort": "high"}),
        );
        assert_eq!(axes.model, "glm-5.3");
        assert_eq!(axes.effort, "high");

        let tool = normalize(
            "PreToolUse",
            &json!({
                "conversationId": "conv-3",
                "toolCall": {"name": "run_command", "args": {"CommandLine": "ls"}}
            }),
        );
        assert_eq!(tool.tool, "run_command");
        assert_eq!(tool.session_id, "conv-3");
    }
}
