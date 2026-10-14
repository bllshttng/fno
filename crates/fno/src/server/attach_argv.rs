//! The spawn argv of one attach gesture, extracted from the parent under the
//! file-budget gate. A claude row a live process argv already names attaches
//! directly; any other claude row asks the canonical re-entry plan.

use super::*;

/// Does the registry row record a route settings file? An unreadable
/// registry reads false: the sideline rows come from the same registry, and
/// the plan would refuse on it anyway.
fn row_records_route(name: &str) -> bool {
    let Ok(raw) = crate::agents_view::registry_text() else {
        return false;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    doc.get("agents")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|rows| {
            rows.iter().any(|r| {
                r.get("name").and_then(serde_json::Value::as_str) == Some(name)
                    && r.get("route_settings_path")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|p| !p.is_empty())
            })
        })
}

impl Core {
    /// The registry row NAME behind an attach id when that row is a
    /// claude row, from the live catalog (the sidepane's in-memory source).
    /// This is the identifier the resolver requests plans by; the attach id
    /// itself is transport-local and never a registry key. The resolver is
    /// claude-only, so a non-claude or harness-less row answers None and its
    /// attach keeps the legacy argv.
    pub(super) fn attach_claude_row(&self, attach_id: &str) -> Option<String> {
        self.agents
            .iter()
            .find(|a| a.attach_id.as_deref() == Some(attach_id))
            .filter(|a| a.harness.as_deref() == Some("claude"))
            .map(|a| a.name.clone())
    }

    /// The harness behind an attach id, from the live catalog. The
    /// argv builder needs it: each harness execs its own interface, and only
    /// the row knows which one this id belongs to.
    fn attach_row_harness(&self, attach_id: &str) -> Option<String> {
        self.agents
            .iter()
            .find(|a| a.attach_id.as_deref() == Some(attach_id))
            .and_then(|a| a.harness.clone())
    }

    /// The attach argv for a claude row whose session a live process argv
    /// names, with the row's config dir. The registry pid is stale after a
    /// resume, so the argv witness is the truth, and an attach starts no
    /// process: a click on such a row needs no re-entry plan and can never
    /// restart the session. `None` for any row the witness does not cover.
    pub(super) fn live_attach_argv(
        &self,
        attach_id: &str,
    ) -> Option<(Vec<String>, Option<std::path::PathBuf>)> {
        let row = self
            .agents
            .iter()
            .find(|a| a.attach_id.as_deref() == Some(attach_id))?;
        let (_, argv, _) = self.attach_replay_argv(attach_id, row)?;
        // A recorded route rides `--settings` on the canonical attach plan,
        // and only the plan builds it, so a routed row keeps the plan.
        if row_records_route(&row.name) {
            return None;
        }
        let (_, config_dir) = self.attach_account_ctx(attach_id);
        Some((argv, config_dir))
    }

    /// One attach gesture's spawn argv. `None` means the canonical
    /// resolution is now running off-loop and the caller must stop - the
    /// verdict re-enters this gesture through `ReentryPlanReady`. A replayed
    /// gesture holds the staged verdict (its argv IS the answer, config dir
    /// included). A claude row a live process names attaches without a plan.
    /// Every non-claude row keeps the legacy argv unchanged - that
    /// axis carries no route or account binding to preserve.
    pub(super) fn attach_gesture_argv(
        &mut self,
        client_id: u64,
        id: &str,
        placement: &crate::proto::PanePlacement,
    ) -> Option<(Vec<String>, Option<std::path::PathBuf>)> {
        if let Some(verdict) = self.reentry_verdict.take() {
            return Some((verdict.prefixed_argv(), verdict.config_dir));
        }
        if let Some(live) = self.live_attach_argv(id) {
            return Some(live);
        }
        if let Some(name) = self.attach_claude_row(id) {
            self.resolve_reentry(
                client_id,
                &name,
                "revive",
                ReentrySpawnRequest::Attach {
                    attach_id: id.to_string(),
                    placement: placement.clone(),
                },
            );
            return None;
        }
        let (acct, cd) = self.attach_account_ctx(id);
        let harness = self.attach_row_harness(id);
        Some((
            attach_argv_for(harness.as_deref(), id, acct.as_deref(), cd.as_deref()),
            cd,
        ))
    }
}
