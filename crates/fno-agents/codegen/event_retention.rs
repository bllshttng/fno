use std::collections::HashMap;

use serde_yaml_ng::Value;

pub fn validate(schema: &Value) -> Result<(), String> {
    let retention = &schema["retention"];
    let get = |key: &str| {
        retention
            .as_mapping()
            .and_then(|m| m.get(&Value::String(key.into())))
    };
    let classes = ["ephemeral", "gate", "telemetry", "durable"];
    let default = match get("default") {
        None => "durable",
        Some(value) => value
            .as_str()
            .filter(|v| classes.contains(v))
            .ok_or_else(|| format!("invalid retention default: {value:?}"))?,
    };
    if let Some(minimum) = get("minimum_ephemeral_ttl_hours") {
        if minimum.as_u64().filter(|v| *v > 0).is_none() {
            return Err(format!("invalid minimum_ephemeral_ttl_hours: {minimum:?}"));
        }
    }
    let mut entries = HashMap::new();
    for entry in schema["event_types"].as_sequence().into_iter().flatten() {
        let Some(name) = entry["name"].as_str() else {
            continue;
        };
        let class = match entry
            .as_mapping()
            .and_then(|m| m.get(&Value::String("retention".into())))
        {
            None => default,
            Some(raw) => raw
                .as_str()
                .ok_or_else(|| format!("invalid retention class for {name}: {raw:?}"))?,
        };
        if !classes.contains(&class) {
            return Err(format!("invalid retention class for {name}: {class:?}"));
        }
        entries.insert(name, class);
    }
    let joins = match get("joins") {
        None => None,
        Some(value) => Some(value.as_sequence().ok_or("invalid retention joins")?),
    };
    for pair in joins.into_iter().flatten() {
        let Some(pair) = pair.as_sequence().filter(|pair| pair.len() == 2) else {
            return Err("invalid retention join".into());
        };
        let (Some(left), Some(right)) = (pair[0].as_str(), pair[1].as_str()) else {
            return Err("invalid retention join".into());
        };
        let (Some(left_class), Some(right_class)) = (entries.get(left), entries.get(right)) else {
            return Err(format!(
                "retention join names unknown event type: [{left:?}, {right:?}]"
            ));
        };
        if left_class != right_class {
            return Err(format!(
                "retention join mismatch: {left}={left_class}, {right}={right_class}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_join_cannot_straddle_classes() {
        let schema: Value = serde_yaml_ng::from_str("retention:\n  default: durable\n  joins: [[spawn, contact]]\nevent_types:\n  - name: spawn\n    retention: durable\n  - name: contact\n    retention: ephemeral\n").unwrap();
        assert!(validate(&schema)
            .unwrap_err()
            .contains("retention join mismatch"));
    }
}
