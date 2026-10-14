//! The registry reader's hand-off to the core: the per-cwd project map and the
//! `CoreMsg::AgentRows` apply step.

use super::*;

/// What a row's cwd says about its project: the canonical repo root, and the
/// id its `.fno/config.toml` declares under `[project]` when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectRef {
    pub(super) root: String,
    pub(super) id: Option<String>,
}

/// Resolve one cwd's project. Runs git and reads one file, so callers keep it
/// off the core loop (the reader's blocking pool).
pub(super) fn resolve_project(cwd: &str) -> ProjectRef {
    let root = squad::resolve_key(cwd);
    let id = std::fs::read_to_string(Path::new(&root).join(".fno/config.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| {
            table
                .get("project")?
                .get("id")?
                .as_str()
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
        });
    ProjectRef { root, id }
}

impl Core {
    /// The `CoreMsg::AgentRows` arm body: adopt the fresh row set, repaint the
    /// sideline, and drain what the new registry read unblocks.
    pub(super) fn apply_agent_rows(
        &mut self,
        rows: Vec<RegistryAgent>,
        branches: HashMap<String, String>,
        tails: HashMap<String, String>,
        ctx: HashMap<String, String>,
        read_ok: bool,
    ) -> Flow {
        self.agents_read_ok = read_ok;
        let identity_published = self.worker_identity_published(&rows);
        // Discoverability, once per server lifetime: the
        // first paneless live row to appear with no portal open names
        // the gesture. "Explicit and lazy" must not mean
        // "undiscoverable". A notice is not state - the latch only
        // mutes repetition, it never gates behavior. The
        // notice names both doors, because opening a SECOND portal is
        // the gesture an operator cannot guess from the first.
        if !self.portal_noticed
            && self.portals.is_empty()
            && rows.iter().any(|a| a.mux.is_none() && !a.exited)
        {
            // Latch on DELIVERY, not on the attempt: workers
            // register before an operator attaches in the ordinary
            // daemon startup, so a latch set with no client attached
            // would burn the once-per-lifetime notice on nobody.
            if self.notice_all(
                "thread row present: reach a row (Enter or click) to open portal 0; P opens another portal beside it",
            ) {
                self.portal_noticed = true;
            }
        }
        self.agents = rows;
        self.branch_by_cwd = branches;
        self.tail_by_session = tails;
        self.ctx_by_session = ctx;
        // Row changes are the journal's change signal: a
        // spawn or removal writes both. Refresh the cached scan here,
        // off the per-push paths that read it.
        self.journal.refresh();
        if identity_published {
            // A registry row can publish after a worker pane was
            // recorded. Force the existing debounce funnel to flush
            // the newly authoritative identity instead of waiting for
            // another topology mutation.
            self.mark_topology_dirty();
        }
        // The debounce flush for topology captures: a drag's
        // events mark dirty without writing; the 1s registry tick is
        // the coalescing timer that turns them into one store write.
        self.bind_named_folders();
        self.flush_topology();
        // The registry just refreshed: a queued template restore
        // whose fno bindings now resolve gets applied here, binding live
        // sessions instead of shells.
        self.drain_template_restores();
        // Rects are unchanged; only the Layout's agent rows moved -
        // push without re-emitting frames (AC1-UI: visible within one
        // layout push; AC2-UI: the read happened off-loop).
        self.push_layout(false);
        Flow::Continue
    }
}
