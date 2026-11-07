//! The pi hook payload: the claude-shaped JSON the extension
//! (`cli/src/fno/setup/assets/pi/footnote.ts`) builds from pi's extension
//! events. pi exposes no permission-prompt event, so no Notification arrives.

use super::HookEvent;
use serde_json::Value;

pub(crate) fn normalize(event: &str, payload: &Value) -> HookEvent {
    super::plugin_shaped("pi", event, payload)
}

/// Translate a pi tool_call payload (`{toolName, input, cwd, session_id}`,
/// sent by the extension) into the claude shape the pretool host's guards
/// read. pi names its shell `bash` and its file tools `edit`/`write` with
/// the path under `input.path` (the mapping the extension used to do in
/// TypeScript); every other tool rides verbatim, unjudged by name, so a
/// matcher can still select it.
pub fn to_claude_pretool(v: Value) -> Value {
    let Some(obj) = v.as_object() else {
        return Value::Null;
    };
    let name = obj.get("toolName").and_then(Value::as_str).unwrap_or("");
    let mut ti = match obj.get("input") {
        Some(Value::Object(map)) => map.clone(),
        _ => serde_json::Map::new(),
    };
    if !ti.contains_key("file_path") {
        if let Some(p) = ti.get("path").and_then(Value::as_str) {
            ti.insert("file_path".to_string(), Value::String(p.to_string()));
        }
    }
    let tool = match name {
        "bash" => "Bash",
        "edit" => "Edit",
        "write" => "Write",
        other => other,
    };
    serde_json::json!({
        "tool_name": tool,
        "tool_input": Value::Object(ti),
        "session_id": obj.get("session_id").cloned().unwrap_or(Value::Null),
        "cwd": obj.get("cwd").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The three shapes: bash/edit/write map with the path key promoted,
    /// an unmapped tool rides verbatim (unjudged by name, matcher-judged),
    /// and a non-object payload stays Null (the host allows).
    #[test]
    fn pi_tool_call_translates_to_the_claude_shape() {
        let bash = to_claude_pretool(json!({
            "toolName": "bash",
            "input": {"command": "ls"},
            "cwd": "/repo",
            "session_id": "s1",
        }));
        assert_eq!(bash.get("tool_name").and_then(Value::as_str), Some("Bash"));
        assert_eq!(
            bash.get("tool_input")
                .and_then(|t| t.get("command"))
                .and_then(Value::as_str),
            Some("ls")
        );
        assert_eq!(bash.get("cwd").and_then(Value::as_str), Some("/repo"));

        let write = to_claude_pretool(json!({
            "toolName": "write",
            "input": {"path": "/tmp/a.md", "content": "x"},
            "cwd": "/repo",
            "session_id": "s1",
        }));
        assert_eq!(
            write.get("tool_name").and_then(Value::as_str),
            Some("Write")
        );
        assert_eq!(
            write
                .pointer("/tool_input/file_path")
                .and_then(Value::as_str),
            Some("/tmp/a.md")
        );

        let other = to_claude_pretool(json!({
            "toolName": "NotebookEdit",
            "input": {"path": "a.ipynb"},
            "cwd": "/repo",
        }));
        assert_eq!(
            other.get("tool_name").and_then(Value::as_str),
            Some("NotebookEdit")
        );
        assert_eq!(
            other
                .pointer("/tool_input/file_path")
                .and_then(Value::as_str),
            Some("a.ipynb")
        );

        assert_eq!(to_claude_pretool(json!("not an object")), Value::Null);
    }
}
