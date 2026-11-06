//! One cheap read of what a session is: its role kind and the lead whose
//! team it sits under. The rule filters (`role`, `lead`) match against this
//! and never walk the graph.
//!
//! Nothing new is stored. Every fact already sits on a registry row:
//! `role_scope` marks a lead, the command that opens `spawn_payload` marks a
//! blueprint or reviewer, and `spawned_by_session` names the parent whose own
//! `role_scope` is the lead. The lead edge is one hop. A worker a lead did not
//! spawn directly, or whose lead has exited, reads `lead: None`; the answer is
//! absent, never guessed from a territory walk.

use serde_json::Value;

use crate::whoami::find_by_session;
use crate::AgentStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    Lead,
    Blueprint,
    Reviewer,
    Worker,
}

impl RoleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleKind::Lead => "lead",
            RoleKind::Blueprint => "blueprint",
            RoleKind::Reviewer => "reviewer",
            RoleKind::Worker => "worker",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRole {
    pub kind: RoleKind,
    /// The registry name: the value a "one named session" filter matches.
    pub name: String,
    /// Canonical scope of the covering lead team. A lead names its own.
    pub lead: Option<String>,
}

fn str_field<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn is_liveish(row: &Value) -> bool {
    row.get("status")
        .and_then(|v| serde_json::from_value::<AgentStatus>(v.clone()).ok())
        .is_some_and(|s| crate::spawn_gate::status_is_liveish(&s))
}

fn own_scope(row: &Value) -> Option<String> {
    let scope = crate::territory::canonical_scope(str_field(row, "role_scope")?);
    (!scope.is_empty()).then_some(scope)
}

fn seed_kind(row: &Value) -> RoleKind {
    let command = str_field(row, "spawn_payload")
        .and_then(|p| p.split_whitespace().next())
        .and_then(crate::provider::parse_command_token)
        .map(|(c, _)| c);
    match command {
        Some("blueprint") => RoleKind::Blueprint,
        Some("review") => RoleKind::Reviewer,
        _ => RoleKind::Worker,
    }
}

fn parent_row<'a>(rows: &'a [Value], row: &Value) -> Option<&'a Value> {
    let parent = str_field(row, "spawned_by_session")?;
    match str_field(row, "spawned_by_harness") {
        Some(harness) => find_by_session(rows, parent, harness),
        None => rows
            .iter()
            .find(|r| str_field(r, "harness_session_id") == Some(parent)),
    }
}

/// The role of the session `session_id` runs under `harness`, or `None` when
/// the registry holds no row for it.
pub fn session_role(rows: &[Value], session_id: &str, harness: &str) -> Option<SessionRole> {
    let row = find_by_session(rows, session_id, harness)?;
    let name = str_field(row, "name").unwrap_or_default().to_string();
    if let Some(scope) = own_scope(row) {
        return Some(SessionRole {
            kind: RoleKind::Lead,
            name,
            lead: Some(scope),
        });
    }
    let lead = parent_row(rows, row)
        .filter(|p| is_liveish(p))
        .and_then(own_scope);
    Some(SessionRole {
        kind: seed_kind(row),
        name,
        lead,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fleet() -> Vec<Value> {
        vec![
            json!({"name": "lead-one", "harness": "claude", "harness_session_id": "s-lead",
                   "status": "busy", "role_level": 1, "role_scope": "b,a"}),
            json!({"name": "wk", "harness": "claude", "harness_session_id": "s-wk",
                   "status": "busy", "spawn_payload": "/fno:target x-1",
                   "spawned_by_session": "s-lead", "spawned_by_harness": "claude"}),
            json!({"name": "bp", "harness": "codex", "harness_session_id": "s-bp",
                   "status": "busy", "spawn_payload": "$fno:blueprint x-2",
                   "spawned_by_session": "s-lead"}),
            json!({"name": "rv", "harness": "claude", "harness_session_id": "s-rv",
                   "status": "busy", "spawn_payload": "/fno:review high",
                   "spawned_by_session": "s-gone", "spawned_by_harness": "claude"}),
            json!({"name": "gone", "harness": "claude", "harness_session_id": "s-gone",
                   "status": "exited", "role_scope": "z"}),
        ]
    }

    #[test]
    fn lead_names_its_own_canonical_scope() {
        let r = session_role(&fleet(), "s-lead", "claude").unwrap();
        assert_eq!(r.kind, RoleKind::Lead);
        assert_eq!(r.lead.as_deref(), Some("a,b"));
        assert_eq!(r.name, "lead-one");
    }

    #[test]
    fn worker_takes_the_lead_of_its_live_parent() {
        let r = session_role(&fleet(), "s-wk", "claude").unwrap();
        assert_eq!((r.kind, r.lead.as_deref()), (RoleKind::Worker, Some("a,b")));
    }
}
