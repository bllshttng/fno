//! Served readings for correction rows, one reader per harness (task 1.3).
//!
//! A fact is `{value, measured_at, basis}` or `{value: null, basis:
//! "unmeasured", reason}`: the moment it was true rides the reading, and a
//! missing fact reads unmeasured with its reason, never a guess (laws
//! d-e952ed19, d-10a72d88).

use serde_json::{json, Value};
use std::path::Path;

/// One harness fact at one moment.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reading {
    pub(crate) value: Option<String>,
    /// RFC3339 stamp of the transcript row that named the fact.
    pub(crate) measured_at: Option<String>,
    pub(crate) basis: &'static str,
    pub(crate) reason: Option<&'static str>,
}

impl Reading {
    pub(crate) fn unmeasured(reason: &'static str) -> Self {
        Reading {
            value: None,
            measured_at: None,
            basis: "unmeasured",
            reason: Some(reason),
        }
    }

    pub(crate) fn observed(
        value: String,
        measured_at: Option<String>,
        basis: &'static str,
    ) -> Self {
        Reading {
            value: Some(value),
            measured_at,
            basis: "observed",
            reason: None,
        }
    }

    pub(crate) fn derived(value: String) -> Self {
        Reading::observed(value, None, "derived")
    }

    pub(crate) fn to_json(&self) -> Value {
        match self.value.as_deref() {
            Some(v) => json!({"value": v, "measured_at": self.measured_at, "basis": self.basis}),
            None => json!({ "value": null, "basis": self.basis, "reason": self.reason }),
        }
    }
}

/// The harness facts one correction row's readings carry.
#[derive(Debug, Clone)]
pub(crate) struct HarnessReadings {
    pub(crate) harness: &'static str,
    pub(crate) model: Reading,
    pub(crate) effort: Reading,
    pub(crate) provider: Reading,
    pub(crate) harness_version: Reading,
}

impl HarnessReadings {
    pub(crate) fn all_unmeasured(harness: &'static str, reason: &'static str) -> Self {
        HarnessReadings {
            harness,
            model: Reading::unmeasured(reason),
            effort: Reading::unmeasured(reason),
            provider: Reading::unmeasured(reason),
            harness_version: Reading::unmeasured(reason),
        }
    }

    /// Fill the provider reading from the model when the transcript named no
    /// provider: `basis: derived`, the one fallback this module allows.
    pub(crate) fn derive_provider_from_model(&mut self) {
        if self.provider.value.is_none() {
            if let Some(model) = self.model.value.clone() {
                self.provider = Reading::derived(crate::provider_cap::provider_from_model(&model));
            }
        }
    }
}

/// A row's RFC3339 stamp as written, else `None`.
fn stamp(obj: &Value) -> Option<String> {
    crate::provenance::turn_stamp(obj).map(str::to_string)
}

/// The RFC3339 stamp as epoch seconds, else `None`.
pub(crate) fn stamp_secs(obj: &Value) -> Option<f64> {
    crate::provenance::turn_ts_epoch(obj)
}

pub(crate) fn claude_readings(raw: &str) -> HarnessReadings {
    let mut out = HarnessReadings {
        harness: "claude",
        model: Reading::unmeasured("no assistant row named a model"),
        effort: Reading::unmeasured("no row named an effort"),
        provider: Reading::unmeasured("no row named a provider"),
        harness_version: Reading::unmeasured("no row named a version"),
    };
    for line in raw.lines() {
        if !line.contains("\"assistant\"")
            && !line.contains("\"effort\"")
            && !line.contains("\"version\"")
        {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if obj.get("type").and_then(Value::as_str) == Some("assistant") {
            if let Some(model) = obj
                .pointer("/message/model")
                .and_then(Value::as_str)
                .filter(|m| *m != crate::session_truth::CLAUDE_SYNTHETIC_MODEL)
            {
                out.model = Reading::observed(model.to_string(), stamp(&obj), "observed");
            }
        }
        if let Some(effort) = obj.get("effort").and_then(Value::as_str) {
            out.effort = Reading::observed(effort.to_string(), stamp(&obj), "observed");
        }
        if let Some(v) = obj.get("version").and_then(Value::as_str) {
            out.harness_version = Reading::observed(v.to_string(), stamp(&obj), "observed");
        }
    }
    out
}

pub(crate) fn codex_readings(raw: &str) -> HarnessReadings {
    let mut out = HarnessReadings {
        harness: "codex",
        model: Reading::unmeasured("no turn_context named a model"),
        effort: Reading::unmeasured("no turn_context named an effort"),
        provider: Reading::unmeasured("no session_meta named a provider"),
        harness_version: Reading::unmeasured("no row named a cli_version"),
    };
    for line in raw.lines() {
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let p = obj.get("payload");
        match obj.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if let Some(pv) = p
                    .and_then(|p| p.get("model_provider"))
                    .and_then(Value::as_str)
                {
                    out.provider = Reading::observed(pv.to_string(), stamp(&obj), "observed");
                }
                if let Some(v) = p.and_then(|p| p.get("cli_version")).and_then(Value::as_str) {
                    out.harness_version = Reading::observed(v.to_string(), stamp(&obj), "observed");
                }
            }
            Some("turn_context") => {
                if let Some(model) = p.and_then(|p| p.get("model")).and_then(Value::as_str) {
                    out.model = Reading::observed(model.to_string(), stamp(&obj), "observed");
                }
                if let Some(effort) = p.and_then(|p| p.get("effort")).and_then(Value::as_str) {
                    out.effort = Reading::observed(effort.to_string(), stamp(&obj), "observed");
                }
            }
            _ => {}
        }
    }
    out
}

/// The pi facts live on the raw rows the render drops: message `model` and
/// `provider`, `model_change` and `thinking_level_change` rows.
pub(crate) fn pi_readings(raw: &str) -> HarnessReadings {
    let mut out = HarnessReadings {
        harness: "pi",
        model: Reading::unmeasured("no message or model_change row named a model"),
        effort: Reading::unmeasured("no thinking_level_change row named a level"),
        provider: Reading::unmeasured("no message named a provider"),
        harness_version: Reading::unmeasured("pi rows name no harness version"),
    };
    for line in raw.lines() {
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let msg = obj.get("message");
        match obj.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(model) = msg.and_then(|m| m.get("model")).and_then(Value::as_str) {
                    out.model = Reading::observed(model.to_string(), stamp(&obj), "observed");
                }
                if let Some(pv) = msg.and_then(|m| m.get("provider")).and_then(Value::as_str) {
                    out.provider = Reading::observed(pv.to_string(), stamp(&obj), "observed");
                }
            }
            Some("model_change") => {
                if let Some(model) = obj.get("model").and_then(Value::as_str) {
                    out.model = Reading::observed(model.to_string(), stamp(&obj), "observed");
                }
            }
            Some("thinking_level_change") => {
                if let Some(level) = obj.get("level").and_then(Value::as_str) {
                    out.effort = Reading::observed(level.to_string(), stamp(&obj), "observed");
                }
            }
            _ => {}
        }
    }
    out
}

/// agy: the conversation bodies are protobuf blobs with no named schema in
/// this repo, so every fact reads unmeasured with the same reason.
pub(crate) fn agy_readings() -> HarnessReadings {
    HarnessReadings::all_unmeasured("agy", "conversation steps are protobuf blobs")
}

/// The footnote facts live on the record file's own rows: turn_context
/// `data.model` per turn, model_request `data.provider_id`, header
/// `data.fno_version`.
pub(crate) fn footnote_readings(raw: &str) -> crate::correction_readings::HarnessReadings {
    let mut out = HarnessReadings {
        harness: "footnote",
        model: Reading::unmeasured("no turn_context named a model"),
        effort: Reading::unmeasured("no record type carries an effort"),
        provider: Reading::unmeasured("no model_request named a provider"),
        harness_version: Reading::unmeasured("no header named a fno_version"),
    };
    for line in raw.lines() {
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let d = obj.get("data");
        match obj.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                if let Some(model) = d.and_then(|d| d.get("model")).and_then(Value::as_str) {
                    out.model = Reading::observed(model.to_string(), stamp(&obj), "observed");
                }
            }
            Some("model_request") => {
                if let Some(pv) = d.and_then(|d| d.get("provider_id")).and_then(Value::as_str) {
                    out.provider = Reading::observed(pv.to_string(), stamp(&obj), "observed");
                }
            }
            Some("header") => {
                if let Some(v) = d
                    .and_then(|d| d.get("fno_version"))
                    .and_then(fno_version_label)
                {
                    out.harness_version = Reading::observed(v, stamp(&obj), "observed");
                }
            }
            _ => {}
        }
    }
    out
}

/// The header's fno_version object as one label: git_rev, else package.
fn fno_version_label(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => v
            .get("git_rev")
            .and_then(Value::as_str)
            .or_else(|| v.get("package").and_then(Value::as_str))
            .map(str::to_string),
        _ => None,
    }
}

/// The opencode facts live on the store itself: the session row's `model`
/// and `version` columns, plus the newest assistant message's `modelID`
/// and `providerID` fields.
pub(crate) fn opencode_readings(db: &Path, session_id: &str) -> HarnessReadings {
    let mut out = HarnessReadings {
        harness: "opencode",
        model: Reading::unmeasured("no session row or message named a model"),
        effort: Reading::unmeasured("no message named a variant"),
        provider: Reading::unmeasured("no message named a provider"),
        harness_version: Reading::unmeasured("no session row named a version"),
    };
    let Ok(conn) = crate::opencode_transcript::open_read_only(db) else {
        return HarnessReadings::all_unmeasured("opencode", "store unreadable");
    };
    let session = conn.query_row(
        "SELECT model, version FROM session WHERE id = ?1",
        [session_id],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        },
    );
    if let Ok((model, version)) = session {
        if let Some(model) = model.filter(|m| !m.is_empty()) {
            out.model = Reading::observed(model, None, "observed");
        }
        if let Some(version) = version.filter(|v| !v.is_empty()) {
            out.harness_version = Reading::observed(version, None, "observed");
        }
    }
    let Ok(mut stmt) = conn.prepare(
        "SELECT data FROM message WHERE session_id = ?1 AND data LIKE '%assistant%' ORDER BY time_created DESC LIMIT 20",
    ) else {
        return out;
    };
    let rows = stmt.query_map([session_id], |row| row.get::<_, String>(0));
    for row in rows.into_iter().flatten().flatten() {
        let Ok(obj) = serde_json::from_str::<Value>(&row) else {
            continue;
        };
        if obj.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(model) = obj.get("modelID").and_then(Value::as_str) {
            out.model = Reading::observed(model.to_string(), None, "observed");
            break;
        }
        if out.provider.value.is_none() {
            if let Some(pv) = obj.get("providerID").and_then(Value::as_str) {
                out.provider = Reading::observed(pv.to_string(), None, "observed");
            }
        }
    }
    out
}

/// Dispatch by harness name. `raw` is the transcript text the fold already
/// read; opencode takes the store path beside the session id because its
/// facts live in the store, not the render.
pub(crate) fn readings_for(harness: &str, raw: &str) -> Option<HarnessReadings> {
    match harness {
        "claude" => Some(claude_readings(raw)),
        "codex" => Some(codex_readings(raw)),
        "pi" => Some(pi_readings(raw)),
        "agy" => Some(agy_readings()),
        "footnote" => Some(footnote_readings(raw)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac6_every_fact_carries_a_value_with_measured_at_or_unmeasured_with_reason() {
        let raw = concat!(
            r#"{"type":"assistant","timestamp":"2026-11-01T05:00:00Z","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"hi"}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-11-01T05:00:10Z","effort":"high","message":{"role":"user","content":"x"}}"#,
            "\n",
        );
        let r = claude_readings(raw);
        assert_eq!(r.model.value.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(r.model.measured_at.as_deref(), Some("2026-11-01T05:00:00Z"));
        assert_eq!(r.effort.value.as_deref(), Some("high"));
        assert_eq!(r.provider.basis, "unmeasured");
        assert!(r.provider.reason.is_some());
        // to_json: observed carries value + measured_at; unmeasured carries a reason.
        let j = r.provider.to_json();
        assert_eq!(j["basis"], json!("unmeasured"));
        assert!(j["reason"].is_string());
    }

    #[test]
    fn codex_reads_turn_context_and_session_meta() {
        let raw = concat!(
            r#"{"timestamp":"2026-11-01T05:00:00Z","type":"session_meta","payload":{"model_provider":"openai","cli_version":"1.2.3"}}"#,
            "\n",
            r#"{"timestamp":"2026-11-01T05:00:05Z","type":"turn_context","payload":{"model":"gpt-6.1-sol","effort":"xhigh"}}"#,
            "\n",
        );
        let r = codex_readings(raw);
        assert_eq!(r.model.value.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(r.effort.value.as_deref(), Some("xhigh"));
        assert_eq!(r.provider.value.as_deref(), Some("openai"));
        assert_eq!(r.harness_version.value.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn footnote_reads_turn_context_model_request_and_header() {
        let raw = concat!(
            r#"{"v":1,"ts":"2026-10-22T06:01:13.240Z","type":"header","data":{"fno_version":{"package":"0.4.3","git_rev":"abc"}}}"#,
            "\n",
            r#"{"ts":"2026-10-22T06:01:42Z","type":"turn_context","data":{"model":"glm-5.3-flash"}}"#,
            "\n",
            r#"{"ts":"2026-10-22T06:01:42.586Z","type":"model_request","data":{"provider_id":"zai"}}"#,
            "\n",
        );
        let r = footnote_readings(raw);
        assert_eq!(r.model.value.as_deref(), Some("glm-5.3-flash"));
        assert_eq!(r.provider.value.as_deref(), Some("zai"));
        assert_eq!(r.effort.basis, "unmeasured");
        assert_eq!(r.harness_version.value.as_deref(), Some("abc"));
    }

    #[test]
    fn pi_reads_message_model_change_and_thinking_level_rows() {
        let raw = concat!(
            r#"{"type":"message","message":{"role":"assistant","model":"pi-model","provider":"anthropic","timestamp":1730000000000}}"#,
            "\n",
            r#"{"type":"thinking_level_change","level":"high","timestamp":"2026-11-01T05:00:00Z"}"#,
            "\n",
        );
        let r = pi_readings(raw);
        assert_eq!(r.model.value.as_deref(), Some("pi-model"));
        assert_eq!(r.provider.value.as_deref(), Some("anthropic"));
        assert_eq!(r.effort.value.as_deref(), Some("high"));
    }

    #[test]
    fn agy_reads_unmeasured_and_the_dispatch_covers_five_harnesses() {
        let r = agy_readings();
        for f in [&r.model, &r.effort, &r.provider, &r.harness_version] {
            assert_eq!(f.basis, "unmeasured");
            assert!(f.reason.is_some());
        }
        assert!(readings_for("claude", "").is_some());
        assert!(readings_for("codex", "").is_some());
        assert!(readings_for("pi", "").is_some());
        assert!(readings_for("footnote", "").is_some());
        assert!(readings_for("agy", "").is_some());
        assert!(readings_for("opencode", "").is_none());
    }

    #[test]
    fn derive_provider_fills_only_when_unmeasured() {
        let mut r = agy_readings();
        r.model = Reading::observed("glm-5.3-flash".into(), None, "observed");
        r.derive_provider_from_model();
        assert_eq!(r.provider.value.as_deref(), Some("zai"));
        assert_eq!(r.provider.basis, "derived");
    }
}
