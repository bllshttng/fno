//! Served readings for correction rows, one reader per harness (task 1.3).
//!
//! A fact is `{value, measured_at, basis}` or `{value: null, basis:
//! "unmeasured", reason}`: the moment it was true rides the reading, and a
//! missing fact reads unmeasured with its reason, never a guess (laws
//! d-e952ed19, d-10a72d88).

use serde_json::{json, Value};
use std::path::Path;

/// One harness fact at one moment. Owned strings because the fold persists
/// each session's latest readings in its cursor between ticks.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Reading {
    pub(crate) value: Option<String>,
    /// RFC3339 stamp of the transcript row that named the fact.
    pub(crate) measured_at: Option<String>,
    pub(crate) basis: String,
    pub(crate) reason: Option<String>,
}

impl Reading {
    pub(crate) fn unmeasured(reason: &str) -> Self {
        Reading {
            value: None,
            measured_at: None,
            basis: "unmeasured".to_string(),
            reason: Some(reason.to_string()),
        }
    }

    pub(crate) fn with_basis(value: String, measured_at: Option<String>, basis: &str) -> Self {
        Reading {
            value: Some(value),
            measured_at,
            basis: basis.to_string(),
            reason: None,
        }
    }

    pub(crate) fn observed(value: String, measured_at: Option<String>) -> Self {
        Reading::with_basis(value, measured_at, "observed")
    }

    pub(crate) fn to_json(&self) -> Value {
        match self.value.as_deref() {
            Some(v) => json!({"value": v, "measured_at": self.measured_at, "basis": self.basis}),
            None => json!({ "value": null, "basis": self.basis, "reason": self.reason }),
        }
    }
}

/// The harness facts one correction row's readings carry.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HarnessReadings {
    pub(crate) harness: String,
    pub(crate) model: Reading,
    pub(crate) effort: Reading,
    pub(crate) provider: Reading,
    pub(crate) harness_version: Reading,
}

impl HarnessReadings {
    /// The state before any row was read, per harness, with the reason each
    /// fact stays unmeasured when no row ever names it.
    pub(crate) fn empty(harness: &str) -> Self {
        let (model, effort, provider, version) = match harness {
            "claude" => (
                "no assistant row named a model",
                "no row named an effort",
                "no row named a provider",
                "no row named a version",
            ),
            "codex" => (
                "no turn_context named a model",
                "no turn_context named an effort",
                "no session_meta named a provider",
                "no row named a cli_version",
            ),
            "pi" => (
                "no message or model_change row named a model",
                "no thinking_level_change row named a level",
                "no message named a provider",
                "pi rows name no harness version",
            ),
            "footnote" => (
                "no turn_context named a model",
                "no record type carries an effort",
                "no model_request named a provider",
                "no header named a fno_version",
            ),
            "opencode" => (
                "no session row or message named a model",
                "no message named a variant",
                "no message named a provider",
                "no session row named a version",
            ),
            _ => (
                "conversation steps are protobuf blobs",
                "conversation steps are protobuf blobs",
                "conversation steps are protobuf blobs",
                "conversation steps are protobuf blobs",
            ),
        };
        HarnessReadings {
            harness: harness.to_string(),
            model: Reading::unmeasured(model),
            effort: Reading::unmeasured(effort),
            provider: Reading::unmeasured(provider),
            harness_version: Reading::unmeasured(version),
        }
    }

    /// Fold one raw transcript row into the latest readings. The fold calls
    /// this line by line, so a row's readings are the facts true at its turn.
    pub(crate) fn observe(&mut self, obj: &Value) {
        match self.harness.as_str() {
            "claude" => self.observe_claude(obj),
            "codex" => self.observe_codex(obj),
            "pi" => self.observe_pi(obj),
            "footnote" => self.observe_footnote(obj),
            _ => {}
        }
    }

    /// Fill the provider reading from the model when the transcript named no
    /// provider: `basis: derived`, the one fallback this module allows. A
    /// model that reads unmeasured derives nothing.
    pub(crate) fn derive_provider_from_model(&mut self) {
        if self.provider.value.is_none() {
            if let Some(model) = self.model.value.clone() {
                self.provider = Reading::with_basis(
                    crate::provider_cap::provider_from_model(&model),
                    self.model.measured_at.clone(),
                    "derived",
                );
            }
        }
    }

    fn observe_claude(&mut self, obj: &Value) {
        if obj.get("type").and_then(Value::as_str) == Some("assistant") {
            if let Some(model) = obj
                .pointer("/message/model")
                .and_then(Value::as_str)
                .filter(|m| *m != crate::session_truth::CLAUDE_SYNTHETIC_MODEL)
            {
                self.model = claude_model_reading(model, obj);
            }
        }
        if let Some(effort) = obj.get("effort").and_then(Value::as_str) {
            self.effort = Reading::observed(effort.to_string(), stamp(obj));
        }
        if let Some(v) = obj.get("version").and_then(Value::as_str) {
            self.harness_version = Reading::observed(v.to_string(), stamp(obj));
        }
    }

    fn observe_codex(&mut self, obj: &Value) {
        let p = obj.get("payload");
        let field = |key: &str| p.and_then(|p| p.get(key)).and_then(Value::as_str);
        match obj.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if let Some(pv) = field("model_provider") {
                    self.provider = Reading::observed(pv.to_string(), stamp(obj));
                }
                if let Some(v) = field("cli_version") {
                    self.harness_version = Reading::observed(v.to_string(), stamp(obj));
                }
            }
            Some("turn_context") => {
                if let Some(model) = field("model") {
                    self.model = Reading::observed(model.to_string(), stamp(obj));
                }
                if let Some(effort) = field("effort") {
                    self.effort = Reading::observed(effort.to_string(), stamp(obj));
                }
            }
            _ => {}
        }
    }

    /// The pi facts live on the raw rows the render drops: message `model`
    /// and `provider`, `model_change` and `thinking_level_change` rows.
    fn observe_pi(&mut self, obj: &Value) {
        let msg = obj.get("message");
        match obj.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(model) = msg.and_then(|m| m.get("model")).and_then(Value::as_str) {
                    self.model = Reading::observed(model.to_string(), stamp(obj));
                }
                if let Some(pv) = msg.and_then(|m| m.get("provider")).and_then(Value::as_str) {
                    self.provider = Reading::observed(pv.to_string(), stamp(obj));
                }
            }
            Some("model_change") => {
                if let Some(model) = obj.get("model").and_then(Value::as_str) {
                    self.model = Reading::observed(model.to_string(), stamp(obj));
                }
            }
            Some("thinking_level_change") => {
                if let Some(level) = obj.get("level").and_then(Value::as_str) {
                    self.effort = Reading::observed(level.to_string(), stamp(obj));
                }
            }
            _ => {}
        }
    }

    /// The footnote facts live on the record file's own rows: turn_context
    /// `data.model` per turn, model_request `data.provider_id`, header
    /// `data.fno_version`.
    fn observe_footnote(&mut self, obj: &Value) {
        let d = obj.get("data");
        match obj.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                if let Some(model) = d.and_then(|d| d.get("model")).and_then(Value::as_str) {
                    self.model = Reading::observed(model.to_string(), stamp(obj));
                }
            }
            Some("model_request") => {
                if let Some(pv) = d.and_then(|d| d.get("provider_id")).and_then(Value::as_str) {
                    self.provider = Reading::observed(pv.to_string(), stamp(obj));
                }
            }
            Some("header") => {
                if let Some(v) = d
                    .and_then(|d| d.get("fno_version"))
                    .and_then(fno_version_label)
                {
                    self.harness_version = Reading::observed(v, stamp(obj));
                }
            }
            _ => {}
        }
    }
}

/// A row's RFC3339 stamp as written, else `None`.
fn stamp(obj: &Value) -> Option<String> {
    crate::provenance::turn_stamp(obj).map(str::to_string)
}

/// The claude model label is what was asked for, not what answered: on a
/// routed endpoint `message.model` echoes the requested name. Anthropic's API
/// writes a `msg_01` message id and a `req_` request id. Measured over 150
/// transcripts, every other id shape came from a routed lane, including rows
/// labelled claude-opus-5-5 that a GLM route served. So a claude label on a
/// foreign shape reads unmeasured, and any other label there reads
/// `basis: label`, as a claim the row cannot confirm.
fn claude_model_reading(label: &str, obj: &Value) -> Reading {
    let id = obj
        .pointer("/message/id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let request = obj.get("requestId").and_then(Value::as_str).unwrap_or("");
    if id.starts_with("msg_01") && request.starts_with("req_") {
        return Reading::observed(label.to_string(), stamp(obj));
    }
    if label.starts_with("claude") {
        return Reading {
            value: None,
            measured_at: stamp(obj),
            basis: "unmeasured".to_string(),
            reason: Some(format!(
                "label {label} sits on a routed row shape, so the served model is unknown"
            )),
        };
    }
    Reading::with_basis(label.to_string(), stamp(obj), "label")
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

/// An epoch-millisecond column as RFC3339.
fn ms_stamp(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// The opencode facts live on the store itself: the session row's `model`
/// and `version` columns, plus the newest assistant message's `modelID`,
/// `providerID` and `variant` fields.
pub(crate) fn opencode_readings(db: &Path, session_id: &str) -> HarnessReadings {
    let mut out = HarnessReadings::empty("opencode");
    let Ok(conn) = crate::opencode_transcript::open_read_only(db) else {
        let reason = "store unreadable";
        out.model = Reading::unmeasured(reason);
        out.effort = Reading::unmeasured(reason);
        out.provider = Reading::unmeasured(reason);
        out.harness_version = Reading::unmeasured(reason);
        return out;
    };
    let session = conn.query_row(
        "SELECT model, version, time_updated FROM session WHERE id = ?1",
        [session_id],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    );
    if let Ok((model, version, updated)) = session {
        if let Some(model) = model.filter(|m| !m.is_empty()) {
            out.model = Reading::observed(model, ms_stamp(updated));
        }
        if let Some(version) = version.filter(|v| !v.is_empty()) {
            out.harness_version = Reading::observed(version, ms_stamp(updated));
        }
    }
    let Ok(mut stmt) = conn.prepare(
        "SELECT data, time_created FROM message WHERE session_id = ?1 AND data LIKE '%assistant%' ORDER BY time_created DESC LIMIT 20",
    ) else {
        return out;
    };
    let rows = stmt.query_map([session_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    });
    for (row, created) in rows.into_iter().flatten().flatten() {
        let Ok(obj) = serde_json::from_str::<Value>(&row) else {
            continue;
        };
        if obj.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let at = ms_stamp(created);
        if let Some(model) = obj.get("modelID").and_then(Value::as_str) {
            out.model = Reading::observed(model.to_string(), at.clone());
            if let Some(pv) = obj.get("providerID").and_then(Value::as_str) {
                out.provider = Reading::observed(pv.to_string(), at.clone());
            }
            if let Some(variant) = obj.get("variant").and_then(Value::as_str) {
                out.effort = Reading::observed(variant.to_string(), at);
            }
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(harness: &str, raw: &str) -> HarnessReadings {
        let mut r = HarnessReadings::empty(harness);
        for line in raw.lines() {
            r.observe(&serde_json::from_str(line).unwrap());
        }
        r
    }

    #[test]
    fn ac6_every_harness_serves_a_stamped_value_and_a_routed_label_reads_unmeasured() {
        let claude = read(
            "claude",
            concat!(
                r#"{"type":"assistant","timestamp":"2026-11-01T05:00:00Z","requestId":"req_1","message":{"id":"msg_01a","model":"claude-opus-5-5"}}"#,
                "\n",
                r#"{"type":"user","timestamp":"2026-11-01T05:00:10Z","effort":"high","message":{"role":"user","content":"x"}}"#,
            ),
        );
        assert_eq!(claude.model.value.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(
            claude.model.measured_at.as_deref(),
            Some("2026-11-01T05:00:00Z")
        );
        assert_eq!(claude.effort.value.as_deref(), Some("high"));
        assert_eq!(claude.provider.to_json()["basis"], json!("unmeasured"));
        assert!(claude.provider.to_json()["reason"].is_string());

        let codex = read(
            "codex",
            concat!(
                r#"{"timestamp":"2026-11-01T05:00:00Z","type":"session_meta","payload":{"model_provider":"openai","cli_version":"1.2.3"}}"#,
                "\n",
                r#"{"timestamp":"2026-11-01T05:00:05Z","type":"turn_context","payload":{"model":"gpt-6.1-sol","effort":"xhigh"}}"#,
            ),
        );
        assert_eq!(codex.model.value.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(codex.effort.value.as_deref(), Some("xhigh"));
        assert_eq!(codex.provider.value.as_deref(), Some("openai"));
        assert_eq!(codex.harness_version.value.as_deref(), Some("1.2.3"));

        let footnote = read(
            "footnote",
            concat!(
                r#"{"v":1,"ts":"2026-10-22T06:01:13.240Z","type":"header","data":{"fno_version":{"package":"0.4.3","git_rev":"abc"}}}"#,
                "\n",
                r#"{"ts":"2026-10-22T06:01:42Z","type":"turn_context","data":{"model":"glm-5.3-flash"}}"#,
                "\n",
                r#"{"ts":"2026-10-22T06:01:42.586Z","type":"model_request","data":{"provider_id":"zai"}}"#,
            ),
        );
        assert_eq!(footnote.model.value.as_deref(), Some("glm-5.3-flash"));
        assert_eq!(footnote.provider.value.as_deref(), Some("zai"));
        assert_eq!(footnote.effort.basis, "unmeasured");
        assert_eq!(footnote.harness_version.value.as_deref(), Some("abc"));

        let pi = read(
            "pi",
            concat!(
                r#"{"type":"message","message":{"role":"assistant","model":"pi-model","provider":"anthropic","timestamp":1730000000000}}"#,
                "\n",
                r#"{"type":"thinking_level_change","level":"high","timestamp":"2026-11-01T05:00:00Z"}"#,
            ),
        );
        assert_eq!(pi.model.value.as_deref(), Some("pi-model"));
        assert_eq!(pi.provider.value.as_deref(), Some("anthropic"));
        assert_eq!(pi.effort.value.as_deref(), Some("high"));

        let agy = HarnessReadings::empty("agy");
        for f in [&agy.model, &agy.effort, &agy.provider, &agy.harness_version] {
            assert_eq!(f.basis, "unmeasured");
            assert!(f.reason.is_some());
        }

        // Worker 29ccd06b's real shape: a GLM route echoing the claude name.
        let lied = read(
            "claude",
            r#"{"type":"assistant","timestamp":"2026-11-01T05:00:00Z","message":{"id":"msg_20261101","model":"claude-opus-5-5"}}"#,
        );
        assert_eq!(lied.model.value, None);
        assert_eq!(lied.model.basis, "unmeasured");
        assert!(lied.model.reason.as_deref().unwrap().contains("routed"));
        // Provider derives nothing from a model it cannot trust.
        let mut derived = lied.clone();
        derived.derive_provider_from_model();
        assert_eq!(derived.provider.basis, "unmeasured");

        // An honest route label stays, marked as an unconfirmed claim.
        let mut glm = read(
            "claude",
            r#"{"type":"assistant","timestamp":"2026-11-01T05:00:00Z","message":{"id":"msg_20261101","model":"glm-5.3-flash"}}"#,
        );
        assert_eq!(glm.model.value.as_deref(), Some("glm-5.3-flash"));
        assert_eq!(glm.model.basis, "label");
        glm.derive_provider_from_model();
        assert_eq!(glm.provider.value.as_deref(), Some("zai"));
        assert_eq!(glm.provider.basis, "derived");
    }
}
