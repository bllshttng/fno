//! MCP `claude/channel` wire format: the single source of truth for the
//! `notifications/claude/channel` envelope shape this runtime emits to
//! Claude Code. Ported verbatim from the deleted `cli/src/fno/mcp/channel.py`
//! (goldens captured from the Python leg before deletion, per the port
//! protocol in docs/architecture/dual-implementation-inventory.md).
//!
//! Two consumers:
//! - [`crate::mcp::server`] forwards delivered envelopes verbatim to CC on
//!   stdout after validating them here (drift demotes the channel).
//! - [`crate::mcp::client`] ships the envelope across the Unix socket to the
//!   sidecar, which routes it to the registered channel server.
//!
//! The reason strings returned by [`validate_envelope`] are machine-stable
//! and byte-match the Python reasons (including the `repr` single quotes),
//! so `mcp_channel_envelope_drift` event payloads keep slicing the same way.

use serde_json::{Map, Value};

/// Wire-format version pinned by the characterization tests. Bump in
/// lockstep with any deliberate envelope change.
pub const ENVELOPE_VERSION: &str = "1";

/// JSON-RPC method per the channels reference, Notification format.
pub const MCP_CHANNEL_METHOD: &str = "notifications/claude/channel";

/// The meta-key rule: CC silently drops keys outside `[A-Za-z0-9_]+`
/// (hyphens, dots, spaces), so the builder refuses to emit them and the
/// validator demotes them on receipt.
pub fn meta_key_ok(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The Python type name of a JSON value, for the builder's error strings:
/// `json.loads` yields `dict` / `list` / `str` / `int` / `float` / `bool` /
/// `NoneType`, and the deleted Python leg spelled those names into every
/// message. The characterization tests pin the mapping.
fn py_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python `repr` of a string: single quotes. The reason strings and the
/// builder's offender lists carry these.
fn py_repr_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| format!("'{s}'")).collect();
    format!("[{}]", quoted.join(", "))
}

/// Validate a meta map against the identifier-key rule, returning it
/// unchanged on success. `None` collapses to an empty map.
fn validate_meta(meta: Option<&Map<String, Value>>) -> Result<Map<String, Value>, String> {
    let Some(meta) = meta else {
        return Ok(Map::new());
    };
    let mut bad: Vec<String> = meta.keys().filter(|k| !meta_key_ok(k)).cloned().collect();
    bad.sort();
    if !bad.is_empty() {
        return Err(format!(
            "meta keys must match [A-Za-z0-9_]+ (CC silently drops non-identifier keys); \
             offenders: {}",
            py_repr_list(&bad)
        ));
    }
    let mut bad_vals: Vec<String> = meta
        .iter()
        .filter(|(_, v)| !v.is_string())
        .map(|(k, _)| k.clone())
        .collect();
    bad_vals.sort();
    if !bad_vals.is_empty() {
        return Err(format!(
            "meta values must be strings; non-string keys: {}",
            py_repr_list(&bad_vals)
        ));
    }
    Ok(meta.clone())
}

/// Build a `notifications/claude/channel` JSON-RPC envelope.
///
/// `request_id`, when present, lands on `meta["request_id"]` so the
/// recipient sees it as a `<channel>` tag attribute, joining event pairs
/// across the wire. Receive-side validation tolerates an absent
/// `request_id`; only documented senders include one. An empty
/// `request_id` fails here, at the producer, never silently on the wire.
pub fn build_channel_notification(
    content: &str,
    meta: Option<&Map<String, Value>>,
    request_id: Option<&str>,
) -> Result<Value, String> {
    if let Some(rid) = request_id {
        if rid.is_empty() {
            return Err(format!(
                "request_id must be a non-empty string, got str='{}'",
                rid
            ));
        }
    }
    let mut merged = validate_meta(meta)?;
    if let Some(rid) = request_id {
        merged.insert("request_id".to_string(), Value::String(rid.to_string()));
    }
    let mut params = Map::new();
    params.insert("content".to_string(), Value::String(content.to_string()));
    params.insert("meta".to_string(), Value::Object(merged));
    let mut envelope = Map::new();
    envelope.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    envelope.insert(
        "method".to_string(),
        Value::String(MCP_CHANNEL_METHOD.to_string()),
    );
    envelope.insert("params".to_string(), Value::Object(params));
    Ok(Value::Object(envelope))
}

/// Validate an inbound (or self-emitted) envelope against the wire
/// contract. `Ok(())` means acceptable; `Err(reason)` carries a short,
/// machine-stable reason for the `mcp_channel_envelope_drift` event, so a
/// Claude Code upgrade that restructures the envelope demotes the channel
/// instead of corrupting downstream state.
pub fn validate_envelope(envelope: &Value) -> Result<(), String> {
    let Some(obj) = envelope.as_object() else {
        return Err(format!("envelope_not_dict:{}", py_type(envelope)));
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("jsonrpc_version_missing_or_not_2.0".to_string());
    }
    let method = obj.get("method");
    if method.and_then(Value::as_str) != Some(MCP_CHANNEL_METHOD) {
        let shown = match method {
            Some(Value::String(s)) => format!("'{s}'"),
            Some(other) => format!("{}", other),
            None => "None".to_string(),
        };
        return Err(format!("method_mismatch:{shown}"));
    }
    let Some(params) = obj.get("params").and_then(Value::as_object) else {
        return Err("params_not_dict".to_string());
    };
    let Some(content) = params.get("content") else {
        return Err("content_missing".to_string());
    };
    if !content.is_string() {
        return Err("content_not_string".to_string());
    }
    let Some(meta) = params
        .get("meta")
        .unwrap_or(&Value::Object(Map::new()))
        .as_object()
    else {
        return Err("meta_not_dict".to_string());
    };
    for (k, v) in meta {
        if !meta_key_ok(k) {
            return Err(format!("meta_key_invalid:'{k}'"));
        }
        if !v.is_string() {
            return Err(format!("meta_value_not_string:'{k}'"));
        }
    }
    Ok(())
}

/// Compare two envelopes and return a diff for the
/// `mcp_channel_envelope_drift` event. Only structural keys are pinned:
/// `content` and `meta` values are user-controlled and never compared.
pub fn envelope_drift_diff(expected: &Value, received: &Value) -> Value {
    let keys = |v: &Value| -> Vec<String> {
        v.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    let mut diff = Map::new();
    let exp_top = keys(expected);
    let rec_top = keys(received);
    let missing: Vec<String> = exp_top
        .iter()
        .filter(|k| !rec_top.contains(k))
        .cloned()
        .collect();
    if !missing.is_empty() {
        diff.insert("missing_top_keys".to_string(), py_string_list(&missing));
    }
    let extra: Vec<String> = rec_top
        .iter()
        .filter(|k| !exp_top.contains(k))
        .cloned()
        .collect();
    if !extra.is_empty() {
        diff.insert("extra_top_keys".to_string(), py_string_list(&extra));
    }
    let method = |v: &Value| v.get("method").cloned().unwrap_or(Value::Null);
    if method(expected) != method(received) {
        diff.insert("method_expected".to_string(), method(expected));
        diff.insert("method_received".to_string(), method(received));
    }
    let params = |v: &Value| v.get("params").cloned().unwrap_or(Value::Null);
    let exp_params = keys(&params(expected));
    let rec_params = keys(&params(received));
    let p_missing: Vec<String> = exp_params
        .iter()
        .filter(|k| !rec_params.contains(k))
        .cloned()
        .collect();
    if !p_missing.is_empty() {
        diff.insert("params_missing".to_string(), py_string_list(&p_missing));
    }
    let p_extra: Vec<String> = rec_params
        .iter()
        .filter(|k| !exp_params.contains(k))
        .cloned()
        .collect();
    if !p_extra.is_empty() {
        diff.insert("params_extra".to_string(), py_string_list(&p_extra));
    }
    Value::Object(diff)
}

fn py_string_list(items: &[String]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|s| Value::String(s.clone()))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_minimal_envelope_has_the_pinned_shape() {
        let env = build_channel_notification("hello", None, None).unwrap();
        assert_eq!(
            env,
            json!({
                "jsonrpc": "2.0",
                "method": MCP_CHANNEL_METHOD,
                "params": {"content": "hello", "meta": {}},
            })
        );
        assert_eq!(ENVELOPE_VERSION, "1");
    }

    #[test]
    fn a_non_identifier_meta_key_is_refused_at_build() {
        let mut meta = Map::new();
        meta.insert("chat-id".to_string(), Value::String("v".to_string()));
        let err = build_channel_notification("x", Some(&meta), None).unwrap_err();
        assert!(err.starts_with("meta keys must match"), "{err}");
        assert!(err.contains("'chat-id'"), "{err}");
    }

    #[test]
    fn a_non_string_meta_value_is_refused_at_build() {
        let mut meta = Map::new();
        meta.insert("k".to_string(), json!(123));
        let err = build_channel_notification("x", Some(&meta), None).unwrap_err();
        assert!(err.starts_with("meta values must be strings"), "{err}");
    }

    #[test]
    fn a_method_drift_is_reported_with_the_python_reason_shape() {
        let mut env = build_channel_notification("hi", None, None).unwrap();
        env["method"] = json!("notifications/some_other/channel");
        let reason = validate_envelope(&env).unwrap_err();
        assert_eq!(reason, "method_mismatch:'notifications/some_other/channel'");
    }

    #[test]
    fn a_missing_content_is_reported_as_content_missing() {
        let env = json!({
            "jsonrpc": "2.0",
            "method": MCP_CHANNEL_METHOD,
            "params": {"meta": {}},
        });
        assert_eq!(validate_envelope(&env).unwrap_err(), "content_missing");
    }

    #[test]
    fn a_non_identifier_meta_key_is_reported_on_receipt() {
        let env = json!({
            "jsonrpc": "2.0",
            "method": MCP_CHANNEL_METHOD,
            "params": {"content": "x", "meta": {"chat-id": "abc"}},
        });
        let reason = validate_envelope(&env).unwrap_err();
        assert_eq!(reason, "meta_key_invalid:'chat-id'");
    }

    #[test]
    fn a_method_rename_surfaces_in_the_drift_diff() {
        let a = build_channel_notification("x", None, None).unwrap();
        let mut b = a.clone();
        b["method"] = json!("notifications/new/channel");
        let diff = envelope_drift_diff(&a, &b);
        assert_eq!(diff["method_expected"], json!(MCP_CHANNEL_METHOD));
        assert_eq!(diff["method_received"], json!("notifications/new/channel"));
    }

    #[test]
    fn a_request_id_merges_over_an_existing_meta_value() {
        let mut meta = Map::new();
        meta.insert("from_name".to_string(), Value::String("alpha".to_string()));
        meta.insert(
            "request_id".to_string(),
            Value::String("old-shouldnt-survive".to_string()),
        );
        let env =
            build_channel_notification("hi", Some(&meta), Some("a1b2c3d4e5f6789012345678901234ab"))
                .unwrap();
        assert_eq!(env["params"]["meta"]["from_name"], json!("alpha"));
        assert_eq!(
            env["params"]["meta"]["request_id"],
            json!("a1b2c3d4e5f6789012345678901234ab")
        );
    }

    #[test]
    fn an_empty_request_id_fails_loud_at_the_producer() {
        let err = build_channel_notification("hi", None, Some("")).unwrap_err();
        assert!(err.contains("request_id"), "{err}");
    }

    #[test]
    fn a_non_dict_envelope_names_the_python_type() {
        assert_eq!(
            validate_envelope(&json!([1, 2])).unwrap_err(),
            "envelope_not_dict:list"
        );
        assert_eq!(
            validate_envelope(&Value::Null).unwrap_err(),
            "envelope_not_dict:NoneType"
        );
        assert_eq!(
            validate_envelope(&json!("s")).unwrap_err(),
            "envelope_not_dict:str"
        );
        assert_eq!(
            validate_envelope(&json!(1.5)).unwrap_err(),
            "envelope_not_dict:float"
        );
    }
}
