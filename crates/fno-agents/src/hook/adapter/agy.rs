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

    /// PreInvocation is agy's turn start: the conversation id files the
    /// report, the invocation-only fields ride nothing.
    #[test]
    fn pre_invocation_files_under_the_conversation_id() {
        let ev = normalize(
            "PreInvocation",
            &json!({
                "invocationNum": 3,
                "initialNumSteps": 10,
                "conversationId": "ec33ebf9-0cba-4100-8142-c61503f6c587",
                "workspacePaths": ["/workspace/project"]
            }),
        );
        assert_eq!(ev.session_id, "ec33ebf9-0cba-4100-8142-c61503f6c587");
        assert_eq!(ev.tool, "");
        assert_eq!(ev.model, "");
        assert_eq!(ev.effort, "");
        assert_eq!(ev.posture, None);
    }

    /// The Stop payload: same session id, and the tolerant axes ride only
    /// when a payload actually names them.
    #[test]
    fn stop_files_under_the_conversation_id_and_rides_tolerant_axes() {
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
    }

    /// A PreToolUse payload names its tool verbatim (the adapter decides
    /// nothing about state, and the mapping to claude tool names stays with
    /// the lead guard's translation).
    #[test]
    fn tool_call_names_ride_verbatim() {
        let ev = normalize(
            "PreToolUse",
            &json!({
                "conversationId": "conv-3",
                "toolCall": {"name": "run_command", "args": {"CommandLine": "ls"}}
            }),
        );
        assert_eq!(ev.tool, "run_command");
        assert_eq!(ev.session_id, "conv-3");
    }
}
