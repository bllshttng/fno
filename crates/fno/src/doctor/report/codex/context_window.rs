//! The codex context-window arithmetic (doctor.py:2891-3055).

use serde_json::{json, Map, Value};

use super::super::codex_home;

/// The ONE gate every cached number goes through, range included
/// (doctor.py:2974-2994). No real window is zero, negative, or astronomical,
/// so the ceiling rejects a corrupt cache without rejecting a served one.
fn number_gate(v: &Value, lo: f64, hi: f64, keep_float: bool) -> Option<f64> {
    if v.is_boolean() || !v.is_number() {
        return None;
    }
    if let Some(i) = v.as_i64() {
        let n = i as f64;
        return (lo..=hi).contains(&n).then_some(n);
    }
    let f = v.as_f64()?;
    if !f.is_finite() {
        return None;
    }
    let n = if keep_float { f } else { f.trunc() };
    (lo..=hi).contains(&n).then_some(n)
}

const WINDOW_CEILING: f64 = 1_000_000_000.0;

fn reason(message: &str) -> Value {
    json!({"reason": message})
}

fn unreadable(e: impl std::fmt::Display) -> Value {
    let text = format!("unreadable-codex-home: {e}");
    reason(text.chars().take(200).collect::<String>().as_str())
}

pub(crate) fn codex_context_window_report(app_server_present: Option<bool>) -> Value {
    let codex_home = codex_home();
    let text = match std::fs::read_to_string(codex_home.join("config.toml")) {
        Ok(t) => t,
        Err(e) => return unreadable(e),
    };
    let doc: toml::Value = match toml::from_str(&text) {
        Ok(d) => d,
        Err(e) => return unreadable(e),
    };
    let cache_text = match std::fs::read_to_string(codex_home.join("models_cache.json")) {
        Ok(t) => t,
        Err(e) => return unreadable(e),
    };
    let cache: Value = match serde_json::from_str(&cache_text) {
        Ok(c) => c,
        Err(e) => return unreadable(e),
    };
    let mut config = match serde_json::to_value(&doc) {
        Ok(Value::Object(map)) => map,
        _ => return unreadable("config.toml root is not a table"),
    };
    let mut source = "model".to_string();
    let mut window_source = "model_context_window".to_string();
    let profile = doc
        .get("profile")
        .and_then(|p| p.as_str())
        .map(String::from);
    if let Some(profile_name) = &profile {
        let table = config
            .get("profiles")
            .and_then(Value::as_object)
            .and_then(|profiles| profiles.get(profile_name))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if table.get("model").is_some() {
            source = format!("profiles.{profile_name}.model");
        }
        if table.get("model_context_window").is_some() {
            window_source = format!("profiles.{profile_name}.model_context_window");
        }
        for (k, v) in table {
            config.insert(k, v);
        }
    }
    let configured = config
        .get("model_context_window")
        .filter(|v| !v.is_boolean() && v.is_number())
        .and_then(Value::as_i64);
    let Some(configured) = configured else {
        return reason("no-configured-window");
    };
    let Some(slug) = config.get("model").and_then(Value::as_str) else {
        return reason("no-configured-model");
    };
    let Some(models) = cache.get("models").and_then(Value::as_array) else {
        return reason("cache-schema-drift: models");
    };
    let entry = models
        .iter()
        .find(|m| m.get("slug").and_then(Value::as_str) == Some(slug));
    let Some(entry) = entry else {
        return reason(format!("model-not-in-cache: {slug}").as_str());
    };
    let cap = number_gate(
        entry.get("max_context_window").unwrap_or(&Value::Null),
        1.0,
        WINDOW_CEILING,
        false,
    );
    let base = number_gate(
        entry.get("context_window").unwrap_or(&Value::Null),
        1.0,
        WINDOW_CEILING,
        false,
    );
    let percent = number_gate(
        entry
            .get("effective_context_window_percent")
            .unwrap_or(&Value::Null),
        1.0,
        100.0,
        true,
    );
    let Some(cap) = cap else {
        return reason(format!("cache-schema-drift: no max_context_window for {slug}").as_str());
    };
    let base_synthesized = base.is_none();
    let base = base.unwrap_or(cap);
    let effective = {
        let mult = percent.unwrap_or(100.0);
        let prod = configured.min(cap as i64) as f64 * mult;
        (prod / 100.0).floor() as i64
    };
    let mut out = Map::new();
    out.insert("model".into(), json!(slug));
    out.insert("model_source".into(), json!(source));
    out.insert("window_source".into(), json!(window_source));
    out.insert("configured".into(), json!(configured));
    out.insert("max_context_window".into(), json!(cap as i64));
    out.insert("base_synthesized".into(), json!(base_synthesized));
    if !base_synthesized {
        out.insert("context_window".into(), json!(base as i64));
    }
    out.insert("effective".into(), json!(effective));
    out.insert(
        "percent".into(),
        percent.map(|p| json!(p)).unwrap_or(Value::Null),
    );
    out.insert("overstated".into(), json!(effective < configured));
    out.insert(
        "leans_on_cached_cap".into(),
        json!(configured > base as i64 || (cap as i64) < configured),
    );
    let present = app_server_present
        .unwrap_or_else(|| super::codex_app_server_report()["present"] == Value::Bool(true));
    out.insert("app_server_running".into(), json!(present));
    out.insert(
        "cache_fetched_at".into(),
        cache.get("fetched_at").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "cache_client_version".into(),
        cache.get("client_version").cloned().unwrap_or(Value::Null),
    );
    Value::Object(out)
}
