//! Which rows exist in the mux UI, and what each row's paint
//! verdict is. The row-set derivation and its receipt live here, named by
//! the question they answer; the file they left is shrink-only under the
//! file-budget gate. The module is a child of `server`, so the private
//! helpers the walk reads (`compose_subline`, `truth_basis`, `portal_of`,
//! ...) stay where they are.

use crate::backlog_view::{BacklogJoin, GraphBinding};
use crate::proto::{AgentRow, AgentRowReceipt, SessionBinding};

use super::*;

/// One row's tool-activity ring: the server samples the registry row's
/// cumulative `(calls, errors)` pair on a 5s gate as fresh row sets arrive,
/// pushing each closed interval's delta. The client only draws.
#[derive(Default)]
pub(super) struct ActivityRing {
    /// The last sample's instant; `None` until the first due tick.
    last: Option<std::time::Instant>,
    /// The cumulative pair the last pushed cell covered. A not-yet-due tick
    /// does not touch it, so the next due sample counts the whole gap as
    /// one interval.
    pushed: (u64, u64),
    cells: std::collections::VecDeque<(u8, u8)>,
}

/// The ring cap and the sample gate: 8 intervals, at most one per 5s.
pub(super) const ACTIVITY_CELLS: usize = 8;
const ACTIVITY_EVERY: std::time::Duration = std::time::Duration::from_secs(5);
const ACTIVITY_IDLE: std::time::Duration = std::time::Duration::from_secs(600);

/// The row identity a ring rides: the claude transcript uuid where one
/// exists, else the harness session id, else the label - the same key the
/// tail pass and the truth probe join on.
/// The ring store, a static beside the fold idiom (`model_price::FOLDS`):
/// keyed by row identity, sampled through interior mutability so the read
/// path can sample without a `&mut Core`. Rows the batches stop naming
/// prune after ten idle minutes.
static RINGS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, ActivityRing>>> =
    std::sync::OnceLock::new();

pub(crate) fn rings() -> std::sync::MutexGuard<'static, HashMap<String, ActivityRing>> {
    RINGS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sample the served row set against the static: one cell per row per
/// [`ACTIVITY_EVERY`].
pub(crate) fn sample_activity(rows: &[RegistryAgent]) {
    sample_rings(&mut rings(), rows, std::time::Instant::now());
}

fn activity_key(a: &RegistryAgent) -> String {
    a.claude_session_uuid
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| a.harness_session_id.clone())
        .unwrap_or_else(|| a.name.clone())
}

/// The ring sampler, free so the gate math is testable without a `Core`:
/// at most one cell per row per [`ACTIVITY_EVERY`]; a row the batch stops
/// naming keeps its ring, and rings with no sample in ten minutes drop.
pub(crate) fn sample_rings(
    rings: &mut HashMap<String, ActivityRing>,
    rows: &[RegistryAgent],
    now: std::time::Instant,
) {
    rings.retain(|_, ring| {
        ring.last
            .is_none_or(|t| now.duration_since(t) < ACTIVITY_IDLE)
    });
    for a in rows {
        let Some((calls, errors)) = a.tool_counts else {
            continue;
        };
        let ring = rings.entry(activity_key(a)).or_default();
        if ring
            .last
            .is_some_and(|t| now.duration_since(t) < ACTIVITY_EVERY)
        {
            continue;
        }
        ring.last = Some(now);
        let cell = (
            calls.saturating_sub(ring.pushed.0).min(u8::MAX as u64) as u8,
            errors.saturating_sub(ring.pushed.1).min(u8::MAX as u64) as u8,
        );
        ring.pushed = (calls, errors);
        ring.cells.push_back(cell);
        while ring.cells.len() > ACTIVITY_CELLS {
            ring.cells.pop_front();
        }
    }
}

/// Whether two cwd paths are checkouts of the SAME project:
/// equal leaves, equal parents (sibling fno worktrees,
/// `<base>/<repo>/<name>`), or one path's parent carries the other's leaf
/// (canonical `<...>/footnote` beside worktree `<...>/footnote/wt-a`).
/// The leaf-vs-leaf compare read every worktree as foreign and stamped
/// `(footnote)` on cards (the operator's 2026-10-04 report). Two unparseable
/// paths (empty cwds) read same-project: neither can show a paren anyway.
pub(super) fn same_project(a: &str, b: &str) -> bool {
    fn leaf(p: &str) -> Option<&str> {
        let p = p.trim_end_matches('/');
        if p.is_empty() {
            None
        } else {
            p.rsplit('/').next()
        }
    }
    fn parent(p: &str) -> Option<&str> {
        let p = p.trim_end_matches('/');
        p.rsplit_once('/')
            .map(|(h, _)| h.trim_end_matches('/'))
            .filter(|h| !h.is_empty())
    }
    let (la, lb) = (leaf(a), leaf(b));
    la == lb && la.is_some()
        || parent(a).and_then(leaf) == lb && lb.is_some()
        || la == parent(b).and_then(leaf) && la.is_some()
        || parent(a) == parent(b) && parent(a).is_some()
}

/// The session-node bindings one registry row carries, strongest source
/// first: the graph's explicit `phase: bind` rows, the row's own spawn
/// node, its cwd's branch clue, the claim holder, a node-shaped name, then
/// the session's full graph history. Nodes dedupe first-wins, so the
/// strongest source to name a node answers for it. A name binds when it
/// parses to a served node or to a node the join carries a pr for (a bare
/// hex run matches by unique suffix) - the pr join outlived the
/// served-card filter and must not regress. History rows
/// bind unfiltered: a session's merged past is real even when no card for
/// it is served.
fn graph_bindings_for(
    join: &BacklogJoin,
    known: &HashMap<String, ()>,
    node_by_holder: &HashMap<String, String>,
    branch_by_cwd: &HashMap<String, String>,
    a: &RegistryAgent,
) -> Vec<GraphBinding> {
    let mut out: Vec<GraphBinding> = Vec::new();
    let mut admit = |b: GraphBinding| {
        if !out.iter().any(|bound| bound.node == b.node) {
            out.push(b);
        }
    };
    // Every accepted spelling of the row's session id; the graph keys
    // history rows by the FULL id.
    let sids = [
        a.session_id.as_deref(),
        a.harness_session_id.as_deref(),
        a.claude_session_uuid.as_deref().filter(|s| !s.is_empty()),
    ];
    for sid in sids.iter().flatten() {
        for b in join.by_session.get(*sid).into_iter().flatten() {
            if b.explicit {
                let mut bound = b.clone();
                bound.source = "explicit".into();
                admit(bound);
            }
        }
    }
    if let Some(node) = &a.node {
        admit(GraphBinding {
            node: node.clone(),
            pr: join.pr.get(node).copied(),
            state: "open".into(),
            source: "spawn".into(),
            ..Default::default()
        });
    }
    if let Some(branch) = branch_by_cwd.get(&a.cwd) {
        if let Some(node) = agents_view::resolve_node_id(branch, known) {
            let pr = join.pr.get(&node).copied();
            admit(GraphBinding {
                node,
                pr,
                state: "open".into(),
                source: "branch".into(),
                ..Default::default()
            });
        }
    }
    if let Some(node) = node_by_holder.get(&a.name) {
        admit(GraphBinding {
            node: node.clone(),
            pr: join.pr.get(node).copied(),
            state: "open".into(),
            source: "claim".into(),
            ..Default::default()
        });
    }
    if let Some(node) = agents_view::resolve_node_id(&a.name, known) {
        let pr = join.pr.get(&node).copied();
        admit(GraphBinding {
            node,
            pr,
            state: "open".into(),
            source: "name".into(),
            ..Default::default()
        });
    }
    for sid in sids.iter().flatten() {
        for b in join.by_session.get(*sid).into_iter().flatten() {
            if !b.explicit {
                let mut bound = b.clone();
                bound.source = "history".into();
                admit(bound);
            }
        }
    }
    out
}

/// The row's current binding: an explicit open bind wins outright (the
/// operator aimed the session), else live-session evidence (spawn, branch,
/// claim, name) outranks history, and ts orders only inside a tier;
/// equal timestamps keep the earlier listing. History rows are the only
/// ones carrying a ts, so a bare ts comparison would let a stale past
/// session outrank this row's live claim.
fn pick_current(bindings: &[GraphBinding]) -> Option<&GraphBinding> {
    let tier = |b: &GraphBinding| if b.source == "history" { 1u8 } else { 0 };
    let mut current: Option<&GraphBinding> = None;
    for b in bindings {
        if b.state != "open" {
            continue;
        }
        if b.explicit {
            return Some(b);
        }
        let newer = match current {
            None => true,
            Some(c) => {
                let (tb, tc) = (tier(b), tier(c));
                tb < tc || (tb == tc && b.ts.as_deref() > c.ts.as_deref())
            }
        };
        if newer {
            current = Some(b);
        }
    }
    current
}

/// The wire form (v111) of one row's bindings: `inferred` is false only
/// for an explicit bind and a claim - the two assignments a human made.
fn wire_bindings(bindings: Vec<GraphBinding>) -> Vec<SessionBinding> {
    bindings
        .into_iter()
        .map(|b| SessionBinding {
            inferred: !(b.explicit || b.source == "claim"),
            node: b.node,
            pr: b.pr,
            source: b.source,
            state: b.state,
        })
        .collect()
}

/// One lookup for every AgentRow constructor: the row's wire bindings plus
/// the current binding's node, pr, and driver attach handle, so the
/// precedence contract lives in exactly one place.
fn row_binding_view(
    join: &BacklogJoin,
    known: &HashMap<String, ()>,
    node_by_holder: &HashMap<String, String>,
    branch_by_cwd: &HashMap<String, String>,
    a: &RegistryAgent,
) -> (
    Vec<SessionBinding>,
    Option<String>,
    Option<u64>,
    Option<String>,
) {
    let bound = graph_bindings_for(join, known, node_by_holder, branch_by_cwd, a);
    let current = pick_current(&bound);
    let node = current.map(|b| b.node.clone());
    let pr = current.and_then(|b| b.pr);
    let attach = current.and_then(|b| join.driver.get(&b.node).cloned());
    (wire_bindings(bound), node, pr, attach)
}

impl Core {
    /// The served interval row for one agent: `None` when the row carries no
    /// counts (no readable transcript), an empty ring reads as waiting.
    pub(crate) fn activity_of(&self, a: &RegistryAgent) -> Option<Vec<(u8, u8)>> {
        rings()
            .get(&activity_key(a))
            .map(|r| r.cells.iter().copied().collect())
    }

    pub(crate) fn agent_rows(&self) -> Vec<AgentRow> {
        sample_activity(&self.agents);
        let mut out = Vec::new();
        // Which registry agents a pane row already claimed (so they don't
        // double-render as watch-only). Indexed like `self.agents`.
        let mut consumed = vec![false; self.agents.len()];
        // The session-node join inputs: the known node ids (served cards
        // plus the pr join's keys, so a bare-hex worker name still
        // suffix-resolves after its card leaves the served set) and the
        // live-claim reverse (holder -> node). `graph_bindings_for` merges
        // the graph's explicit rows, the spawn node, the cwd branch, the
        // claim, a node-shaped name, and the session history into one
        // binding list per row; the row takes its node, pr, and attach
        // handle from the current binding.
        let mut known: HashMap<String, ()> =
            self.backlog.iter().map(|c| (c.id.clone(), ())).collect();
        for id in self.backlog_join.pr.keys() {
            known.entry(id.clone()).or_insert(());
        }
        let node_by_holder: HashMap<String, String> = self
            .backlog_holders
            .iter()
            .map(|(node, holder)| (holder.clone(), node.clone()))
            .collect();
        // A squad-matched row's cwd_base is its FOREIGN-cwd
        // signal, so a row working in another checkout of its own squad's
        // project (a worktree beside the canonical, or a sibling worktree)
        // carries none - leaf-vs-leaf stamped `(footnote)` on every card.
        // Squad-less rows keep the plain basename for the elsewhere paren.
        let squad_cwd_of = |squad_id: Option<u64>| -> Option<String> {
            squad_id
                .and_then(|id| self.session.squads.iter().find(|s| s.id == id))
                .map(|s| s.canonical_cwd().to_string())
        };
        let foreign_cwd_base = |cwd: &str, squad_cwd: Option<&str>| -> Option<String> {
            match squad_cwd {
                Some(sc) if !sc.is_empty() && !same_project(cwd, sc) => cwd_basename(cwd),
                _ => cwd_basename(cwd),
            }
        };
        // 1. Pane rows: one per live tab leaf, deterministic (squad -> tab ->
        //    pane order). Iterating the tree (not `self.agents`) is what makes a
        //    bare shell pane a first-class row.
        for squad in &self.session.squads {
            for tab in &squad.tabs {
                for pid in tree::leaves(&tab.root) {
                    // The registry entry hosting this pane, if any: the join
                    // lives in agent_rows_join (extracted from this
                    // budget-capped file); its module doc carries the
                    // recycled-pane-id rationale and the total order.
                    let matched = agent_rows_join::bind_agent_to_pane(
                        &self.agents,
                        &self.session_name,
                        pid,
                        &self.attached,
                        &|a| self.worker_pane_for_agent(a),
                    );
                    // One lookup: liveness AND the bare-pane label read the same
                    // entry (a tree leaf reaped from `panes` is dying, so it
                    // forces `exited` - the fact-beats-report rule the old join
                    // used).
                    let pane_entry = self.panes.get(&pid);
                    let pane_dead = pane_entry.is_none();
                    // A portal seat is never a row of its own. A seat hosting
                    // a matched registry row renders as THAT row (the mark
                    // below rides it); a seat matching nothing - a held seat
                    // whose viewer died, an idle shell - is daemon inventory,
                    // not live work, so it mints no bare row.
                    if matched.is_none() && self.portal_of(Some(pid)).is_some() {
                        continue;
                    }
                    let row = match matched {
                        Some(i) => {
                            consumed[i] = true;
                            let a = &self.agents[i];
                            // A pane that only VIEWS the row (a portal seat, an
                            // Open Here viewer) sits in the viewer's workspace,
                            // but the row belongs to its own. Only a worker
                            // spawned into this tab takes the hosting squad.
                            let viewer = self.portal_of(Some(pid)).is_some()
                                || self.attached.values().any(|held| *held == pid);
                            let row_squad = if viewer {
                                self.thread_workspace(a)
                            } else {
                                Some(squad.id)
                            };
                            let exited = a.exited || pane_dead;
                            // A confirmed-gone pane is its own corroboration
                            // (fact beats badge) even when the registry row's
                            // own liveness read is Unmeasured -- only render
                            // the softer glyph when nothing here corroborates
                            // the exit at all.
                            let unmeasured = exited
                                && !pane_dead
                                && a.liveness == agents_view::Liveness::Unmeasured;
                            let (bindings, node, pr, pr_session_short) = row_binding_view(
                                &self.backlog_join,
                                &known,
                                &node_by_holder,
                                &self.branch_by_cwd,
                                a,
                            );
                            AgentRow {
                                harness: a.harness.clone(),
                                model: a.model.clone(),
                                route: a.route.clone(),
                                spawned_by_session: a.spawned_by_session.clone(),
                                lineage_kind: a.lineage_kind.clone(),
                                spawned_by_name: a.spawned_by_name.clone(),
                                lineage_reason: a.lineage_reason.clone(),
                                harness_session_id: a.harness_session_id.clone(),
                                squad: row_squad,
                                name: a.name.clone(),
                                pane_id: Some(pid),
                                // Derived every build from the open portals; the row
                                // stores no index of its own.
                                portal: self.row_portal_marker(a),
                                badge: if exited { None } else { a.badge },
                                reason: if exited { None } else { a.reason.clone() },
                                exited,
                                dnd: a.dnd,
                                held_conversation: a.held_conversation,
                                unmeasured,
                                liveness_measured_at: a.liveness_measured_at,
                                context_used_pct: a.context_used_pct,
                                context_tokens: a.context_tokens,
                                context_measured_at: a.context_measured_at,
                                started_at: a.started_at,
                                mail_unread: a.mail_unread,
                                node,
                                harness_title: a.harness_title.clone(),
                                answerable: if exited { None } else { a.answerable.clone() },
                                // A pane-hosted row focuses its pane; the attach
                                // target never rides it (wire contract).
                                attach_id: None,
                                external: a.external,
                                tab: Some(tab.id),
                                seen: self.seen.contains(&pid),
                                // (US3) cwd basename on every row so the
                                // sideline can flag a foreign-cwd join.
                                cwd_base: foreign_cwd_base(
                                    &a.cwd,
                                    squad_cwd_of(row_squad).as_deref(),
                                ),
                                project_root: self
                                    .project_by_cwd
                                    .get(&a.cwd)
                                    .map(|p| p.root.clone()),
                                tombstone: false,
                                subline: subline_with_title(a, self.compose_subline(&a.cwd)),
                                // Structural roster-dir tag wins (Locked
                                // Decision 6); else this pane's birth account.
                                account: a
                                    .account
                                    .clone()
                                    .or_else(|| pane_entry.and_then(|e| e.account.clone())),
                                updated_at: a.updated_at,
                                pr,
                                pr_session_short,
                                tail: self.compose_tail(a),
                                role_level: a.role_level,
                                role_scope: a.role_scope.clone(),
                                role_title: a.role_title.clone(),
                                basis: self.truth_basis(a),
                                last_activity_age_s: self.truth_age(a),
                                session_cost_cents: self.truth_cost(a).0,
                                session_tokens: self.truth_cost(a).1,
                                compaction_count: self
                                    .truth_reading(a)
                                    .and_then(|t| t.compaction_count),
                                activity: self.activity_of(a),
                                resumable: false,
                                no_pane_reason: None,
                                // A registry-hosted pane's badge is its primary
                                // signal, but the vt reading is still real and
                                // one field away: keep it so the client can
                                // show activity when the badge goes quiet.
                                pane_activity: pane_entry.map(|e| e.vt.shell_activity()),
                                // Decorative on a pane-hosted row (its
                                // reach focuses the pane); carried so the field
                                // never lies about the row's capability.
                                reach: agents_view::thread_reach(
                                    a.harness.as_deref(),
                                    a.attach_id.as_deref(),
                                ),
                                bindings,
                            }
                        }
                        None => {
                            // Bare pane: labelled from its own entry (node > cmd
                            // > cwd-basename > "shell"), matching the navigator's
                            // pane labels (v22) so the two agree.
                            let e = pane_entry;
                            AgentRow {
                                harness: None,
                                model: None,
                                route: None,
                                spawned_by_session: None,
                                lineage_kind: None,
                                spawned_by_name: None,
                                lineage_reason: None,
                                harness_session_id: None,
                                squad: Some(squad.id),
                                name: pane_label(
                                    e.and_then(|e| e.name.as_deref()),
                                    e.and_then(|e| e.node.as_deref()),
                                    e.map(|e| e.cwd.as_str()).unwrap_or(""),
                                    e.and_then(|e| e.cmd.as_deref()),
                                ),
                                pane_id: Some(pid),
                                // Derived every build from the open portals; the row
                                // stores no index of its own.
                                portal: self.portal_marker(Some(pid)),
                                badge: None,
                                reason: None,
                                exited: pane_dead
                                    || e.is_some_and(|entry| entry.refused_worker.is_some()),
                                dnd: false,
                                held_conversation: false,
                                unmeasured: false,
                                liveness_measured_at: None,
                                context_used_pct: None,
                                context_tokens: None,
                                context_measured_at: None,
                                session_cost_cents: None,
                                session_tokens: None,
                                compaction_count: None,
                                activity: None,
                                started_at: None,
                                mail_unread: None,
                                node: None,
                                harness_title: None,
                                answerable: None,
                                attach_id: None,
                                external: false,
                                tab: Some(tab.id),
                                seen: self.seen.contains(&pid),
                                cwd_base: foreign_cwd_base(
                                    e.map(|e| e.cwd.as_str()).unwrap_or(""),
                                    Some(squad.canonical_cwd()),
                                ),
                                tombstone: false,
                                subline: self
                                    .compose_subline(e.map(|e| e.cwd.as_str()).unwrap_or("")),
                                account: e.and_then(|e| e.account.clone()),
                                // A bare pane is not a registry worker: no
                                // claim, no pr, no transcript. But not-in-
                                // registry is not is-a-shell - it can be a
                                // full agent with a live workload, so the row
                                // carries the pane's OWN vt reading and the
                                // drain-path activity stamp.
                                pane_activity: e.map(|e| e.vt.shell_activity()),
                                last_activity_age_s: e.map(|e| e.last_output.elapsed().as_secs()),
                                updated_at: None,
                                pr: None,
                                pr_session_short: None,
                                tail: None,
                                // A bare shell pane has no registry entry, so
                                // no team and no reachability probe either.
                                role_level: None,
                                role_scope: None,
                                role_title: None,
                                basis: None,
                                resumable: false,
                                no_pane_reason: None,
                                reach: Reach::Locate,
                                bindings: Vec::new(),
                                project_root: None,
                            }
                        }
                    };
                    out.push(row);
                }
            }
        }

        // 2. Watch-only appendix: registry rows no pane claimed.
        for (i, a) in self.agents.iter().enumerate() {
            if consumed[i] {
                continue;
            }
            match &a.mux {
                Some((sess, pane)) => {
                    // A row hosted in ANOTHER session is that server's to render.
                    if sess != &self.session_name {
                        continue;
                    }
                    // A same-session mux row whose pane left the tree entirely
                    // (fully reaped) is a dangling exited row - preserve the old
                    // behaviour (`find_pane` -> None squad, `exited`).
                    // A same-session mux row whose pane left the
                    // tree is DEAD: its worker's pty is gone (a pane child of
                    // this server). Render it paneless rather than dangling -
                    // there is no pane to focus, and a paneless dead row can
                    // carry `resumable`, so tapping it resumes the session
                    // through the harness's own form instead of dead-ending
                    // on a focus at a pane that no longer exists.
                    let detached = self.detached_pane_for_agent(a);
                    let detached_live = detached.is_some_and(|pane| {
                        self.panes
                            .get(&pane)
                            .is_some_and(|entry| entry.pty.is_child_alive())
                    });
                    let resumable = !detached_live && self.row_resumable_in_session(a);
                    // Attribute the row to its workspace (recorded member,
                    // then the spawner's, then the project default), so the
                    // panel shows it where ResumeAgent will actually place
                    // the pane - the two lookups must agree.
                    let squad = self.thread_workspace(a);
                    let (bindings, node, pr, pr_session_short) = row_binding_view(
                        &self.backlog_join,
                        &known,
                        &node_by_holder,
                        &self.branch_by_cwd,
                        a,
                    );
                    out.push(AgentRow {
                        harness: a.harness.clone(),
                        model: a.model.clone(),
                        route: a.route.clone(),
                        spawned_by_session: a.spawned_by_session.clone(),
                        lineage_kind: a.lineage_kind.clone(),
                        spawned_by_name: a.spawned_by_name.clone(),
                        lineage_reason: a.lineage_reason.clone(),
                        harness_session_id: a.harness_session_id.clone(),
                        squad,
                        name: a.name.clone(),
                        pane_id: None,
                        // No pane, so no portal seat. Absent means NOT shown
                        // through a portal, never "unknown".
                        portal: None,
                        badge: detached_live.then_some(a.badge).flatten(),
                        reason: detached_live.then(|| a.reason.clone()).flatten(),
                        exited: if detached.is_some() {
                            !detached_live
                        } else {
                            true
                        },
                        dnd: a.dnd,
                        held_conversation: a.held_conversation,
                        unmeasured: false,
                        liveness_measured_at: None,
                        context_used_pct: a.context_used_pct,
                        context_tokens: a.context_tokens,
                        context_measured_at: a.context_measured_at,
                        started_at: a.started_at,
                        mail_unread: a.mail_unread,
                        node,
                        harness_title: a.harness_title.clone(),
                        answerable: None,
                        attach_id: None,
                        external: a.external,
                        tab: None,
                        seen: self.seen.contains(pane),
                        cwd_base: foreign_cwd_base(&a.cwd, squad_cwd_of(squad).as_deref()),
                        tombstone: false,
                        subline: subline_with_title(a, self.compose_subline(&a.cwd)),
                        account: a.account.clone(),
                        updated_at: a.updated_at,
                        pr,
                        pr_session_short,
                        tail: self.compose_tail(a),
                        role_level: a.role_level,
                        role_scope: a.role_scope.clone(),
                        role_title: a.role_title.clone(),
                        basis: self.truth_basis(a),
                        last_activity_age_s: self.truth_age(a),
                        session_cost_cents: self.truth_cost(a).0,
                        session_tokens: self.truth_cost(a).1,
                        compaction_count: self.truth_reading(a).and_then(|t| t.compaction_count),
                        activity: self.activity_of(a),
                        resumable,
                        no_pane_reason: if detached_live {
                            Some(AgentNoPaneReason::LivePaneless)
                        } else {
                            self.row_no_pane_reason_in_session(a)
                        },
                        // Dangling dead: the pane is gone, so no vt reading.
                        pane_activity: None,
                        reach: Reach::Locate,
                        bindings,
                        project_root: self.project_by_cwd.get(&a.cwd).map(|p| p.root.clone()),
                    })
                }
                None => {
                    // Truly paneless (bg/headless/daemon/roster). Its attach map
                    // pointed at no live pane (else a pane row claimed it), so it
                    // stays watch-only attachable - the AC1-FR revert.
                    // Never a mission squad: that id names a render-time header
                    // the client draws from names alone, so a row grouped under
                    // one is drawn by no section at all and disappears.
                    // Membership, then the spawner's workspace, then the
                    // project default - the one thread-workspace resolver.
                    let squad = self.thread_workspace(a);
                    // (US3) Every row carries its cwd basename: an orphan
                    // uses it for the `~ elsewhere` disambiguation suffix
                    // (AC2-UI), a squad-matched row for the foreign-cwd
                    // exception subline.
                    let cwd_base = foreign_cwd_base(&a.cwd, squad_cwd_of(squad).as_deref());
                    let (bindings, node, pr, pr_session_short) = row_binding_view(
                        &self.backlog_join,
                        &known,
                        &node_by_holder,
                        &self.branch_by_cwd,
                        a,
                    );
                    out.push(AgentRow {
                        harness: a.harness.clone(),
                        model: a.model.clone(),
                        route: a.route.clone(),
                        spawned_by_session: a.spawned_by_session.clone(),
                        lineage_kind: a.lineage_kind.clone(),
                        spawned_by_name: a.spawned_by_name.clone(),
                        lineage_reason: a.lineage_reason.clone(),
                        harness_session_id: a.harness_session_id.clone(),
                        squad,
                        name: a.name.clone(),
                        pane_id: None,
                        // No pane of its own, but a portal seat can still be
                        // SHOWING this row: the mark rides the row the seat's
                        // key answers (absent means not shown, never unknown).
                        portal: self.row_portal_marker(a),
                        badge: if a.exited { None } else { a.badge },
                        reason: if a.exited { None } else { a.reason.clone() },
                        exited: a.exited,
                        dnd: a.dnd,
                        held_conversation: a.held_conversation,
                        unmeasured: a.liveness == agents_view::Liveness::Unmeasured,
                        liveness_measured_at: a.liveness_measured_at,
                        context_used_pct: a.context_used_pct,
                        context_tokens: a.context_tokens,
                        context_measured_at: a.context_measured_at,
                        started_at: a.started_at,
                        mail_unread: a.mail_unread,
                        node,
                        harness_title: a.harness_title.clone(),
                        answerable: if a.exited { None } else { a.answerable.clone() },
                        attach_id: if a.exited { None } else { a.attach_id.clone() },
                        external: a.external,
                        tab: None,
                        // A watch-only row has no pane to focus, so it is always
                        // unseen.
                        seen: false,
                        cwd_base,
                        tombstone: false,
                        subline: subline_with_title(a, self.compose_subline(&a.cwd)),
                        // The structural roster-dir tag: an isolated-account
                        // foreign row carries its source account here (piece 3).
                        account: a.account.clone(),
                        updated_at: a.updated_at,
                        pr,
                        pr_session_short,
                        tail: self.compose_tail(a),
                        role_level: a.role_level,
                        role_scope: a.role_scope.clone(),
                        role_title: a.role_title.clone(),
                        basis: self.truth_basis(a),
                        last_activity_age_s: self.truth_age(a),
                        session_cost_cents: self.truth_cost(a).0,
                        session_tokens: self.truth_cost(a).1,
                        compaction_count: self.truth_reading(a).and_then(|t| t.compaction_count),
                        activity: self.activity_of(a),
                        resumable: self.row_resumable_in_session(a),
                        no_pane_reason: self.row_no_pane_reason_in_session(a),
                        // Watch-only paneless: no PTY, no vt reading.
                        pane_activity: None,
                        // The load-bearing site: a paneless live row's
                        // reach decides what its gesture opens. The attach_id
                        // half of the input re-reads the registry row (not the
                        // wire row's exited-gated copy) because the tier
                        // describes the SESSION's capability, while the wire's
                        // attach_id is cleared on exit for gate reasons.
                        reach: agents_view::thread_reach(
                            a.harness.as_deref(),
                            a.attach_id.as_deref(),
                        ),
                        bindings,
                        project_root: self.project_by_cwd.get(&a.cwd).map(|p| p.root.clone()),
                    })
                }
            }
        }
        // 3.  Tombstone members DECORATE the row their registry entry
        //    already produced; they no longer mint rows of their own. The old
        //    synthesized `cc-<attach_id>` row was the third row-set reader the
        //    two stores could disagree through: a member joining no registry
        //    row was evidence of nothing, yet it rendered a ghost under its
        //    (live) squad. The registry is the one row-set source now:
        //    a tombstoned member joining an EXITED row marks that row dimmed
        //    + dismissable (still the dimmed dismissable affordance, under the row's
        //    real name); a member joining a LIVE row never dims it (fact beats
        //    a stale tombstone - the liveness kill criterion); a member joining
        //    nothing renders nothing (a stale member; the member-retirement
        //    path removes it at restore).
        for (&sid, members) in &self.squad_members {
            if self.session.squad(sid).is_none() {
                continue;
            }
            for m in members.iter().filter(|m| m.tombstone) {
                if self.attached.contains_key(&m.attach_id) {
                    continue;
                }
                let joined = self.agents.iter().find(|a| {
                    crate::squad_store::member_joins_row(
                        m,
                        a.attach_id.as_deref(),
                        a.harness_session_id.as_deref(),
                    )
                });
                let Some(a) = joined else {
                    continue;
                };
                if !a.exited {
                    continue;
                }
                if let Some(row) = out.iter_mut().find(|r| {
                    // Match the produced row by the same session identity the
                    // join used, never by name alone: exited and live
                    // generations can share a display name, and dimming the
                    // live one is the liveness kill criterion.
                    r.name == a.name
                        && r.harness_session_id.as_deref() == a.harness_session_id.as_deref()
                }) {
                    row.tombstone = true;
                    // The dismiss affordance needs the attach target; exited
                    // rows clear it on the wire (attach-catalog gate), so the
                    // member's copy rides in here.
                    row.attach_id = Some(m.attach_id.clone());
                    // The dead member renders under the squad its membership
                    // persisted, not wherever the registry row's cwd points.
                    row.squad = Some(sid);
                }
            }
        }
        // 4. External-lifecycle tombstone rows: a persisted external
        //    record NOT currently live renders so `x` can act on it. The state
        //    maps onto the existing `exited` flag - stopped -> `exited` (rm);
        //    failed/unknown/stopping/removing -> `!exited` (stop / stop-retry),
        //    with the state as the row reason so an in-flight action is visible
        //    (AC1-UI). Deduped against live external rows (a still-live roster
        //    row wins; the record is stale until the next reconcile clears it).
        let live_ext: std::collections::HashSet<&str> = self
            .agents
            .iter()
            .filter(|a| a.external)
            .filter_map(|a| a.attach_id.as_deref())
            .collect();
        for r in &self.external_lifecycle {
            if live_ext.contains(r.attach_id.as_str()) {
                continue;
            }
            use crate::squad_store::ExternalState as S;
            let (exited, reason) = match r.state {
                S::Stopped => (true, None),
                S::Failed => (
                    false,
                    Some(r.reason.clone().unwrap_or_else(|| "stop failed".into())),
                ),
                S::Unknown => (false, Some("state unknown".to_string())),
                S::Stopping => (false, Some("stopping…".to_string())),
                S::Removing => (false, Some("removing…".to_string())),
            };
            let squad = self.session.find_by_cwd(&r.cwd);
            // (US3) Every row carries its cwd basename - including a
            // squad-matched external-lifecycle row, so its foreign-cwd subline
            // still renders (the "every row" wire contract; codex review).
            let cwd_base = cwd_basename(&r.cwd);
            out.push(AgentRow {
                // squads.json records no lane axes (ExternalLifecycle carries
                // none), so the row renders default-colored.
                harness: None,
                model: None,
                route: None,
                spawned_by_session: None,
                lineage_kind: None,
                spawned_by_name: None,
                lineage_reason: None,
                harness_session_id: None,
                squad,
                name: r.name.clone(),
                pane_id: None,
                portal: None,
                badge: None,
                reason,
                exited,
                dnd: false,
                held_conversation: false,
                unmeasured: false,
                liveness_measured_at: None,
                context_used_pct: None,
                context_tokens: None,
                context_measured_at: None,
                session_cost_cents: None,
                session_tokens: None,
                compaction_count: None,
                activity: None,
                started_at: None,
                mail_unread: None,
                node: None,
                harness_title: None,
                answerable: None,
                // Carried on an exited row so the client can send RemoveExternal;
                // on a live-ish row it is the StopExternal target. Either way the
                // attach-catalog gate (attach_id + !exited) never treats a stopped
                // tombstone as attachable.
                attach_id: Some(r.attach_id.clone()),
                external: true,
                tab: None,
                seen: false,
                cwd_base,
                tombstone: false,
                subline: self.compose_subline(&r.cwd),
                account: None,
                // An external row is never joined (respawn/pr/tail are
                // fno-registry concerns); its state lives in its own daemon, so
                // those cells stay EMPTY rather than inferred (AC4-ERR).
                updated_at: None,
                pr: None,
                pr_session_short: None,
                tail: None,
                // An external-daemon row is not an fno-registry worker: no
                // team, and its liveness lives in its own daemon, so no
                // reachability reading either.
                role_level: None,
                role_scope: None,
                role_title: None,
                basis: None,
                last_activity_age_s: None,
                resumable: false,
                no_pane_reason: None,
                // An external-daemon row owns no PTY of this server.
                pane_activity: None,
                // An external-lifecycle tombstone has no capability data here;
                // a live external row renders through the watch-only arm above.
                reach: Reach::Locate,
                bindings: Vec::new(),
                project_root: None,
            })
        }
        out
    }

    ///  The `fno mux rows` receipt: `agent_rows` trimmed to the
    /// row-set facts, each decorated with the server's paint verdict. The
    /// verdict names what the SERVER knows: a `no_pane_reason` text, the
    /// tombstone marker, or `None` (would paint; a non-paint on screen is a
    /// client-side fold, which is client state the server cannot see). The
    /// receipt distinguishes "present but suppressed" from "absent from the
    /// published set" - the reading the plan demanded before any render fix.
    /// The operator-facing text for a registry row's no-pane classification:
    /// one string per `AgentNoPaneReason`, so every door that names a refusal
    /// names it the same way.
    pub(crate) fn no_pane_reason_text(reason: AgentNoPaneReason) -> &'static str {
        match reason {
            AgentNoPaneReason::LivePaneless => "session is live elsewhere",
            AgentNoPaneReason::MissingHarness => "harness is missing",
            AgentNoPaneReason::MissingSessionId => "session id is missing",
            AgentNoPaneReason::UnsupportedHarness => "harness cannot resume sessions",
            AgentNoPaneReason::LivenessUnmeasured => "liveness was never measured",
        }
    }

    pub(crate) fn agent_rows_receipt(&self) -> Vec<AgentRowReceipt> {
        self.agent_rows()
            .into_iter()
            .map(|row| {
                let reason: Option<String> = row
                    .no_pane_reason
                    .map(|r| Self::no_pane_reason_text(r).to_string())
                    .or_else(|| {
                        row.tombstone
                            .then(|| "tombstoned member; dismissable".into())
                    })
                    // A resumable row is actionable, so it carries no note:
                    // "exited; renders dim" on a resumable row reads as a
                    // refusal the receipt does not make.
                    .or_else(|| {
                        (row.exited && !row.resumable).then(|| "exited; renders dim".into())
                    });
                AgentRowReceipt {
                    name: row.name.clone(),
                    harness: row.harness.clone(),
                    squad: row
                        .squad
                        .map(|sid| (sid, self.session.squad(sid).and_then(|sq| sq.name.clone()))),
                    pane: row.pane_id,
                    exited: row.exited,
                    tombstone: row.tombstone,
                    reason,
                    resumable: Some(row.resumable),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog_view::{BacklogJoin, GraphBinding};

    fn g(
        node: &str,
        pr: Option<u64>,
        state: &str,
        explicit: bool,
        ts: Option<&str>,
    ) -> GraphBinding {
        GraphBinding {
            node: node.into(),
            pr,
            phase: String::new(),
            ts: ts.map(Into::into),
            state: state.into(),
            explicit,
            source: String::new(),
        }
    }

    #[test]
    fn a_branch_clue_binds_its_node_and_pr() {
        // AC1-HP: no node-shaped name, a feature branch naming the node,
        // and the node's pr known to the board - one branch binding with
        // the pr riding through join.pr.
        let mut join = BacklogJoin::default();
        join.pr.insert("x-2222".into(), 3159);
        let ids: HashMap<String, ()> = ["x-2222"]
            .into_iter()
            .map(|i| (i.to_string(), ()))
            .collect();
        let a = RegistryAgent {
            name: "deploy bot".into(),
            cwd: "/tmp/repo".into(),
            ..Default::default()
        };
        let branches: HashMap<String, String> =
            [("/tmp/repo".to_string(), "feature/x-2222-foo".to_string())].into();
        let got = graph_bindings_for(&join, &ids, &HashMap::new(), &branches, &a);
        assert_eq!(got.len(), 1, "one branch clue, one binding");
        assert_eq!(got[0].node, "x-2222");
        assert_eq!(got[0].pr, Some(3159), "pr rides through join.pr");
        assert_eq!(got[0].source, "branch");
        assert_eq!(got[0].state, "open");
        assert!(!got[0].explicit);
    }

    #[test]
    fn current_picks_explicit_then_live_then_newest_history() {
        // AC2-HP: one session's history spans a merged PR, another merged
        // PR, and the open node - all three bind, and the only open one is
        // the current. AC3-EDGE: an explicit bind wins outright; a live
        // claim beats a ts-carrying history row; two inferred opens pick
        // the newest.
        let mut join = BacklogJoin::default();
        join.by_session.insert(
            "s1".into(),
            vec![
                g(
                    "x-aaaa",
                    Some(1),
                    "merged",
                    false,
                    Some("2026-01-01T00:00:00Z"),
                ),
                g(
                    "x-bbbb",
                    Some(2),
                    "merged",
                    false,
                    Some("2026-01-02T00:00:00Z"),
                ),
                g(
                    "x-cccc",
                    Some(3),
                    "open",
                    false,
                    Some("2026-01-03T00:00:00Z"),
                ),
            ],
        );
        let ids: HashMap<String, ()> = ["x-cccc"]
            .into_iter()
            .map(|i| (i.to_string(), ()))
            .collect();
        let a = RegistryAgent {
            name: "worker".into(),
            harness_session_id: Some("s1".into()),
            ..Default::default()
        };
        let got = graph_bindings_for(&join, &ids, &HashMap::new(), &HashMap::new(), &a);
        assert_eq!(got.len(), 3, "every history row binds");
        assert_eq!(got[0].source, "history");
        assert_eq!(got[0].pr, Some(1));
        assert_eq!(got[0].state, "merged");
        assert_eq!(pick_current(&got).map(|b| b.node.as_str()), Some("x-cccc"));

        let mut join = BacklogJoin::default();
        join.by_session.insert(
            "s1".into(),
            vec![
                g("x-1", Some(9), "open", true, Some("2026-01-01T00:00:00Z")),
                g("x-2", Some(10), "open", false, Some("2026-01-02T00:00:00Z")),
            ],
        );
        let a = RegistryAgent {
            name: "worker".into(),
            harness_session_id: Some("s1".into()),
            ..Default::default()
        };
        let got = graph_bindings_for(&join, &ids_empty(), &HashMap::new(), &HashMap::new(), &a);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].node, "x-1", "the explicit row lists first");
        assert_eq!(got[0].source, "explicit");
        assert!(pick_current(&got).map(|b| b.node.as_str()) == Some("x-1"));
        // Without the explicit row, the newer ts wins.
        let mut join2 = BacklogJoin::default();
        join2.by_session.insert(
            "s1".into(),
            vec![
                g("x-1", Some(9), "open", false, Some("2026-01-01T00:00:00Z")),
                g("x-2", Some(10), "open", false, Some("2026-01-02T00:00:00Z")),
            ],
        );
        let got2 = graph_bindings_for(&join2, &ids_empty(), &HashMap::new(), &HashMap::new(), &a);
        assert_eq!(
            pick_current(&got2).map(|b| b.node.as_str()),
            Some("x-2"),
            "two inferred opens pick the last-touched node"
        );
        // A live claim (no ts) beats an open history row (any ts): the
        // claim is this session's present, the history row its past.
        let holders: HashMap<String, String> = [("worker".to_string(), "x-1".to_string())].into();
        let got3 = graph_bindings_for(&join2, &ids_empty(), &holders, &HashMap::new(), &a);
        assert_eq!(
            got3.iter()
                .find(|b| b.source == "claim")
                .map(|b| b.node.as_str()),
            Some("x-1"),
            "the claim binding is admitted ts-less"
        );
        assert_eq!(
            pick_current(&got3).map(|b| b.node.as_str()),
            Some("x-1"),
            "live claim outranks the ts-carrying history row"
        );
    }

    #[test]
    fn overlapping_sources_dedup_to_the_strongest() {
        // spawn beats branch beats claim for the same node; the loser
        // rows never duplicate the node.
        let mut join = BacklogJoin::default();
        join.pr.insert("x-1".into(), 100);
        let ids: HashMap<String, ()> = ["x-1"].into_iter().map(|i| (i.to_string(), ())).collect();
        let holders: HashMap<String, String> = [("deploy".to_string(), "x-1".to_string())].into();
        let branches: HashMap<String, String> =
            [("/tmp/repo".to_string(), "feature/x-1-feat".to_string())].into();
        let a = RegistryAgent {
            name: "deploy".into(),
            cwd: "/tmp/repo".into(),
            node: Some("x-1".into()),
            ..Default::default()
        };
        let got = graph_bindings_for(&join, &ids, &holders, &branches, &a);
        assert_eq!(got.len(), 1, "one node, one binding");
        assert_eq!(got[0].source, "spawn");
        assert_eq!(got[0].pr, Some(100));
        // Claim alone still binds, and inferred is false for it.
        let a2 = RegistryAgent {
            name: "deploy".into(),
            ..Default::default()
        };
        let got2 = graph_bindings_for(&join, &ids, &holders, &HashMap::new(), &a2);
        assert_eq!(got2.len(), 1);
        assert_eq!(got2[0].source, "claim");
    }

    #[test]
    fn an_empty_join_keeps_the_name_fallback() {
        // AC4-EDGE: a malformed/empty join binds nothing by itself, and
        // today's name + claim fallbacks still resolve node and leave pr
        // unset.
        let join = BacklogJoin::default();
        let ids: HashMap<String, ()> = ["x-2222"]
            .into_iter()
            .map(|i| (i.to_string(), ()))
            .collect();
        let holders: HashMap<String, String> = [("w1".to_string(), "x-2222".to_string())].into();
        let claim_agent = RegistryAgent {
            name: "w1".into(),
            ..Default::default()
        };
        let got = graph_bindings_for(&join, &ids, &holders, &HashMap::new(), &claim_agent);
        assert_eq!(got.len(), 1, "the claim fallback survives an empty join");
        assert_eq!(got[0].source, "claim");
        assert_eq!(got[0].node, "x-2222");
        assert_eq!(got[0].pr, None);
        let name_agent = RegistryAgent {
            name: "x-2222-runner".into(),
            ..Default::default()
        };
        let got2 = graph_bindings_for(&join, &ids, &HashMap::new(), &HashMap::new(), &name_agent);
        assert_eq!(got2.len(), 1, "the name fallback survives an empty join");
        assert_eq!(got2[0].source, "name");
        assert_eq!(got2[0].node, "x-2222");
    }

    fn ids_empty() -> HashMap<String, ()> {
        HashMap::new()
    }
}
