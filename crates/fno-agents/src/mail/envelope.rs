//! The bus row: its fields, their wire order, the version rule and the thread
//! rule. `serde_json` keeps insertion order (`preserve_order`), so the field
//! order below is the byte order on the log.

use serde::Serialize;
use serde_json::{Map, Value};

/// The envelope version this build writes and reads. A row with no `v` reads
/// as 1. A row with a higher `v` is skipped by the reader with a warning.
pub const ENVELOPE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Envelope {
    pub v: u32,
    pub id: String,
    pub ts: String,
    /// The thread root: the parent's own id for a first message, else the
    /// root the parent already carries. One level only.
    pub thread: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_count: Option<i64>,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub meta: Map<String, Value>,
    pub body: String,
}

impl Envelope {
    /// A version-1 envelope with the required fields; every tag starts empty.
    pub fn new(
        id: &str,
        ts: &str,
        thread: &str,
        from: &str,
        to: &str,
        kind: &str,
        body: &str,
    ) -> Self {
        Self {
            v: ENVELOPE_VERSION,
            id: id.to_string(),
            ts: ts.to_string(),
            thread: thread.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            kind: kind.to_string(),
            from_harness: None,
            to_harness: None,
            in_reply_to: None,
            from_session: None,
            from_model: None,
            to_kind: None,
            origin: None,
            subject: None,
            word_count: None,
            meta: Map::new(),
            body: body.to_string(),
        }
    }

    /// The row as the log stores it.
    pub fn into_value(self) -> Value {
        serde_json::to_value(self).expect("an envelope always serializes")
    }
}

/// An optional tag as the log stores it: absent and empty both mean no tag.
pub fn tag(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(str::to_string)
}

/// The version a row declares. A row with no `v`, or a `v` that is not a
/// whole number, reads as 1.
pub fn row_version(row: &Value) -> u64 {
    row.get("v").and_then(Value::as_u64).unwrap_or(1)
}

/// Whether this build can read the row: its version is not newer than ours.
pub fn is_readable(row: &Value) -> bool {
    row_version(row) <= u64::from(ENVELOPE_VERSION)
}

/// The thread root for a reply: the parent's thread, else the parent's id. A
/// reply to a reply resolves to the same root as the reply it answers.
pub fn thread_root(parent_thread: Option<&str>, parent_id: &str) -> String {
    parent_thread
        .filter(|t| !t.is_empty())
        .unwrap_or(parent_id)
        .to_string()
}
