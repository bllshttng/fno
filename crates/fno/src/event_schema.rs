use serde_json::{json, Value};

pub(super) fn legacy_projection() -> Result<Value, String> {
    let schema: Value = serde_json::from_str(include_str!("event_store/events_schema.json"))
        .map_err(|error| format!("invalid compiled event schema: {error}"))?;
    let mut sources: Vec<_> = schema["allowed_sources"]
        .as_array()
        .ok_or("compiled event schema has no allowed sources")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    sources.sort();
    let mut source_anyof = vec![json!({"enum": sources})];
    source_anyof.extend(
        schema["source_patterns"]
            .as_array()
            .ok_or("compiled event schema has no source patterns")?
            .iter()
            .map(|pattern| json!({"pattern": pattern})),
    );
    let envelope = json!({
        "$comment": "Unified envelope. Emitted by the native event schema.",
        "type": "object", "required": ["ts", "type", "source", "data"],
        "properties": {
            "ts": {"type": "string", "description": "UTC RFC3339 timestamp"},
            "type": {"type": "string", "description": "Event type name from events-schema.yaml event_types"},
            "source": {"type": "string", "anyOf": source_anyof, "description": "Producer identity: a fixed-string source or a per-agent worker"},
            "data": {"type": "object", "description": "Per-type payload object"}
        },
        "additionalProperties": true
    });
    let infrastructure = [
        "daemon",
        "subagent",
        "loop",
        "pr-heal",
        "active-backlog",
        "rust",
    ];
    let mut types: Vec<_> = schema["event_types"]
        .as_object()
        .ok_or("compiled event schema has no event types")?
        .iter()
        .filter(|(_, spec)| {
            spec["sources"].as_array().is_some_and(|sources| {
                sources
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|source| !infrastructure.contains(&source))
            })
        })
        .map(|(kind, _)| kind.as_str())
        .collect();
    types.sort();
    Ok(json!({"envelope": envelope, "event_types": types}))
}
