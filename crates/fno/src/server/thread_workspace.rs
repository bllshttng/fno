//! Which workspace a thread belongs to.
//!
//! One resolver, six rungs, in order: the operator's pin (a move made by
//! hand, keyed by harness session id), the recorded member, the spawner's
//! workspace (the row's `spawned_by_session` edge followed to the parent
//! registry row), then the project default in three steps: the workspace
//! whose origin owns the row's raw cwd, the one whose origin owns the cwd's
//! canonical repo root (so a linked worktree outside the origin still
//! lands), and the one named workspace whose name is the project id or the
//! root's folder name. A thread's cwd is its own property and decides
//! membership only on the last three rungs, so a thread spawned into another
//! repo still shows under the workspace it was spawned from. No rung reads
//! focus, a portal seat or an Open Here viewer, so a tap never moves a row.

use super::*;

/// How many spawner hops the lineage walk tolerates before it gives up and
/// lets the project default answer. Bounds a corrupted edge cycle.
const HOP_CAP: u32 = 8;

impl Core {
    /// The workspace of a thread row, asked by every thread-row site
    /// (sideline attribution, portal and attach owner routing, reseat graft,
    /// resume placement) so the row renders where the next gesture places it.
    pub(crate) fn thread_workspace(&self, a: &RegistryAgent) -> Option<u64> {
        self.workspace_at(a, 0)
    }

    fn workspace_at(&self, a: &RegistryAgent, depth: u32) -> Option<u64> {
        self.pinned_squad(a)
            .or_else(|| self.member_squad_for_agent(a))
            .or_else(|| self.spawner_workspace(a, depth))
            .or_else(|| self.session.find_by_cwd(&a.cwd))
            .or_else(|| self.project_squad(a))
    }

    /// The pin rung: the workspace the operator moved this session into. The
    /// pin names the workspace by identity (name, else durable key), so a
    /// pin whose workspace is not open reads as absent.
    fn pinned_squad(&self, a: &RegistryAgent) -> Option<u64> {
        let harness = a.harness.as_deref()?;
        let session_id = agent_harness_session_id(a)?;
        let (identity, _) = self
            .thread_pins
            .iter()
            .find(|(_, pin)| pin.harness == harness && pin.harness_session_id == session_id)?;
        self.session
            .squads
            .iter()
            .find(|s| match s.name.as_deref() {
                Some(name) if !name.is_empty() => name == identity,
                _ => s.key == *identity,
            })
            .map(|s| s.id)
    }

    /// The project rungs: the workspace whose origin owns the cwd's canonical
    /// repo root, else the one named workspace the project id (or, with no id,
    /// the root's folder name) names. Two named workspaces claiming one name
    /// read as absent, the same "never a confident wrong answer" rule as the
    /// spawner rung.
    fn project_squad(&self, a: &RegistryAgent) -> Option<u64> {
        let project = self.project_by_cwd.get(&a.cwd)?;
        if let Some(sid) = self.session.find_by_cwd(&project.root) {
            return Some(sid);
        }
        let named = |want: &str| -> Vec<u64> {
            self.session
                .squads
                .iter()
                .filter(|s| s.name.as_deref() == Some(want))
                .map(|s| s.id)
                .collect()
        };
        let mut found = project.id.as_deref().map(named).unwrap_or_default();
        if found.is_empty() {
            let base = project.root.trim_end_matches('/').rsplit('/').next();
            found = base
                .filter(|b| !b.is_empty())
                .map(named)
                .unwrap_or_default();
        }
        (found.len() == 1).then(|| found[0])
    }

    /// The spawner rung: the row's `spawned_by_session` edge joined to the
    /// parent registry row(s) by harness session id (trimmed, case-insensitive
    /// - the same tolerance `spawned_by_name` applies). The session id is the
    /// join key, so a parent sharing the child's display name still answers;
    /// a corrupted self-edge just walks to the hop cap and falls to cwd. A
    /// parent pane-hosted on this server answers where its pane lives NOW
    /// (`find_pane`), so the row lands where ResumeAgent would put it; any
    /// other parent answers with its own full chain one hop deeper. Parents
    /// resolving to DIFFERENT workspaces read as absent: an ambiguous edge is
    /// never a confident wrong answer (the `spawned_by_name` rule).
    fn spawner_workspace(&self, a: &RegistryAgent, depth: u32) -> Option<u64> {
        if depth >= HOP_CAP {
            return None;
        }
        let edge = a.spawned_by_session.as_deref().map(str::trim).unwrap_or("");
        if edge.is_empty() {
            return None;
        }
        let mut squads = self
            .agents
            .iter()
            .filter(|p| {
                agent_harness_session_id(p).is_some_and(|sid| sid.trim().eq_ignore_ascii_case(edge))
            })
            .filter_map(|p| match &p.mux {
                // Only a LIVE parent owns its pane id: ids recycle after the
                // pane dies (the bind_agent_to_pane rule), so an exited row's
                // id can name an unrelated pane in another workspace. A dead
                // parent answers through its own chain instead.
                // A parent seated in a portal, or shown in an Open Here
                // viewer, lives in the VIEWER's workspace: it hands its
                // children its own answer instead.
                Some((sess, pane))
                    if sess == &self.session_name
                        && !p.exited
                        && self.portal_of(Some(*pane)).is_none()
                        && !self.attached.values().any(|held| held == pane) =>
                {
                    self.session
                        .find_pane(*pane)
                        .map(|(sid, _)| sid)
                        .or_else(|| self.workspace_at(p, depth + 1))
                }
                _ => self.workspace_at(p, depth + 1),
            });
        let first = squads.next()?;
        if squads.any(|s| s != first) {
            return None;
        }
        Some(first)
    }

    fn member_squad_for_agent(&self, agent: &RegistryAgent) -> Option<u64> {
        let exact: Vec<u64> = match (agent.harness.as_deref(), agent_harness_session_id(agent)) {
            (Some(harness), Some(session_id)) => self
                .squad_members
                .iter()
                .filter_map(|(sid, members)| {
                    members
                        .iter()
                        .any(|member| {
                            // Ids only: a rename or a resume must not lose
                            // the workspace the member was placed in.
                            member.harness.as_deref() == Some(harness)
                                && member.harness_session_id.as_deref().is_some_and(|m| {
                                    m == session_id
                                        || agent.predecessor_session_ids.iter().any(|p| p == m)
                                })
                        })
                        .then_some(*sid)
                })
                .collect(),
            _ => Vec::new(),
        };
        if exact.len() == 1 {
            return exact.first().copied();
        }
        if exact.len() > 1 {
            return None;
        }
        // An AttachAgent persist stores only the attach handle (no worker
        // name), so the name rungs never match it; match the row's own
        // handle, so an explicit placement survives the pane closing.
        let attached: Vec<u64> = match agent.attach_id.as_deref().filter(|id| !id.is_empty()) {
            Some(id) => self
                .squad_members
                .iter()
                .filter_map(|(sid, members)| {
                    members
                        .iter()
                        .any(|member| member.attach_id == id)
                        .then_some(*sid)
                })
                .collect(),
            None => Vec::new(),
        };
        if attached.len() == 1 {
            return attached.first().copied();
        }
        if attached.len() > 1 {
            return None;
        }
        let legacy: Vec<u64> = self
            .squad_members
            .iter()
            .filter_map(|(sid, members)| {
                members
                    .iter()
                    .any(|member| {
                        member.worker.as_deref() == Some(agent.name.as_str())
                            && member.harness.is_none()
                            && member.harness_session_id.is_none()
                    })
                    .then_some(*sid)
            })
            .collect();
        (legacy.len() == 1).then(|| legacy[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tests::empty_core;
    use crate::squad_store::{StoredMember, StoredThreadPin};

    fn row(
        name: &str,
        cwd: &str,
        harness: Option<&str>,
        sid: Option<&str>,
        spawned_by: Option<&str>,
    ) -> RegistryAgent {
        RegistryAgent {
            name: name.into(),
            cwd: cwd.into(),
            harness: harness.map(Into::into),
            harness_session_id: sid.map(Into::into),
            spawned_by_session: spawned_by.map(Into::into),
            ..Default::default()
        }
    }

    fn squad_at(core: &mut Core, id: u64, origin: &str, pane: u64) {
        core.session.add_squad(
            id,
            vec![origin.into()],
            None,
            Tab {
                name: None,
                id,
                root: Node::Leaf(pane),
                focus: pane,
            },
        );
    }

    fn member(worker: Option<&str>, attach_id: &str, sid: Option<&str>) -> StoredMember {
        StoredMember {
            attach_id: attach_id.into(),
            worker: worker.map(Into::into),
            harness: None,
            harness_session_id: sid.map(Into::into),
            tombstone: false,
            tombstone_reason: None,
            detached: false,
            tab_name: None,
            cwd: None,
            pane_id: None,
        }
    }

    #[test]
    fn a_pane_hosted_parents_workspace_outranks_the_childs_cwd() {
        // AC1-HP: the parent's pane lives in workspace 1; the child's own
        // cwd belongs to workspace 3. The spawner rung answers 1.
        let mut core = empty_core();
        squad_at(&mut core, 1, "/footnote", 5);
        squad_at(&mut core, 3, "/other", 6);
        let parent = row(
            "lead",
            "/footnote",
            Some("claude"),
            Some("sid-parent"),
            None,
        );
        let mut child = row(
            "probe",
            "/other",
            Some("codex"),
            Some("sid-child"),
            Some("SID-PARENT"),
        );
        child.mux = None;
        core.agents = vec![parent, child];
        assert_eq!(core.thread_workspace(&core.agents[1].clone()), Some(1));
    }

    #[test]
    fn a_recorded_member_outranks_lineage_and_cwd() {
        // AC2-HP: member in workspace 2, parent in workspace 1, cwd in
        // workspace 3. The recorded member wins.
        let mut core = empty_core();
        squad_at(&mut core, 1, "/footnote", 5);
        squad_at(&mut core, 2, "/two", 7);
        squad_at(&mut core, 3, "/other", 6);
        core.agents = vec![
            row(
                "lead",
                "/footnote",
                Some("claude"),
                Some("sid-parent"),
                None,
            ),
            row(
                "probe",
                "/other",
                Some("codex"),
                Some("sid-child"),
                Some("sid-parent"),
            ),
        ];
        let mut stored = member(Some("probe"), "", Some("sid-child"));
        stored.harness = Some("codex".into());
        core.squad_members.insert(2, vec![stored]);
        assert_eq!(core.thread_workspace(&core.agents[1].clone()), Some(2));
    }

    #[test]
    fn an_orphan_row_falls_to_the_project_default() {
        // AC3-HP: no member, no parent row in the registry, cwd owned by
        // workspace 3. The project default answers.
        let mut core = empty_core();
        squad_at(&mut core, 3, "/other", 6);
        core.agents = vec![row(
            "probe",
            "/other",
            Some("codex"),
            Some("sid-child"),
            Some("sid-ghost"),
        )];
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), Some(3));
    }

    #[test]
    fn an_edge_cycle_ends_at_the_hop_cap_and_falls_to_cwd() {
        // AC4-EDGE: two rows name each other as spawner. The walk ends
        // within the cap; each row falls to its own project default.
        let mut core = empty_core();
        squad_at(&mut core, 3, "/other", 6);
        core.agents = vec![
            row("a", "/other", Some("codex"), Some("sid-a"), Some("sid-b")),
            row("b", "/other", Some("codex"), Some("sid-b"), Some("sid-a")),
        ];
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), Some(3));
        assert_eq!(core.thread_workspace(&core.agents[1].clone()), Some(3));
    }

    #[test]
    fn an_attach_handle_member_anchors_a_paneless_row() {
        // AC5-HP: the stored member carries only the attach handle; the
        // paneless row carries the same handle. The placement holds.
        let mut core = empty_core();
        squad_at(&mut core, 2, "/two", 7);
        let mut probe = row("probe", "/other", None, None, None);
        probe.attach_id = Some("J".into());
        core.agents = vec![probe];
        core.squad_members.insert(2, vec![member(None, "J", None)]);
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), Some(2));
    }

    #[test]
    fn a_legacy_row_without_an_edge_still_groups_by_cwd() {
        // AC8-EDGE: a paneless row with no spawned_by_session whose cwd a
        // workspace owns keeps grouping there - no regression.
        let mut core = empty_core();
        squad_at(&mut core, 1, "/footnote", 5);
        core.agents = vec![row(
            "old",
            "/footnote/sub/dir",
            Some("codex"),
            Some("sid-old"),
            None,
        )];
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), Some(1));
    }

    #[test]
    fn an_exited_parents_recycled_pane_id_cannot_capture_the_child() {
        // The parent row exited and its pane id now lives in workspace 3's
        // tab (recycled). find_pane(5) answers 3; the exited-parent guard
        // skips that rung, and the parent's own cwd chain answers 1.
        let mut core = empty_core();
        squad_at(&mut core, 1, "/footnote", 6);
        squad_at(&mut core, 3, "/other", 5);
        let mut parent = row(
            "lead",
            "/footnote",
            Some("claude"),
            Some("sid-parent"),
            None,
        );
        parent.mux = Some((core.session_name.clone(), 5));
        parent.exited = true;
        core.agents = vec![
            parent,
            row(
                "probe",
                "/nowhere",
                Some("codex"),
                Some("sid-child"),
                Some("sid-parent"),
            ),
        ];
        assert_eq!(core.thread_workspace(&core.agents[1].clone()), Some(1));
    }

    #[test]
    fn a_parent_sharing_the_childs_display_name_still_answers() {
        // The session id is the join key: a parent row named exactly like
        // its child answers the spawner rung; the name never filters it.
        let mut core = empty_core();
        squad_at(&mut core, 1, "/footnote", 5);
        core.agents = vec![
            row(
                "probe",
                "/footnote",
                Some("claude"),
                Some("sid-parent"),
                None,
            ),
            row(
                "probe",
                "/other",
                Some("codex"),
                Some("sid-child"),
                Some("sid-parent"),
            ),
        ];
        assert_eq!(core.thread_workspace(&core.agents[1].clone()), Some(1));
    }

    #[test]
    fn agent_rows_groups_a_foreign_cwd_thread_under_its_spawners_workspace() {
        // AC6-HP, the think-harness-ranking shape: a live paneless codex
        // thread in a repo no workspace owns, its edge naming a claude
        // session pane-hosted in workspace 1. The built row carries 1.
        let mut core = empty_core();
        squad_at(&mut core, 1, "/footnote", 5);
        core.agents = vec![
            row(
                "lead",
                "/footnote",
                Some("claude"),
                Some("sid-parent"),
                None,
            ),
            row(
                "probe",
                "/tools/foreign",
                Some("codex"),
                Some("sid-child"),
                Some("sid-parent"),
            ),
        ];
        let mut lead = core.agents[0].clone();
        lead.mux = Some((core.session_name.clone(), 5));
        core.agents[0] = lead;
        let rows = core.agent_rows();
        let probe = rows.iter().find(|r| r.name == "probe").unwrap();
        assert_eq!(probe.squad, Some(1), "rows: {rows:?}");
    }

    fn named_at(core: &mut Core, id: u64, name: &str, pane: u64) {
        core.session.add_squad(
            id,
            Vec::new(),
            Some(name.into()),
            Tab {
                name: None,
                id,
                root: Node::Leaf(pane),
                focus: pane,
            },
        );
    }

    fn project(root: &str, id: Option<&str>) -> agent_rows_apply::ProjectRef {
        agent_rows_apply::ProjectRef {
            root: root.into(),
            id: id.map(Into::into),
        }
    }

    #[test]
    fn a_linked_worktree_outside_the_origin_lands_by_its_project_root() {
        // AC1: the workspace owns /code/footnote; the row works in a linked
        // worktree elsewhere. The raw cwd misses, the project root hits.
        let mut core = empty_core();
        squad_at(&mut core, 1, "/code/footnote", 5);
        core.agents = vec![row(
            "wt",
            "/work/trees/x",
            Some("codex"),
            Some("sid-wt"),
            None,
        )];
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), None);
        core.project_by_cwd
            .insert("/work/trees/x".into(), project("/code/footnote", None));
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), Some(1));
    }

    #[test]
    fn the_name_rung_claims_a_row_only_when_one_workspace_matches() {
        // AC2: a named workspace with no origin claims the row whose project
        // id (else root folder name) equals its name. Two claimants read as
        // absent, never a coin flip.
        let mut core = empty_core();
        named_at(&mut core, 1, "fno-books", 5);
        named_at(&mut core, 2, "fnh", 6);
        core.agents = vec![
            row("b", "/books/sub", Some("codex"), Some("sid-b"), None),
            row("h", "/code/fnh", Some("codex"), Some("sid-h"), None),
        ];
        core.project_by_cwd
            .insert("/books/sub".into(), project("/books", Some("fno-books")));
        core.project_by_cwd
            .insert("/code/fnh".into(), project("/code/fnh", None));
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), Some(1));
        assert_eq!(core.thread_workspace(&core.agents[1].clone()), Some(2));
        named_at(&mut core, 3, "fno-books", 7);
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), None);
        // The name also binds the folder, and a new tab opens there.
        core.bind_named_folders();
        assert_eq!(core.session.squad(2).unwrap().canonical_cwd(), "/code/fnh");
        assert_eq!(core.spawn_cwd(2), "/code/fnh");
    }

    #[test]
    fn a_pin_outranks_the_member_and_a_seated_parent_hands_down_its_own_answer() {
        // The operator's move beats the recorded member and the cwd. A
        // parent seated in a portal in workspace 3 does not drag its
        // children there: the child falls through to its own project.
        let mut core = empty_core();
        squad_at(&mut core, 1, "/footnote", 5);
        named_at(&mut core, 2, "fnh", 6);
        squad_at(&mut core, 3, "/viewer", 7);
        core.agents = vec![row(
            "probe",
            "/footnote",
            Some("codex"),
            Some("sid-1"),
            None,
        )];
        let mut stored = member(Some("probe"), "", Some("sid-1"));
        stored.harness = Some("codex".into());
        core.squad_members.insert(1, vec![stored]);
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), Some(1));
        core.thread_pins.push((
            "fnh".into(),
            StoredThreadPin {
                harness: "codex".into(),
                harness_session_id: "sid-1".into(),
            },
        ));
        assert_eq!(core.thread_workspace(&core.agents[0].clone()), Some(2));

        let mut parent = row("lead", "/footnote", Some("claude"), Some("sid-p"), None);
        parent.mux = Some((core.session_name.clone(), 7));
        let child = row(
            "kid",
            "/nowhere",
            Some("codex"),
            Some("sid-k"),
            Some("sid-p"),
        );
        core.agents = vec![parent, child];
        core.portals.insert(
            0,
            crate::thread_viewer::Portal {
                row_key: "lead".into(),
                seat: 7,
                tab: 3,
            },
        );
        assert_eq!(core.thread_workspace(&core.agents[1].clone()), Some(1));
    }
}
