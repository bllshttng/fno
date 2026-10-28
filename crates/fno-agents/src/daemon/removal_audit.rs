//! The audit context every `agent.rm` carries, and the user's pin hold that
//! rides on its reason. Extracted from daemon.rs (shrink-only).

use crate::paths::AgentsHome;
use crate::protocol::Request;
use crate::state;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(super) struct RemovalAuditContext {
    pub(super) actor: String,
    pub(super) reason: String,
    pub(super) request_id: String,
    pub(super) worktree_touched: Option<bool>,
    pub(super) reclaimed_bytes: Option<u64>,
}

impl RemovalAuditContext {
    pub(super) fn from_request(req: &Request, entry: &state::RegistryEntry) -> Self {
        let string_param = |key: &str| {
            req.params
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        };
        let actor = string_param("audit_actor").unwrap_or_else(|| {
            [
                "FNO_HARNESS_SESSION_ID",
                "CLAUDE_CODE_SESSION_ID",
                "CODEX_THREAD_ID",
            ]
            .iter()
            .find_map(|key| std::env::var(key).ok())
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("session:{value}"))
            .unwrap_or_else(|| "operator".into())
        });
        let reason = string_param("audit_reason").unwrap_or_else(|| "operator-requested".into());
        let request_id = string_param("audit_request_id")
            .unwrap_or_else(|| format!("agent-rm:{}:{}", entry.name, entry.created_at));
        Self {
            actor,
            reason,
            request_id,
            worktree_touched: req
                .params
                .get("audit_worktree_touched")
                .and_then(Value::as_bool),
            reclaimed_bytes: req
                .params
                .get("audit_reclaimed_bytes")
                .and_then(Value::as_u64),
        }
    }

    /// The hold message when a machine caller meets the user's pin. A machine
    /// caller names its reason (the watchdog sandbox lane, the post-merge
    /// ritual), and the pin outranks every one of them, `--force` included.
    /// A person's own rm carries the default reason and still acts.
    pub(super) fn pin_hold(
        &self,
        home: &AgentsHome,
        entry: &state::RegistryEntry,
    ) -> Option<String> {
        if self.reason == "operator-requested" {
            return None;
        }
        let why = crate::keep_mark::for_row(home, entry)?;
        let name = &entry.name;
        Some(format!(
            "rm {name} held: {why}; the {} removal waits until the user unpins it \
             (fno agents reap --unkeep {name})",
            self.reason
        ))
    }
}
