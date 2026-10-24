//! `fno backlog retro`: file the work a merged PR left out as backlog nodes.
//!
//! Pipeline: harvest -> classify -> dedup -> land. Three verbs share it:
//! `run` consumes the retro-pending sentinels (plus the plain-run postmortem
//! drain and gate-escape summary), `drain-postmortems` runs only the
//! postmortem pass, and `sweep-carveouts` reads the whole carve-out ledger
//! with no PR at all. Every filed node carries its source's reasoning
//! verbatim plus a cite; nothing is invented.

pub mod classify;
pub mod cli;
pub mod dedup;
pub mod difflib;
pub mod gate_escape;
pub mod harvest;
pub mod keep_going;
pub mod land;
pub mod ledger;
pub mod postmortem;
pub mod reconcile_findings;
pub mod routine;
pub mod sweep;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use cli::run;

pub const KIND_CARVEOUT: &str = "carveout";
pub const KIND_REVIEW: &str = "review";
pub const KIND_DEFERRED: &str = "deferred_finding";
pub const KIND_POSTMORTEM: &str = "postmortem";

pub const TIER_NODE: &str = "node";
pub const TIER_INBOX: &str = "inbox";

/// One harvested left-out signal, before classification.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RawItem {
    pub kind: String,
    pub text: String,
    pub source_pr: Option<i64>,
    pub source_id: String,
    pub severity: Option<String>,
    pub priority: Option<String>,
    pub url: Option<String>,
    pub reviewer: Option<String>,
    pub title_hint: Option<String>,
    pub subkind: Option<String>,
}

/// A classified, dedup-ready node or inbox proposal.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub title: String,
    pub body: String,
    pub tier: String,
    pub priority: String,
    pub source_pr: Option<i64>,
    pub source_id: String,
    pub content_hash: String,
    pub uncited: bool,
    /// The raw finding text the hash covers, not the cite, so one issue
    /// flagged by two reviewers collapses to one node.
    pub finding_text: String,
    pub extra: Map<String, Value>,
}

impl Candidate {
    /// A string field of `extra`, `None` when absent or not a string.
    pub fn extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(Value::as_str)
    }
}

/// Python's `str(x)` for the JSON scalars the retro readers stringify.
pub(crate) fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

/// Python truthiness of a JSON value.
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(node.get(key) or "")`: a falsy value reads as empty.
pub(crate) fn str_or_empty(node: &Value, key: &str) -> String {
    let v = node.get(key);
    if truthy(v) {
        py_str(v)
    } else {
        String::new()
    }
}

/// Python `s[:n]` on characters, not bytes.
pub(crate) fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub(crate) fn char_len(s: &str) -> usize {
    s.chars().count()
}
