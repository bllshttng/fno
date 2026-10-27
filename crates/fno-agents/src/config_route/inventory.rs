//! Inventory and slot previews composed from the route-slot owner.

use super::*;
use std::io::Write;

pub fn run_capture(json_output: bool) -> (i32, String, String) {
    let inv = slot_call(json!({"mode": "inventory"})).unwrap_or_else(|_| json!({}));
    let objective = inv
        .get("objective")
        .and_then(Value::as_str)
        .filter(|s| ["cheapest-that-clears", "best-available", "prefer-harness"].contains(s))
        .unwrap_or("cheapest-that-clears");
    let prefer = inv
        .get("prefer_harness")
        .and_then(Value::as_str)
        .unwrap_or("");
    let commands = inv
        .get("verbs")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .cloned()
        .unwrap_or_else(|| {
            json!(["think", "blueprint", "target", "review", "role"])
                .as_array()
                .unwrap()
                .clone()
        });
    let mut slots = Vec::new();
    for command in commands
        .iter()
        .filter_map(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        slots.push(slot_states(command));
    }
    let rows = inv
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        let note = "no inventory declared (config.routing.models is empty); the grid records no-inventory-declared";
        let out = if json_output {
            pretty(&json!({"objective": objective, "models": [], "note": note, "slots": slots}))
        } else {
            format!("{note}\n{}", echo_slots(&slots))
        };
        return (0, out, String::new());
    }
    let answer = slot_call(json!({
        "mode": "inventory", "inventory": {"rows": rows},
        "known_harnesses": crate::provider::KNOWN_PROVIDERS,
    }))
    .unwrap_or_else(|e| json!({"refusals": [format!("route-slot-unavailable ({e})")]}));
    let rows = answer
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let drift = answer
        .get("drift")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let out = if json_output {
        let fingerprint = slots
            .iter()
            .filter_map(|s| s.get("fingerprint").and_then(Value::as_str))
            .find(|s| !s.is_empty())
            .unwrap_or("");
        let policy = slot_call(json!({"mode": "policy"}))
            .unwrap_or_else(|_| json!({"enforce_inventory": false, "operator_access": "unknown"}));
        pretty(
            &json!({"objective": objective, "prefer_harness": prefer, "models": rows, "slots": slots, "fingerprint": fingerprint, "policy": policy, "drift": drift}),
        )
    } else {
        let mut out = format!(
            "objective={objective}{}\n",
            if prefer.is_empty() {
                String::new()
            } else {
                format!(" prefer_harness={prefer}")
            }
        );
        let cols = [
            "name",
            "harness",
            "model",
            "band",
            "percentile",
            "effort",
            "window",
            "verdict",
        ];
        let headers: Vec<_> = cols.iter().map(|c| c.to_uppercase()).collect();
        out.push_str(&render_table(
            &cols,
            &headers.iter().map(String::as_str).collect::<Vec<_>>(),
            &rows,
        ));
        out.push_str(&echo_slots(&slots));
        for line in drift.iter().filter_map(Value::as_str) {
            out.push_str(&format!("{line}\n"));
        }
        out
    };
    let err = answer
        .get("refusals")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|s| format!("refused: {s}\n"))
        .collect::<String>();
    (0, out, err)
}

/// Keep the route-slot entry's gather and states dispatch in-process. The
/// payload file adapts its capture API without taking over process stdin.
fn slot_call(payload: Value) -> Result<Value, String> {
    let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    file.write_all(payload.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let (code, out, err) =
        crate::route_slot::run_route_slot_capture(&[file.path().to_string_lossy().into_owned()]);
    if code != 0 {
        return Err(err);
    }
    serde_json::from_str(&out).map_err(|e| e.to_string())
}

fn slot_states(command: &str) -> Value {
    let rung = format!("agents.profiles.{command}");
    let states = slot_call(json!({
        "rung_base": rung, "node": null, "substrate": null,
        "permission_mode": null, "constrain_harness": null,
        "explicit_lane": false, "gate_bypassed": false,
        "model_occupied": false, "work_command": command, "mode": "states",
    }))
    .unwrap_or_else(|e| json!({"would_take": format!("slot=route-slot-unavailable ({e})")}));
    let mut out = json!({"verb": command, "lanes": [], "on_exhausted": "", "would_take": "", "routing": "unarmed"});
    for key in [
        "on_exhausted",
        "on_low",
        "on_unknown",
        "would_take",
        "routing",
        "work_kind",
        "operator_access",
        "policy_source",
        "skipped",
        "fingerprint",
    ] {
        if let Some(value) = states.get(key) {
            out[key] = value.clone();
        }
    }
    for line in states
        .get("chain")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some(note) = line.strip_prefix(&format!("slot note {rung} ")) {
            out["note"] = json!(note);
        }
    }
    let lanes = states.get("lane_states").and_then(Value::as_array).into_iter().flatten().map(|entry| {
        let mut lane = json!({"rung": cell(entry, "rung"), "name": cell(entry, "name"), "state": entry.get("state").map(value_string).unwrap_or_else(|| "unknown".into())});
        for key in ["identity", "source"] {
            if let Some(value) = entry.get(key).filter(|v| !v.is_null() && v.as_str() != Some("")) { lane[key] = json!(value_string(value)); }
        }
        lane
    }).collect::<Vec<_>>();
    out["lanes"] = json!(lanes);
    out
}

fn echo_slots(slots: &[Value]) -> String {
    let mut out = "slots: preview (simulated; no launch)\n".to_string();
    for slot in slots {
        let lanes = slot
            .get("lanes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if lanes.is_empty() {
            out.push_str(&format!(
                "  {}: {}\n",
                cell(slot, "verb"),
                cell(slot, "would_take")
            ));
            continue;
        }
        let lane_text = lanes
            .iter()
            .map(|lane| {
                let rung = cell(lane, "rung");
                let mut text = format!(
                    "{} {} capacity={}",
                    rung.rsplit('.').next().unwrap_or(""),
                    cell(lane, "name"),
                    cell(lane, "state")
                );
                if !cell(lane, "identity").is_empty() {
                    text.push_str(&format!(" identity={}", cell(lane, "identity")));
                }
                text
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("  {}: {lane_text}", cell(slot, "verb")));
        if !cell(slot, "on_exhausted").is_empty() {
            out.push_str(&format!("; on_exhausted={}", cell(slot, "on_exhausted")));
        }
        out.push_str(&format!(
            "; would take {}; routing={}",
            cell(slot, "would_take"),
            slot.get("routing")
                .and_then(Value::as_str)
                .unwrap_or("unarmed")
        ));
        for (key, label) in [("work_kind", "work"), ("operator_access", "access")] {
            if !cell(slot, key).is_empty() {
                out.push_str(&format!("; {label}={}", cell(slot, key)));
            }
        }
        out.push('\n');
        for reason in slot
            .get("skipped")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            out.push_str(&format!("    {reason}\n"));
        }
    }
    out
}

pub(super) fn value_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(v) => if *v { "True" } else { "False" }.into(),
        _ => value.to_string(),
    }
}

fn cell(row: &Value, key: &str) -> String {
    row.get(key).map(value_string).unwrap_or_default()
}

pub(super) fn render_table(cols: &[&str], headers: &[&str], rows: &[Value]) -> String {
    let widths: Vec<_> = cols
        .iter()
        .zip(headers)
        .map(|(c, h)| {
            rows.iter()
                .map(|r| cell(r, c).chars().count())
                .chain([h.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let render = |cells: Vec<String>| {
        cells
            .iter()
            .zip(&widths)
            .map(|(s, w)| format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count()))))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
            + "\n"
    };
    let mut out = render(headers.iter().map(|s| s.to_string()).collect());
    for row in rows {
        out.push_str(&render(cols.iter().map(|c| cell(row, c)).collect()));
    }
    out
}

pub(super) fn pretty(value: &Value) -> String {
    let text = serde_json::to_string_pretty(value).expect("JSON value serializes");
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out + "\n"
}
