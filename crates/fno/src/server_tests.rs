use super::*;
// The drain-fold tests kept here use the routing module's step constant and
// burst cap; server.rs itself no longer imports these two names.
use crate::proto::TemplateName;
use crate::pty::ChildGuard;
use crate::restore_gate::{set_restore_registry_rows, RestoreRegistryRowsGuard};
use crate::server::mouse_route::{bounded_scroll_target, MOUSE_WHEEL_LINES};
use crate::server::pane_scroll::{wheel_gate, WHEEL_GATE_BUDGET, WHEEL_GATE_WINDOW};

#[path = "server_tests/pane_scroll_tests.rs"]
mod pane_scroll_tests;

#[path = "server/server_thread_viewer_tests.rs"]
mod thread_viewer_tests;
// The portal test family lives in its own module; this file is shrink-only.
#[path = "server/tests/portal_tests.rs"]
mod portal_tests;
// The TV-model acceptance family (views, anchors, prune).
#[path = "server/tests/portal_tv_tests.rs"]
mod portal_tv_tests;
// Same treatment: the lifecycle-resolution test family.
#[path = "server/tests/lifecycle_tests.rs"]
mod lifecycle_tests;
// The (v72) re-seat test family, same treatment.
#[path = "server/tests/reseat_tests.rs"]
mod reseat_tests;
// The (x-eb79) resume-argv staging family, same treatment.
#[path = "server/tests/resume_argv_staging_tests.rs"]
mod resume_argv_staging_tests;
// The sideline rename test family, same treatment.
#[path = "server/tests/rename_tests.rs"]
mod rename_tests;

// The squad-store sync family (prune reload marker + negative control).
#[path = "server/tests/squad_sync_tests.rs"]
mod squad_sync_tests;

// (v75, x-7649) The exact-session retirement handler family.
#[path = "server/tests/retire_session_tests.rs"]
mod retire_session_tests;

// The per-pane orphan verdict family.
#[path = "server/tests/pane_identity_tests.rs"]
mod pane_identity_tests;

// (x-b64e) The restore test family, same treatment: the file is
// shrink-only under the file-budget gate. Moved verbatim.
#[path = "server/tests/server_restore_tests.rs"]
mod server_restore_tests;
#[path = "server/tests/shutdown_tests.rs"]
mod shutdown_tests;
// The keeper re-adoption test family, same treatment.
#[path = "server/tests/keeper_adopt_tests.rs"]
mod keeper_adopt_tests;
// The pane_send fail-closed gate family.
#[path = "server/tests/pane_send_gate_tests.rs"]
mod pane_send_gate_tests;
// The dead-row resume disposition family.
#[path = "server/tests/dead_row_resume_tests.rs"]
mod dead_row_resume_tests;
#[path = "server/tests/reflow_tests.rs"]
mod reflow_tests;

/// AC14-HP, AC15-HP (x-6678, x-296f): the argv builder is keyed on the
/// row's DECLARED attach form. A codex thread execs codex's own TUI after
/// its daemon pre-exec; claude is byte-identical to what it was, account
/// wrapper included.
#[test]
fn attach_argv_rows() {
    set_attach_program(&["claude", "attach"]);
    let dir = std::path::Path::new("/home/u/.claude-alt");

    // AC15: claude, both postures, unchanged.
    assert_eq!(
        attach_argv_for(Some("claude"), "job1", None, None),
        attach_argv("job1", None, None)
    );
    assert_eq!(
        attach_argv_for(Some("claude"), "job1", Some("readyrule"), Some(dir)),
        attach_argv("job1", Some("readyrule"), Some(dir))
    );
    // No harness recorded: keep the claude shape it had before.
    assert_eq!(
        attach_argv_for(None, "job1", None, None),
        attach_argv("job1", None, None)
    );

    // AC14: codex, rendered from the declaration. No account wrapper - the
    // control socket, and therefore CODEX_HOME, is what decides which
    // daemon this reaches; a non-claude harness carrying a config_dir must
    // not inherit a CLAUDE_CONFIG_DIR prefix.
    let uuid = "01a04546-28b2-7a41-ae4c-892bbeb8e295";
    let form = agents_view::attach_form("codex").expect("codex declares an attach form");
    assert_eq!(
        attach_argv_for(Some("codex"), uuid, Some("readyrule"), Some(dir)),
        form.render(uuid)
    );
    assert_eq!(form.render(uuid)[..2], ["sh".to_string(), "-c".to_string()]);

    // Cursor Agent declares NO attach form: a second --resume process is
    // a rival TUI on the same remote chat, not a join. Its rows are
    // driven by pane or by mail, and the re-entry form (--resume with
    // --trust) belongs to resume, not to the attach door.
    assert!(agents_view::attach_form("cursor-agent").is_none());
    assert!(
        agents_view::resume_form("cursor-agent").is_some(),
        "the resume lane stays the honest re-entry"
    );

    set_attach_program(&["claude", "attach"]); // pin the base (no leak)
                                               // Default account: no env wrapper (byte-identical to the bare attach).
    assert_eq!(
        attach_argv("job1", None, None),
        vec![
            "claude".to_string(),
            "attach".to_string(),
            "job1".to_string()
        ]
    );
    // Isolated account: wrapped so `claude attach` hits THAT daemon, with the
    // birth account stamped for the re-attached pane's glyph.
    let dir = std::path::Path::new("/home/u/.claude-alt");
    assert_eq!(
        attach_argv("job1", Some("readyrule"), Some(dir)),
        vec![
            "env".to_string(),
            "CLAUDE_CONFIG_DIR=/home/u/.claude-alt".to_string(),
            "FNO_ACCOUNT=readyrule".to_string(),
            "claude".to_string(),
            "attach".to_string(),
            "job1".to_string(),
        ]
    );

    // The REAL `_mesh_env_wrapper` output for an --account spawn: the auth-var
    // scrub (`-u VAR` pairs) leads the assignments. Both FNO_ACCOUNT AND
    // FNO_NODE must still parse past it (codex P1: a naive scan stopped on
    // `-u` and dropped both, so the badge never showed and node provenance
    // was lost for every routed spawn).
    let argv: Vec<String> = [
        "env",
        "-u",
        "ANTHROPIC_API_KEY",
        "-u",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "FNO_AGENT_SELF=w",
        "FNO_NODE=x-1",
        "FNO_ACCOUNT=readyrule",
        "CLAUDE_CONFIG_DIR=/home/u/.claude-alt",
        "claude",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(node_from_argv(&argv), Some("x-1".to_string()));
    assert_eq!(account_from_argv(&argv), Some("readyrule".to_string()));
    assert_eq!(agent_self_from_argv(&argv), Some("w".to_string()));
    assert_eq!(cmd_from_argv(&argv).as_deref(), Some("claude"));

    // x-0ba1: the registered name lives in the env(1) prefix like FNO_NODE
    // and parses past the same -u scrub. argv[0] past the env run is the
    // QoS wrapper (taskpolicy), not the provider - the title must not read it.
    let to_argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let argv = to_argv(&[
        "env",
        "FNO_AGENT_SELF=build",
        "FNO_NODE=x-2af5",
        "/usr/sbin/taskpolicy",
        "-c",
        "utility",
        "--",
        "claude",
    ]);
    assert_eq!(agent_self_from_argv(&argv).as_deref(), Some("build"));
    // Empty value falls through (env_token_from_argv filters empties).
    assert_eq!(
        agent_self_from_argv(&to_argv(&["env", "FNO_AGENT_SELF=", "claude"])),
        None
    );
    // An ad-hoc `pane run -- htop` carries no FNO_AGENT_SELF.
    assert_eq!(agent_self_from_argv(&to_argv(&["htop"])), None);
}
#[test]
fn tab_label_rows() {
    // Explicit rename wins outright.
    assert_eq!(
        tab_label(
            Some("debug"),
            Some((None, Some("x-1"), "/w/x-2", Some("claude"))),
            "/w",
            0
        ),
        "debug"
    );
    // FNO_NODE provenance beats cwd + cmd (AC1-HP). name absent here.
    assert_eq!(
        tab_label(
            None,
            Some((None, Some("x-abcd"), "/w/x-2", Some("claude"))),
            "/w",
            0
        ),
        "x-abcd"
    );
    // A spawn cwd whose basename differs from the squad's outranks the
    // cmd label (AC2-EDGE: the worktree-per-node case).
    assert_eq!(
        tab_label(
            None,
            Some((
                None,
                None,
                "/conductor/workspaces/footnote/x-9f21",
                Some("claude")
            )),
            "/code/footnote",
            1
        ),
        "x-9f21"
    );
    // Same basename would just echo the squad label -> cmd.
    assert_eq!(
        tab_label(
            None,
            Some((None, None, "/code/footnote", Some("htop"))),
            "/code/footnote",
            1
        ),
        "htop"
    );
    // Every source empty -> the bare 1-based index, exactly today's
    // label (AC1-EDGE, AC2-FR: nothing errors, logs, or bells).
    assert_eq!(
        tab_label(
            None,
            Some((None, None, "/code/footnote", None)),
            "/code/footnote",
            2
        ),
        "3"
    );

    // AC3-FR: tab.focus names a reaped pane (mid-reap race) - the chain
    // skips provenance/cwd/cmd and terminates at the index, no panic.
    assert_eq!(tab_label(None, None, "/w", 0), "1");

    // codex peer review: derived sources (FNO_NODE, dir names, argv) admit
    // control bytes and land in chrome cells - sanitize like a rename.
    assert_eq!(
        tab_label(None, Some((None, Some("\x1b[31mx-1"), "/w", None)), "/w", 0),
        "[31mx-1"
    );
    // A whitespace-only node sanitizes to empty and falls through to the
    // next source instead of rendering a blank label.
    assert_eq!(
        tab_label(None, Some((None, Some("   "), "/w/x-2", None)), "/w", 0),
        "x-2"
    );
    // A control-char-only dir basename falls through to cmd.
    assert_eq!(
        tab_label(
            None,
            Some((None, None, "/w/\x01\x02", Some("htop"))),
            "/w",
            0
        ),
        "htop"
    );

    // x-0ba1 decision b: a QoS-wrapped worker's argv[0] is taskpolicy/nice,
    // so titling from the process table collapses every worker to one label.
    // The registered name is the top derived source and wins over node/cwd/cmd.
    let argv: Vec<String> = [
        "env",
        "FNO_AGENT_SELF=build",
        "FNO_NODE=x-2af5",
        "/usr/sbin/taskpolicy",
        "-c",
        "utility",
        "--",
        "claude",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let pane = (
        agent_self_from_argv(&argv),
        node_from_argv(&argv),
        String::from("/w"),
        cmd_from_argv(&argv),
    );
    // What a process-table title would show (and must NOT be used):
    assert_eq!(pane.3.as_deref(), Some("taskpolicy"));
    assert_eq!(
        tab_label(
            None,
            Some((
                pane.0.as_deref(),
                pane.1.as_deref(),
                pane.2.as_str(),
                pane.3.as_deref()
            )),
            "/w",
            0,
        ),
        "build"
    );

    // x-0ba1 verify: registered names are unique per session, so a second
    // worker yields a different title. cwd basenames are not unique (two
    // workers share a worktree), so name - not cwd - distinguishes them.
    let mk = |self_name: &'static str| (Some(self_name), None, "/w", Some("taskpolicy"));
    assert_eq!(tab_label(None, Some(mk("build")), "/w", 0), "build");
    assert_eq!(tab_label(None, Some(mk("lint")), "/w", 0), "lint");
}

#[test]
fn pane_label_rows() {
    // x-0ba1: the navigator discriminator reads the registered name first,
    // so it no longer shows the QoS wrapper (taskpolicy) either.
    assert_eq!(
        pane_label(Some("build"), Some("x-1"), "/w", Some("taskpolicy")),
        "build"
    );

    // The navigator's pane label (v22, x-653d) when no registered name: cmd
    // is the intra-tab discriminator, then node, then the cwd basename, else
    // "shell". (x-0ba1: a registered name leads when present - see
    // pane_label_prefers_the_registered_name.)
    assert_eq!(
        pane_label(None, Some("x-abcd"), "/home/u/proj", Some("claude")),
        "claude"
    );
    assert_eq!(
        pane_label(None, Some("x-abcd"), "/home/u/proj", None),
        "x-abcd"
    );
    assert_eq!(pane_label(None, None, "/home/u/proj", None), "proj");
    assert_eq!(pane_label(None, None, "", None), "shell");
    // A control-only candidate sanitizes to empty and falls through.
    assert_eq!(
        pane_label(None, None, "/home/u/proj", Some("\u{7}")),
        "proj"
    );

    // x-ed59: an attached/driven pane reads its registered name from the live
    // catalog (the sidepane's source) so its tab/pane title matches the row,
    // not the `claude` command basename. Falls through unchanged for an ad-hoc
    // attach with no matching worker.
    let (mut core, _client, p1, _p2, _rx) = seen_test_core();
    assert_eq!(
        core.panes.get(&p1).unwrap().name,
        None,
        "shell pane starts unnamed"
    );
    core.agents = vec![bg_row("build", "/tmp", Some("deadbee2"))];
    core.name_attached_pane(p1, "deadbee2", None);
    assert_eq!(core.panes.get(&p1).unwrap().name.as_deref(), Some("build"));
    // An ad-hoc attach (no matching worker) leaves the name unset.
    let (mut core2, _, q1, _, _) = seen_test_core();
    core2.name_attached_pane(q1, "no-such-worker", None);
    assert_eq!(core2.panes.get(&q1).unwrap().name, None);
}

#[test]
fn unknown_control_command_gets_typed_wire_refusal() {
    let request = serde_json::json!({
        "Control": {
            "proto": crate::proto::PROTO_VERSION + 1,
            "build": crate::proto::BUILD_VERSION,
            "verb": {"FutureVerb": null},
        }
    });

    match unknown_control_refusal(&request) {
        Some(ServerMsg::Err { code, msg }) => {
            assert_eq!(code, err_code::BAD_REQUEST);
            assert!(msg.contains("FutureVerb"), "{msg}");
            assert!(
                msg.contains(&format!("v{}", crate::proto::PROTO_VERSION)),
                "{msg}"
            );
        }
        other => panic!("expected typed unknown-command refusal, got {other:?}"),
    }
}
// -- Rerun idle guard (x-38c4) ---------------------------------------------

fn agent_in(sess: &str, pane: u64, badge: Option<AgentBadge>, exited: bool) -> RegistryAgent {
    RegistryAgent {
        model: None,
        route: None,
        spawned_by_session: None,
        session_id: None,
        harness_session_id: None,
        predecessor_session_ids: Vec::new(),
        related_session_id: None,
        forked_from_session_id: None,
        name: "w".into(),
        cwd: "/w".into(),
        exited,
        dnd: false,
        badge,
        reason: None,
        mux: Some((sess.into(), pane)),
        answerable: None,
        attach_id: None,
        external: false,
        account: None,
        claude_session_uuid: None,
        log_path: None,
        updated_at: None,
        role_level: None,
        role_scope: None,
        liveness: if exited {
            agents_view::Liveness::Dead
        } else {
            agents_view::Liveness::Alive
        },
        harness: None,
        ..Default::default()
    }
}

#[test]
fn pane_id_floor_seeds_from_legacy_registry_refs() {
    let row = agent_in("old", 41, Some(AgentBadge::Done), true);

    assert_eq!(pane_id_floor(0, &[row]), 42);
}

fn agent(pane: u64, badge: Option<AgentBadge>, exited: bool) -> RegistryAgent {
    agent_in("main", pane, badge, exited)
}
#[test]
fn new_squad_rejects_a_blank_name_and_creates_nothing() {
    // Change #2 / AC1-ERR: a whitespace-only name is refused fail-closed -
    // no squad, no pane. PTY-free: the reject returns before any spawn.
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec!["/x".into()],
        None,
        Tab {
            name: None,
            id: 5,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.clients.push(client(1, 5, (24, 80), false));
    let flow = core.command(
        1,
        Command::NewSquad {
            name: "   ".into(),
            origin: None,
        },
    );
    assert!(matches!(flow, Flow::Continue));
    assert_eq!(core.session.squads.len(), 1, "blank name creates no squad");
    assert!(core.panes.is_empty(), "blank name spawns no pane");
}

#[test]
fn rename_tab_rows() {
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec!["/x".into()],
        None,
        Tab {
            name: None,
            id: 5,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.clients.push(client(1, 5, (24, 80), false));
    // The rename stores the trimmed name (AC2-HP's server half)...
    core.command(
        1,
        Command::RenameTab {
            tab: 5,
            name: "  debug ".into(),
        },
    );
    assert_eq!(
        core.session.squads[0].tabs[0].name.as_deref(),
        Some("debug")
    );
    // ...and a blank rename CLEARS it back to the derived label (AC3-HP,
    // Locked 2) - a clear, never an error. (Re-register the sender: the
    // test client's dropped receiver made the rename's own layout push
    // reap it, exactly like a real disconnect.)
    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RenameTab {
            tab: 5,
            name: "   ".into(),
        },
    );
    assert_eq!(core.session.squads[0].tabs[0].name, None);

    // AC1-ERR: a RenameTab naming a closed tab mutates nothing.
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec!["/x".into()],
        None,
        Tab {
            name: Some("keep".into()),
            id: 5,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.clients.push(client(1, 5, (24, 80), false));
    let flow = core.command(
        1,
        Command::RenameTab {
            tab: 999,
            name: "x".into(),
        },
    );
    assert!(matches!(flow, Flow::Continue));
    assert_eq!(
        core.session.squads[0].tabs[0].name.as_deref(),
        Some("keep"),
        "a stale id must not touch any live tab"
    );

    // AC2-ERR: control chars are stripped and the stored name is capped -
    // the wire is not the overlay, so the server owns the guarantee.
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec!["/x".into()],
        None,
        Tab {
            name: None,
            id: 5,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RenameTab {
            tab: 5,
            name: format!("\x1b[31m{}", "a".repeat(200)),
        },
    );
    let stored = core.session.squads[0].tabs[0].name.clone().unwrap();
    assert_eq!(stored, format!("[31m{}", "a".repeat(MAX_TAB_NAME - 4)));
}

// -- x-96e8 squad management commands ----------------------------------

pub(super) fn leaf_tab(id: TabId, pane: u64) -> Tab {
    Tab {
        name: None,
        id,
        root: Node::Leaf(pane),
        focus: pane,
    }
}
// ---- v41 (x-d865) layout script API server ops ----------------------

/// squad 1: tab 10 = panes [1,2] (H-split); tab 20 "bee" = pane [3].
fn two_tab_core() -> Core {
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec!["/a".into()],
        None,
        Tab {
            name: None,
            id: 10,
            root: Node::Branch {
                axis: Axis::Horizontal,
                children: vec![(0.5, Node::Leaf(1)), (0.5, Node::Leaf(2))],
            },
            focus: 1,
        },
    );
    core.session.squad_mut(1).unwrap().tabs.push(Tab {
        name: Some("bee".into()),
        id: 20,
        root: Node::Leaf(3),
        focus: 3,
    });
    core.sizing.areas.insert(10, (24, 80));
    core.sizing.areas.insert(20, (24, 80));
    core.next_pane_id = 100;
    core
}

// ---- v42 (x-c4d4) declarative layout templates -----------------------

/// A registry row binding fno id `sess_id` to live `pane` in the test
/// session ("test"), so `resolve_local_pane` can find it.
fn bound_agent(sess_id: &str, pane: u64) -> RegistryAgent {
    let mut a = agent_in("test", pane, None, false);
    a.session_id = Some(sess_id.into());
    a
}

/// A one-tab squad (id 1) whose single tab (id 5) holds one real spawned
/// shell pane, plus a scratch shell so template shell slots can spawn.
fn template_core() -> (Core, u64) {
    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    core.next_pane_id = 100;
    let p = core.spawn_pane(24, 80, "/a").unwrap();
    core.session
        .add_squad(1, vec!["/a".into()], Some("sq".into()), leaf_tab(5, p));
    core.sizing.areas.insert(5, (24, 80));
    (core, p)
}

fn shell_spec(t: TemplateName, k: usize) -> LayoutSpec {
    LayoutSpec {
        template: t,
        slots: (0..k).map(|_| SlotBinding::Shell).collect(),
    }
}

#[test]
fn template_rows() {
    // AC1/AC2 shape: main-left with 4 shell slots -> H[ leaf, V[leaf,leaf,leaf] ].
    let (mut core, _p) = template_core();
    let results = core
        .apply_spec(
            1,
            &TabSel::Id(5),
            &shell_spec(TemplateName::MainLeft, 4),
            false,
        )
        .unwrap();
    assert_eq!(results.len(), 4);
    assert!(results
        .iter()
        .all(|r| r.outcome == SlotOutcome::Shell && r.pane_id.is_some()));
    let root = &core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == 5)
        .unwrap()
        .root;
    match root {
        Node::Branch {
            axis: Axis::Horizontal,
            children,
        } => {
            assert!(
                matches!(children[0].1, Node::Leaf(_)),
                "slot 0 is the main leaf"
            );
            assert!(
                matches!(&children[1].1, Node::Branch { axis: Axis::Vertical, children } if children.len() == 3),
                "the rest stack vertically"
            );
        }
        other => panic!("expected H[leaf, V[..]], got {other:?}"),
    }
    assert_eq!(tree::leaves(root).len(), 4, "four live panes");

    // AC3: re-applying the same spec spawns nothing, closes nothing, and the
    // tree comes back byte-identical (the FIFO spare-drain contract).
    let (mut core, _p) = template_core();
    let spec = shell_spec(TemplateName::MainLeft, 4);
    core.apply_spec(1, &TabSel::Id(5), &spec, false).unwrap();
    let root1 = core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == 5)
        .unwrap()
        .root
        .clone();
    let panes1 = core.panes.len();

    let results = core.apply_spec(1, &TabSel::Id(5), &spec, false).unwrap();
    let root2 = core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == 5)
        .unwrap()
        .root
        .clone();
    assert_eq!(root1, root2, "re-apply is byte-identical");
    assert_eq!(core.panes.len(), panes1, "no pane spawned or reaped");
    assert!(results.iter().all(|r| r.outcome == SlotOutcome::Shell));

    // AC1 core: a live session S1 in its own tab; apply main-left binding
    // slot 0 to it -> S1's pane becomes the left main, its source tab empties
    // and is removed, and no pane is spawned for that slot.
    let (mut core, p1) = template_core(); // p1 lives in tab 5
    core.agents = vec![bound_agent("S1", p1)];
    // A second, empty target tab to apply into.
    let tid = core.create_tab_in(1, Some("grid".into())).unwrap();
    core.sizing.areas.insert(tid, (24, 80));

    let spec = LayoutSpec {
        template: TemplateName::MainLeft,
        slots: vec![
            SlotBinding::Fno("S1".into()),
            SlotBinding::Shell,
            SlotBinding::Shell,
            SlotBinding::Shell,
        ],
    };
    let results = core.apply_spec(1, &TabSel::Id(tid), &spec, false).unwrap();
    assert_eq!(results[0].outcome, SlotOutcome::Reused);
    assert_eq!(results[0].pane_id, Some(p1), "S1's pane, reused in place");
    assert!(results[1..].iter().all(|r| r.outcome == SlotOutcome::Shell));
    // Source tab 5 emptied (its only pane relocated) -> removed.
    assert!(
        core.session
            .squad(1)
            .unwrap()
            .tabs
            .iter()
            .all(|t| t.id != 5),
        "source tab removed"
    );
    let target = core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == tid)
        .unwrap();
    assert_eq!(
        tree::leaves(&target.root)[0],
        p1,
        "p1 is the main-left leaf"
    );

    // AC4: slot 1 bound to S2; S2 exits; re-apply -> slot 1 Unbound (shell),
    // no second S2 pane, the surviving bound pane keeps running.
    let (mut core, p1) = template_core();
    let p2 = core.spawn_pane(24, 80, "/a").unwrap();
    core.agents = vec![bound_agent("S1", p1), bound_agent("S2", p2)];
    let spec = LayoutSpec {
        template: TemplateName::MainLeft,
        slots: vec![
            SlotBinding::Fno("S1".into()),
            SlotBinding::Fno("S2".into()),
            SlotBinding::Shell,
            SlotBinding::Shell,
        ],
    };
    core.apply_spec(1, &TabSel::Id(5), &spec, false).unwrap();

    // S2 exits: drop its registry row and reap its pane.
    core.agents
        .retain(|a| a.session_id.as_deref() != Some("S2"));
    core.reap_pane(p2);

    let results = core.apply_spec(1, &TabSel::Id(5), &spec, false).unwrap();
    assert_eq!(
        results[1].outcome,
        SlotOutcome::Unbound,
        "dead S2 slot is a reported shell"
    );
    assert!(results[1].pane_id.is_some(), "the unbound slot got a shell");
    assert!(!core.panes.contains_key(&p2), "no resurrected S2 pane");
    assert!(
        core.panes.contains_key(&p1),
        "the surviving bound pane keeps running"
    );
    assert_eq!(results[0].pane_id, Some(p1));

    // AC5: a grid-2x2 with a bound slot 0; reshape to main-left keeps that
    // pane's id (its PTY untouched, only relocated).
    let (mut core, p1) = template_core();
    core.agents = vec![bound_agent("S1", p1)];
    let grid = LayoutSpec {
        template: TemplateName::Grid2x2,
        slots: vec![
            SlotBinding::Fno("S1".into()),
            SlotBinding::Shell,
            SlotBinding::Shell,
            SlotBinding::Shell,
        ],
    };
    core.apply_spec(1, &TabSel::Id(5), &grid, false).unwrap();
    assert!(core.panes.contains_key(&p1));

    let main_left = LayoutSpec {
        template: TemplateName::MainLeft,
        slots: vec![
            SlotBinding::Fno("S1".into()),
            SlotBinding::Shell,
            SlotBinding::Shell,
        ],
    };
    let results = core
        .apply_spec(1, &TabSel::Id(5), &main_left, false)
        .unwrap();
    assert_eq!(
        results[0].pane_id,
        Some(p1),
        "the bound pane survives the reshape"
    );
    assert!(core.panes.contains_key(&p1));
    let root = &core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == 5)
        .unwrap()
        .root;
    assert_eq!(tree::leaves(root)[0], p1, "and lands as the main-left leaf");
}

#[test]
fn template_refusal_rows() {
    // AC6: a tab too small to tile grid-2x2 -> TEMPLATE_UNFITTABLE, tab
    // unchanged (the pre-mutation atomic refuse).
    let (mut core, _p) = template_core();
    core.sizing.areas.insert(5, (3, 8)); // far too small for four tiles
    let before = core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == 5)
        .unwrap()
        .root
        .clone();
    let err = core
        .apply_spec(
            1,
            &TabSel::Id(5),
            &shell_spec(TemplateName::Grid2x2, 4),
            false,
        )
        .unwrap_err();
    assert_eq!(err.0, err_code::TEMPLATE_UNFITTABLE);
    let after = core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == 5)
        .unwrap()
        .root
        .clone();
    assert_eq!(before, after, "the tab is left completely unchanged");

    // AC7: grid-2x2 with three slots -> TEMPLATE_ARITY, no mutation.
    let (mut core, _p) = template_core();
    let panes_before = core.panes.len();
    let err = core
        .apply_spec(
            1,
            &TabSel::Id(5),
            &shell_spec(TemplateName::Grid2x2, 3),
            false,
        )
        .unwrap_err();
    assert_eq!(err.0, err_code::TEMPLATE_ARITY);
    assert_eq!(
        core.panes.len(),
        panes_before,
        "arity refuse spawns nothing"
    );

    // Codex P1 regression: the same fno id in two slots would commit a
    // duplicate PaneId leaf. Refuse pre-mutation.
    let (mut core, p1) = template_core();
    core.agents = vec![bound_agent("S1", p1)];
    let panes_before = core.panes.len();
    let dup = LayoutSpec {
        template: TemplateName::MainLeft,
        slots: vec![
            SlotBinding::Fno("S1".into()),
            SlotBinding::Fno("S1".into()),
            SlotBinding::Shell,
            SlotBinding::Shell,
        ],
    };
    let err = core.apply_spec(1, &TabSel::Id(5), &dup, false).unwrap_err();
    assert_eq!(err.0, err_code::BAD_REQUEST);
    assert_eq!(core.panes.len(), panes_before, "the refusal spawns nothing");
}

#[test]
fn template_lifecycle_rows() {
    // x-cde1 AC1-HP: Command::RenameTab (the interactive overlay path, not
    // the ControlCommand::TabRename wire API) must re-persist a template tab's
    // spec so restore finds it under the NEW name. persist_squad alone
    // preserves tab_specs byte-for-byte, keeping the stale old key.
    let _s = StoreScratch::new("cde1-rename");
    let (mut core, _p) = template_core();
    core.clients.push(client(1, 5, (24, 80), false));
    core.session.squad_mut(1).unwrap().tabs[0].name = Some("grid".into());
    core.apply_spec(
        1,
        &TabSel::Id(5),
        &shell_spec(TemplateName::MainLeft, 2),
        false,
    )
    .unwrap();
    let loaded = crate::squad_store::load();
    let specs = &loaded
        .squads
        .iter()
        .find(|s| s.name == "sq")
        .unwrap()
        .tab_specs;
    assert_eq!(specs.len(), 1);
    assert_eq!(
        specs[0].tab_name, "grid",
        "persisted under the original name"
    );

    // apply_spec's layout push reaps the test client (dropped receiver);
    // re-register it so the rename command has a live sender to act on.
    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RenameTab {
            tab: 5,
            name: "reviews".into(),
        },
    );

    let loaded = crate::squad_store::load();
    let specs = &loaded
        .squads
        .iter()
        .find(|s| s.name == "sq")
        .unwrap()
        .tab_specs;
    assert_eq!(specs.len(), 1, "still exactly one template spec");
    assert_eq!(specs[0].tab_name, "reviews", "re-keyed under the new name");
    assert!(
        !specs.iter().any(|s| s.tab_name == "grid"),
        "no stale old key survives"
    );

    // x-cde1 AC2-HP: closing a template tab's last pane via close_pane removes
    // the tab and must drop its stored spec, or restore resurrects the closed
    // tab. A second tab keeps the session alive so the removal hits the
    // tab-removed branch (not SessionEmpty/shutdown).
    let _s = StoreScratch::new("cde1-close");
    let (mut core, _p) = template_core();
    core.clients.push(client(1, 5, (24, 80), false));
    core.create_tab_in(1, None)
        .expect("second tab keeps the session alive");
    core.session.squad_mut(1).unwrap().tabs[0].name = Some("grid".into());
    core.apply_spec(
        1,
        &TabSel::Id(5),
        &shell_spec(TemplateName::MainLeft, 2),
        false,
    )
    .unwrap();
    let loaded = crate::squad_store::load();
    let specs = &loaded
        .squads
        .iter()
        .find(|s| s.name == "sq")
        .unwrap()
        .tab_specs;
    assert_eq!(specs.len(), 1, "spec persisted before teardown");

    // Close tab 5's panes one at a time; the last close removes the tab.
    let leaves = tree::leaves(
        &core
            .session
            .squad(1)
            .unwrap()
            .tabs
            .iter()
            .find(|t| t.id == 5)
            .unwrap()
            .root,
    );
    for p in leaves {
        core.close_pane(p);
    }

    let loaded = crate::squad_store::load();
    let specs = &loaded
        .squads
        .iter()
        .find(|s| s.name == "sq")
        .unwrap()
        .tab_specs;
    assert!(
        !specs.iter().any(|s| s.tab_name == "grid"),
        "the closed template tab's spec is dropped from the store"
    );

    // Codex P1 regression: when a re-apply's slots are all bound (no shell
    // slot to absorb it), a dropped bound session's pane must NOT be reaped -
    // it is broken into its own tab, still running (the never-kill invariant).
    let (mut core, p1) = template_core();
    let p2 = core.spawn_pane(24, 80, "/a").unwrap();
    let p3 = core.spawn_pane(24, 80, "/a").unwrap();
    core.agents = vec![
        bound_agent("S1", p1),
        bound_agent("S2", p2),
        bound_agent("S3", p3),
    ];
    let thirds = LayoutSpec {
        template: TemplateName::RowThirds,
        slots: vec![
            SlotBinding::Fno("S1".into()),
            SlotBinding::Fno("S2".into()),
            SlotBinding::Fno("S3".into()),
        ],
    };
    core.apply_spec(1, &TabSel::Id(5), &thirds, false).unwrap();

    // Re-apply main-left binding ONLY S2 and S3 (both slots bound, no shell):
    // S1 is dropped with nowhere to be reused.
    let two = LayoutSpec {
        template: TemplateName::MainLeft,
        slots: vec![SlotBinding::Fno("S2".into()), SlotBinding::Fno("S3".into())],
    };
    core.apply_spec(1, &TabSel::Id(5), &two, false).unwrap();

    assert!(
        core.panes.contains_key(&p1),
        "S1's live pane is never reaped"
    );
    let tab5 = core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == 5)
        .unwrap();
    assert!(
        !tree::leaves(&tab5.root).contains(&p1),
        "S1 left the template tab"
    );
    let hosted = core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .any(|t| tree::leaves(&t.root).contains(&p1));
    assert!(hosted, "S1's pane lives on in a rehomed tab");

    // x-3f39: the reap path was guarded in b7cff6d0, but the recycle-as-shell
    // path was not. A re-apply whose new spec carries a Shell slot must NOT
    // hand a dropped live agent's pane to it (step 7); the live leftover
    // rehomes to its own tab and the Shell slot gets a genuinely fresh shell.
    let (mut core, p1) = template_core();
    let p2 = core.spawn_pane(24, 80, "/a").unwrap();
    let p3 = core.spawn_pane(24, 80, "/a").unwrap();
    core.agents = vec![
        bound_agent("S1", p1),
        bound_agent("S2", p2),
        bound_agent("S3", p3),
    ];
    let thirds = LayoutSpec {
        template: TemplateName::RowThirds,
        slots: vec![
            SlotBinding::Fno("S1".into()),
            SlotBinding::Fno("S2".into()),
            SlotBinding::Fno("S3".into()),
        ],
    };
    core.apply_spec(1, &TabSel::Id(5), &thirds, false).unwrap();

    // Re-apply main-left binding S2 + a SHELL slot: S1 and S3 are dropped
    // live leftovers and the shell slot is a recycle target.
    let spec = LayoutSpec {
        template: TemplateName::MainLeft,
        slots: vec![SlotBinding::Fno("S2".into()), SlotBinding::Shell],
    };
    let results = core.apply_spec(1, &TabSel::Id(5), &spec, false).unwrap();

    // Slot 0 reuses S2; slot 1 is a genuine fresh shell, never a live pane.
    assert_eq!(results[0].pane_id, Some(p2), "slot 0 reuses S2");
    assert_eq!(results[1].outcome, SlotOutcome::Shell);
    let shell_pane = results[1].pane_id.expect("shell slot filled");
    assert_ne!(shell_pane, p1, "shell slot must not be S1's live pane");
    assert_ne!(shell_pane, p3, "shell slot must not be S3's live pane");

    // AC1-FR: both live leftovers survive (never reaped) and each rehomes to
    // a tab of its own, out of the template tab.
    assert!(
        core.panes.contains_key(&p1),
        "S1's live pane is never reaped"
    );
    assert!(
        core.panes.contains_key(&p3),
        "S3's live pane is never reaped"
    );
    let sq = core.session.squad(1).unwrap();
    let tab5_leaves = tree::leaves(&sq.tabs.iter().find(|t| t.id == 5).unwrap().root);
    assert!(
        !tab5_leaves.contains(&p1) && !tab5_leaves.contains(&p3),
        "dropped live panes left the template tab"
    );
    for p in [p1, p3] {
        let hosted = sq
            .tabs
            .iter()
            .any(|t| t.id != 5 && tree::leaves(&t.root).contains(&p));
        assert!(hosted, "live leftover {p} lives on in its own rehomed tab");
    }

    // AC3-EDGE: the step-6 partition must not disturb genuine-shell
    // recycling. A real shell is not live-bound, so it stays in the recycle
    // pool and a re-apply of the same spec reuses it FIFO - no new spawn, no
    // rehome tab.
    let (mut core, p1) = template_core();
    core.agents = vec![bound_agent("S1", p1)];
    let spec = LayoutSpec {
        template: TemplateName::MainLeft,
        slots: vec![SlotBinding::Fno("S1".into()), SlotBinding::Shell],
    };
    let r1 = core.apply_spec(1, &TabSel::Id(5), &spec, false).unwrap();
    let shell1 = r1[1].pane_id.expect("shell filled");
    let tabs_after_first = core.session.squad(1).unwrap().tabs.len();

    let r2 = core.apply_spec(1, &TabSel::Id(5), &spec, false).unwrap();
    assert_eq!(r2[0].pane_id, Some(p1), "S1 still reused in slot 0");
    assert_eq!(
        r2[1].pane_id,
        Some(shell1),
        "the same genuine shell recycles, not a new spawn"
    );
    assert_eq!(
        core.session.squad(1).unwrap().tabs.len(),
        tabs_after_first,
        "no rehome tab created for a genuine shell"
    );
}
#[test]
fn layout_get_carries_nested_tree_and_geometry() {
    // Locked Decision 5: structure AND rects.
    let core = two_tab_core();
    let squads = core.layout_get(&LayoutScope::Session, None).unwrap();
    assert_eq!(squads.len(), 1);
    let tab10 = squads[0].tabs.iter().find(|t| t.tab_id == 10).unwrap();
    assert!(
        matches!(tab10.root, Node::Branch { .. }),
        "the nested tree is carried, not flattened"
    );
    assert_eq!(tab10.panes.len(), 2, "both panes are tiled with rects");
    assert!(tab10.panes.iter().all(|(_, r)| r.cols > 0 && r.rows > 0));
    // (x-1499) The worker join is OPT-IN: absent (None) unless the caller
    // asked, so the machine JSON shape never grows a key on a healthy
    // reply; asked for, it joins the registry rows by pane.
    assert!(
        squads[0].tabs.iter().all(|t| t.workers.is_none()),
        "no workers key without the request"
    );
    let mut core = two_tab_core();
    core.session_name = "sess".into();
    let mut w = agent_in("sess", 1, None, false);
    w.session_id = Some("W1".into());
    core.agents = vec![w];
    let squads = core
        .layout_get(&LayoutScope::Session, Some(&core.agents.clone()))
        .unwrap();
    let tab10 = squads[0].tabs.iter().find(|t| t.tab_id == 10).unwrap();
    assert_eq!(
        tab10.workers.as_ref().map(|ws| ws.first().cloned()),
        Some(Some(TabPaneOccupant {
            pane_id: 1,
            fno_id: Some("W1".into())
        })),
        "the requested join names the pane's worker"
    );
}

#[test]
fn split_pane_script_splits_arbitrary_pane_without_stealing_focus() {
    // AC1-HP + AC1-FR: split the NON-focused pane 2 (tab focus is 1); the
    // new pane appears but the viewer's focus stays put.
    let mut core = two_tab_core();
    core.shells = vec!["/bin/cat".into()];
    assert_eq!(core.session.squad(1).unwrap().tabs[0].focus, 1);
    let new_pid = core.split_pane_script(2, Dir::Right, true).unwrap();
    let tab = &core.session.squad(1).unwrap().tabs[0];
    let mut ls = tree::leaves(&tab.root);
    ls.sort_unstable();
    assert_eq!(ls, vec![1, 2, new_pid], "the 3rd pane joined the tab");
    assert_eq!(tab.focus, 1, "a scripted split never steals focus");
    core.reap_pane(new_pid);
}

// The pane-run placement family (x-18c4 receipt plus the named-tab/anchor
// placement test) moved verbatim into its own module: this file is over
// the shrink-only line, and test motion is the sanctioned shrink.
#[path = "server/tests/pane_run_receipt_tests.rs"]
mod pane_run_receipt_tests;

// The fit placement tests (x-ae47) moved out for the same reason.
#[path = "server/tests/placement_fit_tests.rs"]
mod placement_fit_tests;
#[test]
fn tab_create_and_rename_sanitize_names() {
    // codex P2: control bytes stripped and length capped at the wire
    // boundary, exactly like Command::RenameTab.
    let mut core = two_tab_core();
    core.shells = vec!["/bin/cat".into()];
    let pid = core
        .tab_create(&PaneTarget::SquadId(1), Some("\x1b[31mwork".into()))
        .unwrap();
    let (sid, ti) = core.session.find_pane(pid).unwrap();
    let name = core.session.squad(sid).unwrap().tabs[ti]
        .name
        .clone()
        .unwrap();
    assert!(!name.contains('\x1b'), "escape byte stripped: {name:?}");
    core.reap_pane(pid);

    core.tab_rename(
        &PaneTarget::SquadId(1),
        &TabSel::Id(10),
        "x".repeat(MAX_TAB_NAME + 50),
    )
    .unwrap();
    let renamed = core.session.squad(1).unwrap().tabs[0].name.clone().unwrap();
    assert!(renamed.len() <= MAX_TAB_NAME, "oversized name capped");
}
#[test]
fn session_lineage_pane_ls_reports_thread_and_current_beside_each_other() {
    // AC7-HP (mux half): the listing carries the stable thread id AND the
    // current harness session, with the succession chain and fork edge
    // alongside, so a retired id can never read as current.
    let (mut core, pane_id) = template_core();
    core.session_name = "sess".into();
    core.panes.get_mut(&pane_id).unwrap().name = Some("worker".into());
    let mut successor = agent_in("sess", pane_id, None, false);
    successor.harness_session_id = Some("session-b".into());
    successor.predecessor_session_ids = vec!["session-a".into()];
    successor.forked_from_session_id = None;
    successor.name = "worker".into();

    match core.pane_ls_from_fresh_agents(Some(&[successor.clone()])) {
        ServerMsg::PaneList { panes } => {
            let pane = panes.iter().find(|p| p.pane_id == pane_id).unwrap();
            assert_eq!(pane.harness_session_id.as_deref(), Some("session-b"));
            assert_eq!(pane.predecessor_session_ids, vec!["session-a"]);
            assert!(pane.forked_from_session_id.is_none());
        }
        other => panic!("pane ls should join lineage, got {other:?}"),
    }

    // A branch row shows its fork edge.
    let mut branch = agent_in("sess", pane_id, None, false);
    branch.harness_session_id = Some("session-c".into());
    branch.forked_from_session_id = Some("session-b".into());
    branch.name = "worker-branch".into();
    match core.pane_ls_from_fresh_agents(Some(&[branch.clone()])) {
        ServerMsg::PaneList { panes } => {
            let pane = panes.iter().find(|p| p.pane_id == pane_id).unwrap();
            assert_eq!(pane.forked_from_session_id.as_deref(), Some("session-b"));
            assert!(pane.predecessor_session_ids.is_empty());
        }
        other => panic!("pane ls should join the fork edge, got {other:?}"),
    }
}

#[test]
fn squad_rename_rows() {
    // A blank rename clears an origin-backed squad to its derived label. An
    // origin-less squad has no derivable label, so the blank is refused.
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/x".into()], Some("work".into()), leaf_tab(5, 1));
    core.session
        .add_squad(2, vec![], Some("scratch".into()), leaf_tab(6, 2));

    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RenameSquad {
            squad: 1,
            name: "  oss ".into(),
        },
    );
    assert_eq!(core.session.squads[0].name.as_deref(), Some("oss"));

    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RenameSquad {
            squad: 1,
            name: "   ".into(),
        },
    );
    assert_eq!(
        core.session.squads[0].name, None,
        "blank clears an origin-backed squad to its derived label"
    );

    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RenameSquad {
            squad: 2,
            name: "".into(),
        },
    );
    assert_eq!(
        core.session.squads[1].name.as_deref(),
        Some("scratch"),
        "an origin-less squad refuses a blank (nothing to derive)"
    );

    // Reported as "renaming a workspace to a name another workspace
    // already carries fails silently". named_squad_taken already guards
    // this - the invariant this test pins is that the rename never
    // applies, so the operator's row keeps its old name rather than reading
    // as a no-op when it is actually a name collision with nothing renamed.
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/x".into()], Some("work".into()), leaf_tab(5, 1));
    core.session
        .add_squad(2, vec!["/y".into()], Some("scratch".into()), leaf_tab(6, 2));
    let (tx, mut rx) = mpsc::channel(4);
    core.clients.push(Client {
        reliable_tx: tx,
        ..client(1, 5, (24, 80), false)
    });

    core.command(
        1,
        Command::RenameSquad {
            squad: 1,
            name: "scratch".into(),
        },
    );

    assert_eq!(
        core.session.squads[0].name.as_deref(),
        Some("work"),
        "a rename onto a taken name must not apply"
    );
    match rx.try_recv() {
        Ok(ServerMsg::Notice { text }) => assert_eq!(text, "name taken"),
        other => panic!("expected a name-taken notice, got {other:?}"),
    }

    // AC12/AC13-HP (x-6b0b): an unnamed->named rename used to fall through
    // the plain persist: upsert wrote the new name AND the old key, and
    // `same_squad` matching named rows by name alone left the old key row
    // alive - one rename, two rows. Now the old key row is removed, the
    // live key is cleared, and every shape holds exactly one row.
    let _s = StoreScratch::new("x6b0b-rename-unnamed");
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/repo".into()], None, leaf_tab(5, 1));
    core.squad_members.insert(1, vec![]);
    core.persist_squad(1);
    let key_before = crate::squad_store::load().squads[0].key.clone();
    assert!(
        !key_before.is_empty(),
        "the unnamed squad persists under a durable key"
    );

    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RenameSquad {
            squad: 1,
            name: "oss".into(),
        },
    );

    let loaded = crate::squad_store::load();
    assert_eq!(
        loaded.squads.len(),
        1,
        "AC12-HP: one row, not a minted twin"
    );
    assert_eq!(loaded.squads[0].name, "oss");
    assert!(
        loaded.squads[0].key.is_empty(),
        "AC12-HP: a named row keys by name"
    );
    assert_eq!(core.session.squads[0].key, String::new());

    // AC13-HP: clearing the name re-derives the key from origins.
    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RenameSquad {
            squad: 1,
            name: "   ".into(),
        },
    );
    let loaded = crate::squad_store::load();
    assert_eq!(loaded.squads.len(), 1, "AC13-HP: still exactly one row");
    assert_eq!(loaded.squads[0].name, "");
    assert_eq!(
        loaded.squads[0].key,
        crate::squad_store::origin_key(&["/repo".to_string()]),
        "AC13-HP: the key re-derives from origins"
    );

    // AC2-ERR: a RemoveSquad naming a dead id touches nothing.
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], None, leaf_tab(5, 1));
    core.clients.push(client(1, 5, (24, 80), false));
    let flow = core.command(1, Command::RemoveSquad(999));
    assert!(matches!(flow, Flow::Continue));
    assert_eq!(core.session.squads.len(), 1, "no squad removed");

    // AC2-HP / AC2-EDGE (server half): removing a squad drops it and re-
    // anchors active_squad; removing the last squad ends the session.
    // Store half (de-persist contract): both rows leave the store too,
    // the SessionEmpty path included.
    let _s = StoreScratch::new("x361b-remove-reanchor");
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], None, leaf_tab(5, 1));
    core.session
        .add_squad(2, vec!["/b".into()], None, leaf_tab(6, 2));
    core.session.active_squad = Some(1);
    core.persist_squad(1);
    core.persist_squad(2);
    let ident1 = core.squad_identity(1).expect("identity before removal");
    let ident2 = core.squad_identity(2).expect("identity before removal");
    assert!(
        crate::squad_store::load()
            .squads
            .iter()
            .any(|s| (s.name.clone(), s.key.clone()) == ident1),
        "row 1 written before the removal"
    );

    core.clients.push(client(1, 5, (24, 80), false));
    let flow = core.command(1, Command::RemoveSquad(1));
    assert!(matches!(flow, Flow::Continue));
    assert_eq!(core.session.squads.len(), 1);
    assert_eq!(core.session.squad(1), None);
    assert_eq!(
        core.session.active_squad,
        Some(2),
        "active re-anchors to a survivor"
    );

    core.clients.push(client(1, 6, (24, 80), false));
    let flow = core.command(1, Command::RemoveSquad(2));
    assert!(
        matches!(flow, Flow::Shutdown),
        "removing the last squad ends the session (Locked Decision 8)"
    );
    assert!(core.session.squads.is_empty());
    let rows = crate::squad_store::load().squads;
    assert!(
        !rows
            .iter()
            .any(|s| (s.name.clone(), s.key.clone()) == ident1
                || (s.name.clone(), s.key.clone()) == ident2),
        "both dismissed workspaces left the store, SessionEmpty included"
    );
}

#[test]
fn depersist_rows() {
    // The reported leak: a workspace of plain shell panes has no member
    // context, so CloseTab's reconcile loop did nothing and the row
    // survived in the store. Positives on both sides: the closed identity
    // is gone AND the sibling's row is still there.
    let _s = StoreScratch::new("x361b-closetab");
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], Some("one".into()), leaf_tab(5, 1));
    core.session
        .add_squad(2, vec!["/b".into()], Some("two".into()), leaf_tab(6, 2));
    core.persist_squad(1);
    core.persist_squad(2);
    let ident1 = core.squad_identity(1).expect("identity before close");
    let ident2 = core.squad_identity(2).expect("identity before close");
    assert!(
        crate::squad_store::load()
            .squads
            .iter()
            .any(|s| (s.name.clone(), s.key.clone()) == ident1),
        "row written before the close"
    );

    core.clients.push(client(1, 5, (24, 80), false));
    let flow = core.command(1, Command::CloseTab);
    assert!(matches!(flow, Flow::Continue), "squad 2 keeps the session");
    let rows = crate::squad_store::load().squads;
    assert!(
        !rows
            .iter()
            .any(|s| (s.name.clone(), s.key.clone()) == ident1),
        "the memberless workspace's row left the store"
    );
    assert!(
        rows.iter()
            .any(|s| (s.name.clone(), s.key.clone()) == ident2),
        "the sibling's row survives"
    );

    // x-9052 AC4-EDGE: a shell-only tab close used to persist NOTHING on
    // a surviving squad, so its stored tree replayed at every restart.
    // Positives both sides: the closed tree leaves tab_trees, the
    // sibling's tree stays.
    let _s = StoreScratch::new("x9052-shellclose");
    let mut core = empty_core();
    let mut one = leaf_tab(5, 1);
    let two = leaf_tab(6, 2);
    one.name = Some("shell-a".into());
    core.session
        .add_squad(1, vec!["/a".into()], Some("one".into()), one);
    core.session.squad_mut(1).expect("squad").tabs.push(two);
    core.persist_squad(1);
    let ident = core.squad_identity(1).expect("identity");
    assert_eq!(
        crate::squad_store::load()
            .squads
            .iter()
            .find(|s| (s.name.clone(), s.key.clone()) == ident)
            .map(|s| s.tab_trees.len()),
        Some(2),
        "both trees captured before the close"
    );
    core.close_tab_cascade(1, 1);
    assert_eq!(
        crate::squad_store::load()
            .squads
            .iter()
            .find(|s| (s.name.clone(), s.key.clone()) == ident)
            .map(|s| s.tab_trees.len()),
        Some(1),
        "the closed tab's tree left the store"
    );

    // x-9052 AC4-HP: a churned worker's persist used to be members-only,
    // so its collapsed tab stayed in tab_trees forever.
    let _s = StoreScratch::new("x9052-churn");
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], Some("one".into()), leaf_tab(5, 1));
    let member = crate::squad_store::StoredMember {
        attach_id: "a1b2c3d4".into(),
        tombstone: false,
        tombstone_reason: None,
        detached: false,
        tab_name: None,
        cwd: None,
        worker: None,
        harness: None,
        harness_session_id: None,
        pane_id: None,
    };
    core.squad_members.insert(1, vec![member.clone()]);
    core.attached.insert("a1b2c3d4".into(), 5);
    core.persist_squad(1);
    let ident = core.squad_identity(1).expect("identity");
    assert_eq!(
        crate::squad_store::load()
            .squads
            .iter()
            .find(|s| (s.name.clone(), s.key.clone()) == ident)
            .map(|s| s.tab_trees.len()),
        Some(1),
        "tree captured while the member pane lives"
    );
    // The pane and its tab die with the server's session bookkeeping;
    // the squad survives (a sibling tab keeps it).
    let two = leaf_tab(6, 2);
    core.session.squad_mut(1).expect("squad").tabs.push(two);
    core.session.remove_tab(1, 0);
    core.attached.remove("a1b2c3d4");
    let ctx = (
        1u64,
        ident.0.clone(),
        ident.1.clone(),
        vec!["/a".into()],
        "a1b2c3d4".to_string(),
    );
    core.reconcile_member_close(Some(ctx), true);
    assert_eq!(
        crate::squad_store::load()
            .squads
            .iter()
            .find(|s| (s.name.clone(), s.key.clone()) == ident)
            .map(|s| s.tab_trees.len()),
        Some(1),
        "the churned member's tree left the store; the sibling's stays"
    );

    // Same contract through close_pane (the mouse pane-close path), which
    // de-persisted never before: it handled template specs and nothing
    // else when remove_tab dropped the squad.
    let _s = StoreScratch::new("x361b-closepane");
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], Some("one".into()), leaf_tab(5, 1));
    core.session
        .add_squad(2, vec!["/b".into()], Some("two".into()), leaf_tab(6, 2));
    core.persist_squad(1);
    core.persist_squad(2);
    let ident1 = core.squad_identity(1).expect("identity before close");
    let ident2 = core.squad_identity(2).expect("identity before close");

    let flow = core.close_pane(1);
    assert!(matches!(flow, Flow::Continue), "squad 2 keeps the session");
    let rows = crate::squad_store::load().squads;
    assert!(
        !rows
            .iter()
            .any(|s| (s.name.clone(), s.key.clone()) == ident1),
        "closing the last pane cleared the workspace's row"
    );
    assert!(
        rows.iter()
            .any(|s| (s.name.clone(), s.key.clone()) == ident2),
        "the sibling's row survives"
    );

    // The gate this drops: a squad the store holds but squad_members does
    // not (restore's per-squad isolation can produce exactly that) took
    // the false branch and its row survived the dismiss.
    let _s = StoreScratch::new("x361b-remove-untracked");
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], Some("one".into()), leaf_tab(5, 1));
    core.persist_squad(1);
    let ident1 = core.squad_identity(1).expect("identity before removal");
    assert!(
        !core.squad_members.contains_key(&1),
        "precondition: the squad is in the store but untracked"
    );

    core.clients.push(client(1, 5, (24, 80), false));
    let flow = core.command(1, Command::RemoveSquad(1));
    assert!(matches!(flow, Flow::Shutdown));
    let rows = crate::squad_store::load().squads;
    assert!(
        !rows
            .iter()
            .any(|s| (s.name.clone(), s.key.clone()) == ident1),
        "the untracked workspace's row left the store"
    );
}

#[test]
fn prune_done_slots_collapses_and_skips() {
    // x-9052 AC3-EDGE: pure collapse semantics - a done leaf vanishes, a
    // one-child split unwraps, an emptied tree is None, weights and
    // siblings survive.
    let done: HashSet<String> = ["gone-slot".to_string()].into_iter().collect();
    let tree = LayoutTreeSpec::Split {
        axis: crate::tree::Axis::Horizontal,
        children: vec![
            LayoutTreeChild {
                weight: 2.0,
                tree: LayoutTreeSpec::Slot("gone-slot".into()),
            },
            LayoutTreeChild {
                weight: 1.0,
                tree: LayoutTreeSpec::Slot("kept".into()),
            },
        ],
    };
    let pruned = prune_done_slots(&tree, &done).expect("one leaf survives");
    match pruned {
        LayoutTreeSpec::Slot(name) => assert_eq!(name, "kept"),
        other => panic!("one-child split should unwrap, got {other:?}"),
    }
    let all_done: HashSet<String> = ["gone-slot".to_string(), "kept".to_string()]
        .into_iter()
        .collect();
    assert!(prune_done_slots(&tree, &all_done).is_none());
}

#[test]
fn reorder_rows() {
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], None, leaf_tab(5, 1));
    core.session
        .squad_mut(1)
        .unwrap()
        .tabs
        .extend([leaf_tab(6, 2), leaf_tab(7, 3)]);
    core.session.squad_mut(1).unwrap().active_tab = 1;
    let (conn, mut rx) = client_with_rx(1);
    core.clients.push(conn);

    core.command(
        1,
        Command::ReorderTab {
            squad: 1,
            tab: 6,
            delta: 1,
        },
    );

    let squad = core.session.squad(1).unwrap();
    assert_eq!(
        squad.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        vec![5, 7, 6]
    );
    assert_eq!(squad.tabs[squad.active_tab].id, 6);
    assert!(rx.try_recv().is_ok(), "a successful reorder pushes Layout");

    // (x-cf97) The `fno mux tab move` door: `to` names a 1-based POSITION,
    // the server computes the delta, and the same trunk moves the tab
    // while holding the squad's active tab. The ordinal grammar is the
    // shared one: 0 is a refusal, never a silent zero-based index
    // (x-1499), and past-the-end is refused rather than clamped.
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], None, leaf_tab(5, 1));
    core.session
        .squad_mut(1)
        .unwrap()
        .tabs
        .extend([leaf_tab(6, 2), leaf_tab(7, 3)]);
    core.session.squad_mut(1).unwrap().active_tab = 2;

    core.tab_reorder(
        &PaneTarget::SquadId(1),
        &TabSel::Index(3),
        &TabSel::Index(1),
    )
    .unwrap();
    let squad = core.session.squad(1).unwrap();
    assert_eq!(
        squad.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        vec![7, 5, 6]
    );
    assert_eq!(
        squad.tabs[squad.active_tab].id, 7,
        "the active tab survives the move"
    );

    assert!(
        core.tab_reorder(
            &PaneTarget::SquadId(1),
            &TabSel::Index(0),
            &TabSel::Index(1)
        )
        .is_err(),
        "ordinal 0 is refused"
    );
    assert!(
        core.tab_reorder(
            &PaneTarget::SquadId(1),
            &TabSel::Index(1),
            &TabSel::Index(9)
        )
        .is_err(),
        "past-the-end is refused"
    );

    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], None, leaf_tab(5, 1));
    core.session.squad_mut(1).unwrap().tabs.push(leaf_tab(6, 2));
    let (conn, mut rx) = client_with_rx(1);
    core.clients.push(conn);

    core.command(
        1,
        Command::ReorderTab {
            squad: 1,
            tab: 5,
            delta: -1,
        },
    );
    core.command(
        1,
        Command::ReorderTab {
            squad: 1,
            tab: 6,
            delta: 1,
        },
    );

    let squad = core.session.squad(1).unwrap();
    assert_eq!(
        squad.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        vec![5, 6]
    );
    assert!(
        rx.try_recv().is_err(),
        "edge bumps push neither Layout nor Notice"
    );

    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], None, leaf_tab(5, 1));
    let squad = core.session.squad_mut(1).unwrap();
    squad.tabs.push(leaf_tab(6, 2));
    squad.active_tab = usize::MAX;
    core.clients.push(client(1, 5, (24, 80), false));

    core.command(
        1,
        Command::ReorderTab {
            squad: 1,
            tab: 5,
            delta: 1,
        },
    );

    let squad = core.session.squad(1).unwrap();
    assert_eq!(
        squad.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        vec![6, 5]
    );
    assert_eq!(squad.active_tab, 1);

    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], None, leaf_tab(5, 1));
    let (conn, mut rx) = client_with_rx(1);
    core.clients.push(conn);

    core.command(
        1,
        Command::ReorderTab {
            squad: 1,
            tab: 999,
            delta: 1,
        },
    );

    assert_eq!(core.session.find_tab(5), Some((1, 0)));
    assert_eq!(drain_notice(&mut rx).as_deref(), Some("no such tab"));

    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], None, leaf_tab(5, 1));
    core.session.squad_mut(1).unwrap().tabs.push(leaf_tab(6, 2));
    core.session
        .add_squad(2, vec!["/b".into()], None, leaf_tab(7, 3));
    core.session.squad_mut(2).unwrap().tabs.push(leaf_tab(8, 4));
    let (conn, mut rx) = client_with_rx(1);
    core.clients.push(conn);

    core.command(1, Command::MoveTab { tab: 6, squad: 2 });
    while rx.try_recv().is_ok() {}
    core.command(
        1,
        Command::ReorderTab {
            squad: 1,
            tab: 6,
            delta: -1,
        },
    );

    assert_eq!(
        core.session
            .squad(2)
            .unwrap()
            .tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        vec![7, 8, 6],
        "a stale reorder must not mutate the destination squad"
    );
    assert!(drain_notice(&mut rx).unwrap().contains("moved"));

    // AC3-HP + Boundaries: reorder clamps to the list, and an at-edge move
    // is a silent no-op (holding a reorder key at the top must not churn).
    let mut core = empty_core();
    for (sid, tid, pid) in [(1u64, 5u64, 1u64), (2, 6, 2), (3, 7, 3)] {
        core.session
            .add_squad(sid, vec![format!("/{sid}")], None, leaf_tab(tid, pid));
    }
    let order = |c: &Core| c.session.squads.iter().map(|s| s.id).collect::<Vec<_>>();

    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::MoveSquad {
            squad: 3,
            delta: -1,
        },
    );
    assert_eq!(order(&core), vec![1, 3, 2], "squad 3 moved up one");

    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::MoveSquad {
            squad: 1,
            delta: -1,
        },
    );
    assert_eq!(
        order(&core),
        vec![1, 3, 2],
        "an at-edge bump changes nothing"
    );

    // Invariant (view validity): a viewer of the moved tab follows it into
    // the destination squad - content continuity beats spatial position.
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/a".into()], None, leaf_tab(5, 1));
    core.session
        .add_squad(2, vec!["/b".into()], None, leaf_tab(6, 2));
    // A live receiver so push_layout does not reap the client before we
    // read its post-move view.
    let (tx, _rx) = mpsc::channel(8);
    core.clients.push(Client {
        id: 1,
        reliable_tx: tx,
        dirty: Arc::default(),
        notify: Arc::new(Notify::new()),
        synced_modes: Modes::default(),
        view: (1, 5),
        visible: HashSet::new(),
        dims: (24, 80),
        passive: false,
        last_press: None,
    });

    core.command(1, Command::MoveTab { tab: 5, squad: 2 });
    assert_eq!(
        core.session.find_tab(5),
        Some((2, 1)),
        "tab 5 re-homed into squad 2"
    );
    assert_eq!(
        core.clients[0].view,
        (2, 5),
        "the viewer follows the moved tab into its new squad"
    );
}

#[test]
fn attach_agent_refuses_unknown_or_malformed_jobid() {
    // The jobId lands in `claude attach <id>`'s argv, so an out-of-shape id
    // is refused before any pane spawns (argv defense in depth). A
    // well-formed id that names no surfaced watch-only row is refused too
    // (catalog membership, like the sibling FocusPane/SelectTab commands) -
    // here `empty_core` has no agents, so even valid-shape "deadbeef" fails.
    for bad in [
        "short",
        "toolongxx",
        "ZZZZZZZZ",
        "; rm -rf /",
        "c19cd2c",
        "deadbeef",
    ] {
        let mut core = empty_core();
        let flow = core.command(1, Command::attach_agent(bad));
        assert!(matches!(flow, Flow::Continue));
        assert!(
            core.panes.is_empty(),
            "un-surfaced/malformed jobId {bad:?} must not spawn a pane"
        );
    }
}

pub(super) fn client_with_rx(id: u64) -> (Client, mpsc::Receiver<ServerMsg>) {
    let (tx, rx) = mpsc::channel::<ServerMsg>(8);
    let mut c = client(id, 5, (24, 80), false);
    c.reliable_tx = tx;
    (c, rx)
}

fn drain_notice(rx: &mut mpsc::Receiver<ServerMsg>) -> Option<String> {
    let mut out = None;
    while let Ok(ServerMsg::Notice { text }) = rx.try_recv() {
        out = Some(text);
    }
    out
}

#[test]
fn stop_agent_unknown_name_refused() {
    // US5 / AC3-ERR: a StopAgent naming a row absent from the catalog is
    // refused fail-closed with a notice. A plain #[test] has no tokio
    // runtime, so reaching agent_action's spawn would panic - the clean
    // refusal is also proof the happy path is never taken here.
    let mut core = empty_core();
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    let flow = core.command(
        1,
        Command::StopAgent {
            harness_session_id: None,
            name: "ghost".into(),
            pane_id: None,
        },
    );
    assert!(matches!(flow, Flow::Continue));
    assert!(drain_notice(&mut rx).unwrap().contains("no such agent"));
}

/// A helper for the respawn refusal tests: an EXITED registry row with an
/// optional recorded claude session uuid.
pub(super) fn exited_claude_row(name: &str, uuid: Option<&str>) -> RegistryAgent {
    RegistryAgent {
        model: None,
        route: None,
        spawned_by_session: None,
        session_id: None,
        harness_session_id: None,
        predecessor_session_ids: Vec::new(),
        related_session_id: None,
        forked_from_session_id: None,
        name: name.into(),
        cwd: "/w".into(),
        exited: true,
        dnd: false,
        liveness: agents_view::Liveness::Dead,
        badge: None,
        reason: None,
        mux: None,
        answerable: None,
        attach_id: None,
        external: false,
        account: None,
        claude_session_uuid: uuid.map(str::to_owned),
        log_path: None,
        updated_at: None,
        role_level: None,
        role_scope: None,
        harness: None,
        ..Default::default()
    }
}
#[test]
fn resume_agent_rows() {
    // x-5f7f: a dead paneless codex row resumes through codex's own form
    // in the recorded cwd. x-eb79: the argv now resolves off-loop (the
    // staged seam here), so the test stages what `fno-agents resume-argv`
    // would return; /bin/cat keeps the spawn hermetic.
    let _guard = ResumeProgramGuard;
    set_resume_program(&["/bin/cat"]);
    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let cwd = std::env::temp_dir().join("fno-resume-cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let shell = core
        .spawn_pane(24, 80, cwd.to_string_lossy().as_ref())
        .unwrap();
    core.session.add_squad(
        7,
        vec![cwd.to_string_lossy().into_owned()],
        None,
        Tab {
            name: None,
            id: 70,
            root: Node::Leaf(shell),
            focus: shell,
        },
    );
    core.agents = vec![RegistryAgent {
        harness_session_id: Some("01a027ad-fe00-7c12-a116-9ee37c6bdfec".into()),
        harness: Some("codex".into()),
        name: "t-codex-one".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        exited: true,
        liveness: agents_view::Liveness::Dead,
        ..Default::default()
    }];
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    // The gesture consumes the staged argv (what the off-loop
    // `fno-agents resume-argv` shell-out would deliver).
    core.staged_resume_argv = Some(vec![
        "/bin/cat".into(),
        "01a027ad-fe00-7c12-a116-9ee37c6bdfec".into(),
    ]);
    core.command(
        1,
        Command::ResumeAgent {
            name: "t-codex-one".into(),
        },
    );
    let notices = drain_notices(&mut rx).join("\n");
    assert!(notices.contains("resumed t-codex-one"), "{notices}");
    // One NEW pane beyond the seed shell, running the staged program,
    // titled from the registry row, placed in the squad owning the cwd.
    let new_panes: Vec<&u64> = core.panes.keys().filter(|&&p| p != shell).collect();
    assert_eq!(new_panes.len(), 1, "exactly one resumed pane");
    let entry = core.panes.get(new_panes[0]).unwrap();
    assert_eq!(entry.cmd.as_deref(), Some("cat"), "the override ran");
    assert_eq!(
        entry.name.as_deref(),
        Some("t-codex-one"),
        "the pane is titled from the registry row"
    );
    let members = core.squad_members.get(&7).expect("member recorded");
    assert!(
        members
            .iter()
            .any(|m| m.worker.as_deref() == Some("t-codex-one")),
        "the resumed pane persists as a worker member: {members:?}"
    );

    // The external-review P1: the resume argv carries no registry binding,
    // so before the worker_pane map a second Resume for the same row
    // launched a SECOND session on the same rollout. The map binds row to
    // pane for the pane's lifetime; a second gesture focuses, and the
    // panel presents the row pane-hosted while it lives. x-eb79: the
    // argv arrives through the staged seam (what `fno-agents
    // resume-argv` would deliver), so /bin/cat keeps the spawn hermetic.
    let _guard = ResumeProgramGuard;
    set_resume_program(&["/bin/cat"]);
    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let cwd = std::env::temp_dir().join("fno-resume-cwd2");
    std::fs::create_dir_all(&cwd).unwrap();
    let shell = core
        .spawn_pane(24, 80, cwd.to_string_lossy().as_ref())
        .unwrap();
    core.session.add_squad(
        7,
        vec![cwd.to_string_lossy().into_owned()],
        None,
        Tab {
            name: None,
            id: 70,
            root: Node::Leaf(shell),
            focus: shell,
        },
    );
    core.agents = vec![RegistryAgent {
        harness_session_id: Some("01a027ad-fe00-7c12-a116-9ee37c6bdfec".into()),
        harness: Some("codex".into()),
        name: "t-codex-one".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        exited: true,
        liveness: agents_view::Liveness::Dead,
        ..Default::default()
    }];
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    core.staged_resume_argv = Some(vec![
        "/bin/cat".into(),
        "01a027ad-fe00-7c12-a116-9ee37c6bdfec".into(),
    ]);
    core.command(
        1,
        Command::ResumeAgent {
            name: "t-codex-one".into(),
        },
    );
    drain_notices(&mut rx);
    let first: Vec<u64> = core.panes.keys().copied().filter(|&p| p != shell).collect();
    assert_eq!(first.len(), 1, "the first resume spawns one pane");
    core.command(
        1,
        Command::ResumeAgent {
            name: "t-codex-one".into(),
        },
    );
    let notices = drain_notices(&mut rx).join("\n");
    assert!(
        notices.contains("already resumed; focused existing pane"),
        "{notices}"
    );
    let second: Vec<u64> = core.panes.keys().copied().filter(|&p| p != shell).collect();
    assert_eq!(second, first, "the second resume spawns nothing");
    // The panel presents the row pane-hosted through the same map.
    let rows = core.agent_rows();
    let row = rows.iter().find(|r| r.name == "t-codex-one").unwrap();
    assert_eq!(
        row.pane_id,
        Some(first[0]),
        "the row is pane-hosted, not idle"
    );
    // Pane death releases the binding: the row returns to idle.
    core.reap_pane(first[0]);
    let rows = core.agent_rows();
    let row = rows.iter().find(|r| r.name == "t-codex-one").unwrap();
    assert_eq!(
        row.pane_id, None,
        "the row returns to idle when the pane dies"
    );

    // Fail-closed catalog gates, same posture as AttachAgent: an unknown
    // name, and a row whose pane is still live (focus, never a second
    // spawn), never reach an argv.
    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let live = core.spawn_pane(24, 80, "/tmp").unwrap();
    core.session.add_squad(
        7,
        vec!["/tmp".into()],
        None,
        Tab {
            name: None,
            id: 70,
            root: Node::Leaf(live),
            focus: live,
        },
    );
    let live_row = RegistryAgent {
        harness_session_id: Some("01a027ad-fe00-7c12-a116-9ee37c6bdfec".into()),
        harness: Some("codex".into()),
        name: "live-codex".into(),
        cwd: "/tmp".into(),
        mux: Some(("test".into(), live)),
        liveness: agents_view::Liveness::Alive,
        ..Default::default()
    };
    core.agents = vec![live_row];
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    core.command(
        1,
        Command::ResumeAgent {
            name: "ghost".into(),
        },
    );
    assert!(drain_notice(&mut rx).unwrap().contains("no such agent"));
    core.command(
        1,
        Command::ResumeAgent {
            name: "live-codex".into(),
        },
    );
    assert!(
        drain_notice(&mut rx).unwrap().contains("live pane"),
        "a row with a live pane is refused, never double-spawned"
    );
    assert_eq!(core.panes.len(), 1, "nothing was spawned by either refusal");

    // One table of registry facts owns both the dead-row resume decision
    // and the branch-four reason. A LIVE claude bg row with a jobId still
    // uses attach, but its disposition remains live-paneless.
    let base = || RegistryAgent {
        harness_session_id: Some("01a027ad".into()),
        harness: Some("codex".into()),
        name: "w".into(),
        cwd: "/w".into(),
        exited: true,
        liveness: agents_view::Liveness::Alive,
        ..Default::default()
    };
    assert_eq!(
        Core::row_resume_disposition(&base()),
        RowResumeDisposition::Resumable
    );
    assert!(Core::row_resumable(&base()), "a dead codex row resumes");
    let mut live_codex = base();
    live_codex.exited = false;
    assert_eq!(
        Core::row_resume_disposition(&live_codex),
        RowResumeDisposition::NoPane(AgentNoPaneReason::LivePaneless)
    );
    assert!(
            !Core::row_resumable(&live_codex),
            "a live codex row has a process writing its rollout: resuming under it opens a second writer"
        );
    let mut not_exited = base();
    not_exited.exited = false;
    not_exited.liveness = agents_view::Liveness::Unmeasured;
    assert!(
        !matches!(
            Core::row_resume_disposition(&not_exited),
            RowResumeDisposition::NoPane(AgentNoPaneReason::LivePaneless)
        ),
        "an unmeasured backend must not be labeled live"
    );
    assert_eq!(
        Core::row_resume_disposition(&not_exited),
        RowResumeDisposition::NoPane(AgentNoPaneReason::LivenessUnmeasured),
        "(x-d401) unmeasured names the absent reading, not a dead backend"
    );
    not_exited.liveness = agents_view::Liveness::Dead;
    assert_eq!(
        Core::row_resume_disposition(&not_exited),
        RowResumeDisposition::Resumable,
        "a positive dead reading resumes whatever the status word says"
    );
    let mut agy = base();
    agy.harness = Some("agy".into());
    agy.harness_session_id = None;
    // The no-form arm needs an override: the table declares interactive_resume
    // for agy today (x-7b5e), so a bare agy row now fails on its missing
    // session id instead.
    {
        let _guard = DeclaredResumeFormsGuard;
        set_declared_resume_form("agy", None);
        assert_eq!(
            Core::row_resume_disposition(&agy),
            RowResumeDisposition::NoPane(AgentNoPaneReason::UnsupportedHarness)
        );
        assert!(
            !Core::row_resumable(&agy),
            "no resume form and no session id: no Resume offered"
        );
    }
    // With the declared form, the same row fails one step later - and a
    // dead agy row with a session id IS resumable, which is the AC3-HP
    // change: the old two-arm match offered no Resume at all.
    assert_eq!(
        Core::row_resume_disposition(&agy),
        RowResumeDisposition::NoPane(AgentNoPaneReason::MissingSessionId)
    );
    let mut dead_agy = base();
    dead_agy.harness = Some("agy".into());
    assert_eq!(
        Core::row_resume_disposition(&dead_agy),
        RowResumeDisposition::Resumable,
        "a declared harness with a session id is resumable without a Rust change"
    );
    let mut live_claude = base();
    live_claude.harness = Some("claude".into());
    live_claude.exited = false;
    live_claude.attach_id = Some("c19cd2c3".into());
    assert_eq!(
        Core::row_resume_disposition(&live_claude),
        RowResumeDisposition::NoPane(AgentNoPaneReason::LivePaneless)
    );
    assert!(
        !Core::row_resumable(&live_claude),
        "a live claude bg row attaches; its daemon owns the session"
    );
    let mut dead_claude = live_claude;
    dead_claude.exited = true;
    assert!(
        Core::row_resumable(&dead_claude),
        "a dead claude row resumes from its transcript"
    );
    let mut no_sid = base();
    no_sid.harness_session_id = None;
    assert_eq!(
        Core::row_resume_disposition(&no_sid),
        RowResumeDisposition::NoPane(AgentNoPaneReason::MissingSessionId)
    );
    assert!(
        !Core::row_resumable(&no_sid),
        "no session id means nothing to resume"
    );
    let mut no_harness = base();
    no_harness.harness = None;
    assert_eq!(
        Core::row_resume_disposition(&no_harness),
        RowResumeDisposition::NoPane(AgentNoPaneReason::MissingHarness)
    );

    // (x-d401) The row-to-pane join key: the session id a pane-run argv
    // resumes. Claude's flag form, codex's subcommand form, both behind
    // the env(1) wrapper; a command that merely mentions the token never
    // parses.
    use super::resume_target_from_argv;
    let argv = |toks: &[&str]| toks.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        resume_target_from_argv(&argv(&["claude", "--resume", "01a03a4e-b862"])),
        Some("01a03a4e-b862".into())
    );
    assert_eq!(
        resume_target_from_argv(&argv(&["codex", "resume", "f00dcaf3"])),
        Some("f00dcaf3".into())
    );
    assert_eq!(
        resume_target_from_argv(&argv(&[
            "env",
            "FNO_NODE=x-9d03",
            "FNO_AGENT_SELF=peer",
            "claude",
            "--resume",
            "01a03a4e-b862"
        ])),
        Some("01a03a4e-b862".into())
    );
    assert_eq!(
        resume_target_from_argv(&argv(&["grep", "--resume", "file"])),
        None,
        "a non-harness command never parses"
    );
    assert_eq!(
        resume_target_from_argv(&argv(&["claude", "--resume"])),
        None,
        "a resume flag with no following token parses nothing"
    );
    // (x-d401) A FLAG is not a session id. `codex resume --last` names no
    // session, so storing `--last` yields a join key matching no row, and
    // `pane_resumes_session` then fails to keep that row non-resumable
    // while a pane runs it. One tap opens a second writer on a live
    // rollout, so this filter is a safety guard, not tidiness.
    assert_eq!(
        resume_target_from_argv(&argv(&["codex", "resume", "--last"])),
        None,
        "a flag is not a session id"
    );
    assert_eq!(
        resume_target_from_argv(&argv(&["claude", "--resume", "--foo"])),
        None,
        "the claude form rejects a flag too"
    );
    assert_eq!(
        resume_target_from_argv(&argv(&["/bin/zsh"])),
        None,
        "a shell pane has no resume target"
    );
    // The detector is the DECLARED form, not a harness-name list: every
    // harness the table gives a form parses, so a live pane running one
    // keeps its row non-resumable exactly as claude/codex panes do.
    assert_eq!(
        resume_target_from_argv(&argv(&["gemini", "--resume", "gem-1234"])),
        Some("gem-1234".into()),
        "a declared flag form beyond the old two parses"
    );
    assert_eq!(
        resume_target_from_argv(&argv(&["agy", "--conversation", "agy-5678"])),
        Some("agy-5678".into()),
        "a session_flag form parses"
    );
    assert_eq!(
        resume_target_from_argv(&argv(&["gemini", "chat", "not-a-target"])),
        None,
        "an argv that never walks the form's literals parses nothing"
    );
    assert_eq!(
        resume_target_from_argv(&argv(&[
            "env",
            "FNO_AGENT_SELF=peer",
            "opencode",
            "--session",
            "oc-90ab"
        ])),
        Some("oc-90ab".into()),
        "the env(1) wrapper is skipped for every declared form"
    );

    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let pid = core.spawn_pane(24, 80, "/tmp").unwrap();
    core.session
        .add_squad(1, vec!["/tmp".into()], None, leaf_tab(1, pid));
    core.held_workers.insert(
        pid,
        HeldWorker {
            name: "live-worker".into(),
            harness: "codex".into(),
            harness_session_id: "full-session".into(),
            cwd: "/tmp".into(),
        },
    );
    let mut live = bg_row("renamed-live-worker", "/tmp", None);
    live.harness = Some("codex".into());
    live.harness_session_id = Some("full-session".into());
    core.agents = vec![live];
    let (mut conn, mut rx) = client_with_rx(1);
    conn.view = (1, 1);
    core.clients.push(conn);

    core.command(1, Command::FocusPane(pid));

    assert!(
        core.panes.contains_key(&pid),
        "the refusal keeps its named shell"
    );
    assert!(core.worker_pane.is_empty(), "no second writer is spawned");
    assert!(
        !core.held_workers.contains_key(&pid),
        "the refusal is one-shot"
    );
    assert!(
        core.panes[&pid].vt.text().contains("live elsewhere"),
        "the pane itself carries the refusal"
    );
    assert!(
        drain_notices(&mut rx).join("\n").contains("live elsewhere"),
        "the client receives the same reason"
    );
}
#[test]
fn mail_rows() {
    // AC1-ERR: MailAgent naming an absent row is refused fail-closed.
    let mut core = empty_core();
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    core.command(
        1,
        Command::MailAgent {
            name: "ghost".into(),
            text: "hi".into(),
        },
    );
    assert!(drain_notice(&mut rx).unwrap().contains("no such agent"));

    // AC3-ERR: a valid target but blank-after-sanitize text is refused (the
    // resolve succeeds, so the refusal proves the sanitize gate, not the
    // resolver, caught it - and no subprocess is reached in a plain #[test]).
    let mut core = empty_core();
    core.agents = vec![bg_row("worker", "/w", None)];
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    core.command(
        1,
        Command::MailAgent {
            name: "worker".into(),
            text: "   ".into(),
        },
    );
    assert!(drain_notice(&mut rx).unwrap().contains("empty"));

    // Control chars stripped, trimmed; blank refused; over-cap refused (never
    // truncated - Locked Decision 7).
    assert_eq!(sanitize_mail_text("  hi \x07there \n").unwrap(), "hi there");
    assert!(sanitize_mail_text("").is_err());
    assert!(sanitize_mail_text("\x07\x08 \t").is_err());
    let ok = "x".repeat(crate::proto::MAX_MAIL_TEXT);
    assert_eq!(
        sanitize_mail_text(&ok).unwrap().len(),
        crate::proto::MAX_MAIL_TEXT
    );
    assert!(sanitize_mail_text(&"x".repeat(crate::proto::MAX_MAIL_TEXT + 1)).is_err());
}
fn ext_record(
    id: &str,
    state: crate::squad_store::ExternalState,
) -> crate::squad_store::ExternalLifecycle {
    crate::squad_store::ExternalLifecycle {
        attach_id: id.into(),
        name: format!("ext-{id}"),
        cwd: "/tmp".into(),
        state,
        generation: 1,
        updated_at: String::new(),
        reason: None,
    }
}

#[test]
fn agent_rows_render_external_tombstones_by_state() {
    // A stopped record renders an EXITED external row carrying its attach_id
    // (so `x` sends RemoveExternal); a failed record renders `!exited` (so
    // `x` retries the stop). Both are external.
    use crate::squad_store::ExternalState as S;
    let mut core = empty_core();
    core.external_lifecycle = vec![
        ext_record("deadbeef", S::Stopped),
        ext_record("cafef00d", S::Failed),
    ];
    let rows = core.agent_rows();
    let stopped = rows
        .iter()
        .find(|r| r.attach_id.as_deref() == Some("deadbeef"))
        .expect("a stopped tombstone row");
    assert!(
        stopped.external && stopped.exited,
        "stopped -> exited external"
    );
    let failed = rows
        .iter()
        .find(|r| r.attach_id.as_deref() == Some("cafef00d"))
        .expect("a failed tombstone row");
    assert!(
        failed.external && !failed.exited,
        "failed -> live-ish external"
    );
}

#[test]
fn agent_rows_dedup_external_tombstone_against_live_row() {
    // A record whose attach_id is ALSO a live external roster row is skipped
    // (the live row wins) so a stop mid-flight never double-renders.
    use crate::squad_store::ExternalState as S;
    let mut core = empty_core();
    core.agents = vec![RegistryAgent {
        session_id: None,
        harness_session_id: None,
        predecessor_session_ids: Vec::new(),
        related_session_id: None,
        forked_from_session_id: None,
        external: true,
        ..bg_row("ext-live", "/tmp", Some("deadbeef"))
    }];
    core.external_lifecycle = vec![ext_record("deadbeef", S::Stopping)];
    let n = core
        .agent_rows()
        .iter()
        .filter(|r| r.attach_id.as_deref() == Some("deadbeef"))
        .count();
    assert_eq!(n, 1, "the live row wins; the record row is deduped away");
}

#[test]
fn lifecycle_name_collision_refused_fail_closed() {
    // codex review: `name` is not a unique catalog key (dedup is by
    // attach_id). When an external roster row shares a name with a registry
    // row, the command must refuse on provenance and NEVER act on the registry
    // agent the external shadows; two same-named registry rows are ambiguous.
    // Both are fail-closed refusals, so no unrelated agent is ever stopped.
    let shared = |external, exited, attach: &str| RegistryAgent {
        session_id: None,
        harness_session_id: None,
        predecessor_session_ids: Vec::new(),
        related_session_id: None,
        forked_from_session_id: None,
        external,
        exited,
        ..bg_row("dup", "/tmp", Some(attach))
    };

    // External shadows a registry row -> refuse as external, act on neither.
    let mut core = empty_core();
    core.agents = vec![
        shared(false, false, "reg00001"),
        shared(true, false, "ext00001"),
    ];
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    core.command(
        1,
        Command::StopAgent {
            name: "dup".into(),
            harness_session_id: None,
            pane_id: None,
        },
    );
    assert!(drain_notice(&mut rx).unwrap().contains("external"));

    // Two non-external rows with one name -> ambiguous refusal.
    let mut core = empty_core();
    core.agents = vec![
        shared(false, true, "reg00001"),
        shared(false, true, "reg00002"),
    ];
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    core.command(
        1,
        Command::RemoveAgent {
            name: "dup".into(),
            harness_session_id: None,
            pane_id: None,
            measure: false,
        },
    );
    assert!(drain_notice(&mut rx).unwrap().contains("ambiguous"));
}

#[test]
fn attach_reconcile_focuses_mapped_pane_no_second_tab() {
    // x-0090 AC2-HP / AC2-FR: an attach_id already mapped to a live pane
    // focuses it, never mints a second tab. The seeded (pane + map entry)
    // stands in for a successful first attach (the real spawn needs a live
    // `claude`), so the live command under test is attach #2 - and a third
    // is still a focus, covering the double-action guard.
    let (mut core, client_id, _p1, p2, _rx) = seen_test_core();
    core.agents = vec![bg_row("spawn-fix-c3d4", "/tmp/seen", Some("deadbee1"))];
    core.attached.insert("deadbee1".into(), p2);
    let panes_before = core.panes.len();

    core.command(client_id, Command::attach_agent("deadbee1"));
    assert_eq!(
        core.panes.len(),
        panes_before,
        "reconcile-focus spawns no new pane"
    );
    // The view jumped to the mapped pane's tab (p2 is tab id 2).
    assert_eq!(core.client_view(client_id), Some((1, 2)), "view follows p2");

    core.command(client_id, Command::attach_agent("deadbee1"));
    assert_eq!(
        core.panes.len(),
        panes_before,
        "a second action stays a focus - exactly one pane"
    );
}
#[test]
fn fresh_attach_unknown_target_fails_closed_before_spawn() {
    // x-3e38 AC4: an explicit target that names no live squad refuses BEFORE
    // any PTY spawn - no pane, no attach mapping, a visible reason.
    let (mut core, client_id, _p1, _p2, mut rx) = seen_test_core();
    core.agents = vec![bg_row("spawn-fix-c3d4", "/tmp/seen", Some("deadbee1"))];
    let panes_before = core.panes.len();

    core.command(
        client_id,
        Command::AttachAgent {
            id: "deadbee1".into(),
            placement: PanePlacement {
                target: PaneTarget::SquadName("ghost".into()),
                ..Default::default()
            },
        },
    );
    assert_eq!(core.panes.len(), panes_before, "no PTY spawned");
    assert!(
        !core.attached.contains_key("deadbee1"),
        "no attach mapping recorded on a refused target"
    );
    let mut saw = false;
    while let Ok(msg) = rx.try_recv() {
        if let ServerMsg::Notice { text } = msg {
            saw |= text.contains("no such workspace");
        }
    }
    assert!(saw, "the refusal names the missing workspace");
}

// -- x-9f75 open-here (PanePlacement.here) ---------------------------

/// Collect every notice text still queued on `rx`.
pub(super) fn drain_notices(rx: &mut mpsc::Receiver<ServerMsg>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let ServerMsg::Notice { text } = msg {
            out.push(text);
        }
    }
    out
}

#[test]
fn open_here_swaps_focused_viewer_and_detaches_displaced() {
    // AC1-HP: the focused viewer of session A is repointed at B - the tab's tree slot now hosts B's
    // viewer, focus is the new pane, A's viewer is reaped (A resurfaces watch-only), and B is mapped.
    set_attach_program(&["/bin/cat"]); // stand in for `claude attach`
    let (mut core, client_id, _p1, _p2, mut rx) = seen_test_core();
    let view = core.client_view(client_id).unwrap();
    let focus = core.viewed_tab(view).unwrap().focus;
    // A occupies the focused pane; B is a watch-only row to open here.
    core.attached.insert("deadbee1".into(), focus);
    core.agents = vec![bg_row("target-b", "/tmp/seen", Some("deadbee2"))];
    let new_pid = core.next_pane_id;

    core.command(client_id, Command::attach_agent_here("deadbee2"));

    let tab = core.viewed_tab(view).unwrap();
    assert_eq!(tab.root, Node::Leaf(new_pid), "slot repointed at B");
    assert_eq!(tab.focus, new_pid, "focus follows the swap");
    assert!(!core.panes.contains_key(&focus), "A's viewer pane reaped");
    assert_eq!(core.attached.get("deadbee2"), Some(&new_pid), "B mapped");
    assert!(
        !core.attached.contains_key("deadbee1"),
        "A's mapping swept - it resurfaces watch-only"
    );
    assert!(
        drain_notices(&mut rx)
            .iter()
            .any(|t| t.contains("opened here")),
        "notice names the displaced session"
    );
    core.reap_pane(new_pid); // don't leak the stand-in child
}
#[test]
fn open_here_refuses_conflicting_placement_before_spawn() {
    // AC2-ERR: `here` with a split or a non-CurrentRoute target is a
    // contradiction - refused with no spawn.
    for placement in [
        PanePlacement {
            tab: None,
            at: None,
            here: true,
            split: Some(Dir::Right),
            ..Default::default()
        },
        PanePlacement {
            tab: None,
            at: None,
            here: true,
            target: PaneTarget::SquadName("review".into()),
            ..Default::default()
        },
    ] {
        let (mut core, client_id, _p1, _p2, mut rx) = seen_test_core();
        core.agents = vec![bg_row("target-b", "/tmp/seen", Some("deadbee2"))];
        let panes_before = core.panes.len();
        core.command(
            client_id,
            Command::AttachAgent {
                id: "deadbee2".into(),
                placement,
            },
        );
        assert_eq!(core.panes.len(), panes_before, "no pane spawned");
        assert!(drain_notices(&mut rx)
            .iter()
            .any(|t| t.contains("open-here takes no split or target")));
    }
}

// The thread viewport's tab outlives its viewer: a recorded
// viewer dying in a lone-leaf tab swaps in an idle shell instead of
// deleting the one tab every reach shares, and the next reach repoints
// the stand-in in the SAME tab.

#[test]
fn attach_agent_anchored_drop_lands_beside_the_anchor_not_the_active_tab() {
    // (x-d6a8 G3) Regression for the codex P1 finding: a paneless bg row
    // dropped beside a SPECIFIC pane attaches in THAT pane's tab, beside it -
    // honoring the drop slot - rather than splitting beside the squad's
    // active-tab focus (the pre-fix place_spawned_pane behavior). The anchor
    // determines both squad and tab, overriding owner routing.
    set_attach_program(&["/bin/cat"]); // stand in for `claude attach`
    let (mut core, client_id, p1, p2, _rx) = seen_test_core();
    // View + activate the OTHER tab (id 2), so a non-anchored attach would
    // land there; the anchor (p1, tab 1) must override that.
    core.set_view(client_id, 1, 2);
    core.session.squad_mut(1).unwrap().active_tab = 1; // index 1 == tab id 2
    core.agents = vec![bg_row("bg", "/tmp/seen", Some("deadbee2"))];
    let new_pid = core.next_pane_id;

    core.command(
        client_id,
        Command::AttachAgent {
            id: "deadbee2".into(),
            placement: PanePlacement {
                at: Some(p1),
                split: Some(Dir::Right),
                ..Default::default()
            },
        },
    );

    let sq = core.session.squad(1).unwrap();
    let tab1 = sq.tabs.iter().find(|t| t.id == 1).unwrap();
    let mut ls = tree::leaves(&tab1.root);
    ls.sort_unstable();
    let mut expected = vec![p1, new_pid];
    expected.sort_unstable();
    assert_eq!(ls, expected, "attach landed beside the anchor p1 in tab 1");
    let tab2 = sq.tabs.iter().find(|t| t.id == 2).unwrap();
    assert_eq!(
        tree::leaves(&tab2.root),
        vec![p2],
        "the active/viewed tab is untouched - the drop honored its anchor"
    );
    assert_eq!(core.attached.get("deadbee2"), Some(&new_pid), "B mapped");
    core.reap_pane(new_pid); // don't leak the stand-in child
}

#[test]
fn open_here_spawn_failure_leaves_layout_untouched() {
    // AC3-ERR: the attach spawn fails - no pane is displaced, `attached` is
    // unchanged, and the sender gets `attach failed`.
    set_attach_program(&["/nonexistent/definitely-not-a-real-binary-xyz"]);
    let (mut core, client_id, _p1, _p2, mut rx) = seen_test_core();
    let view = core.client_view(client_id).unwrap();
    let focus = core.viewed_tab(view).unwrap().focus;
    core.attached.insert("deadbee1".into(), focus);
    core.agents = vec![bg_row("target-b", "/tmp/seen", Some("deadbee2"))];
    let root_before = core.viewed_tab(view).unwrap().root.clone();

    core.command(client_id, Command::attach_agent_here("deadbee2"));

    assert_eq!(
        core.viewed_tab(view).unwrap().root,
        root_before,
        "nothing displaced on spawn failure"
    );
    assert!(core.panes.contains_key(&focus), "A's viewer still live");
    assert_eq!(
        core.attached.get("deadbee1"),
        Some(&focus),
        "A's mapping untouched"
    );
    assert!(!core.attached.contains_key("deadbee2"), "B never mapped");
    assert!(drain_notices(&mut rx)
        .iter()
        .any(|t| t.contains("attach failed")));
}
#[test]
fn open_here_reads_current_focus_never_the_elsewhere_viewer() {
    // AC2-FR: the displacement guard evaluates whatever pane is focused NOW (re-resolved
    // server-side). Focus is a lone idle shell, so open-here takes it over (x-fbb1); a viewer
    // sits on ANOTHER tab and must be left completely untouched - open-here never displaces a
    // pane the operator is not looking at.
    set_attach_program(&["/bin/cat"]);
    let (mut core, client_id, p1, p2, mut rx) = seen_test_core();
    let view = core.client_view(client_id).unwrap();
    let focus = core.viewed_tab(view).unwrap().focus;
    // A viewer exists, but on the OTHER (unviewed) tab's pane; focus is a pristine idle shell.
    let other = if focus == p1 { p2 } else { p1 };
    core.panes
        .get_mut(&focus)
        .unwrap()
        .vt
        .feed(b"\x1b]133;A\x07");
    core.attached.insert("deadbee1".into(), other);
    core.agents = vec![bg_row("target-b", "/tmp/seen", Some("deadbee2"))];
    let new_pid = core.next_pane_id;

    core.command(client_id, Command::attach_agent_here("deadbee2"));

    assert!(
        !core.panes.contains_key(&focus),
        "the focused idle shell was taken over"
    );
    assert_eq!(
        core.attached.get("deadbee2"),
        Some(&new_pid),
        "B landed on the focused slot"
    );
    assert!(
        core.panes.contains_key(&other),
        "the elsewhere viewer is never touched"
    );
    assert_eq!(
        core.attached.get("deadbee1"),
        Some(&other),
        "the elsewhere viewer's mapping is intact"
    );
    assert!(drain_notices(&mut rx)
        .iter()
        .any(|t| t.contains("took over tab")));
    core.reap_pane(new_pid);
}

// -- git diff side pane ----------------------------------------------

/// A scratch dir under the temp root, keyed per test thread (one test ==
/// one thread) so parallel tests never share one. Callers drop it via
/// `remove_dir_all`; nothing outside the temp root is touched.
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "fno-diff-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch dir");
    d
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// An initialized repo with one commit, so `HEAD` resolves.
fn scratch_repo(tag: &str) -> std::path::PathBuf {
    let d = scratch_dir(tag);
    git(&d, &["init", "-q"]);
    git(&d, &["config", "user.email", "t@t"]);
    git(&d, &["config", "user.name", "t"]);
    std::fs::write(d.join("tracked.txt"), "one\n").unwrap();
    git(&d, &["add", "tracked.txt"]);
    git(&d, &["commit", "-qm", "seed"]);
    d
}

/// Run the REAL diff script with `cat` standing in for the pager, so the
/// shipped script's own behavior is what gets asserted (a pager would block
/// on a pipe and hide it).
fn run_diff_script(dir: &std::path::Path) -> String {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(diff_script("cat"))
        .current_dir(dir)
        .output()
        .expect("script runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}
#[test]
fn diff_script_unborn_head_diffs_against_the_empty_tree() {
    // AC3-ERR: `git diff HEAD` errors outright in a zero-commit repo. The
    // script falls back to the empty tree so a fresh repo shows its staged
    // content instead of a blank pane.
    let d = scratch_dir("unborn");
    git(&d, &["init", "-q"]);
    git(&d, &["config", "user.email", "t@t"]);
    git(&d, &["config", "user.name", "t"]);
    std::fs::write(d.join("first.txt"), "hello\n").unwrap();
    git(&d, &["add", "first.txt"]);

    let out = run_diff_script(&d);

    assert!(!out.trim().is_empty(), "never a blank pane");
    assert!(
        out.contains("first.txt") && out.contains("hello"),
        "the empty-tree fallback shows the staged content: {out:?}"
    );
    let _ = std::fs::remove_dir_all(&d);
}
#[test]
fn diff_renderer_chain_prefers_delta_and_falls_back_to_less() {
    // AC3-EDGE: delta is preferred when present, git-color + less when not.
    // `--paging=always` is asserted because without it delta dumps and
    // exits on a short diff, which reads as a broken pane.
    let d = scratch_dir("pathprobe");
    assert!(
        !delta_in_path(Some(d.as_os_str())),
        "an empty dir has no delta"
    );
    // A non-executable file of the right name must NOT be selected: it
    // would be picked as the renderer and then fail to exec, losing the
    // diff into a dead pipe.
    let bin = d.join("delta");
    std::fs::write(&bin, "#!/bin/sh\n").unwrap();
    assert!(
        !delta_in_path(Some(d.as_os_str())),
        "a non-executable delta is not a renderer"
    );
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        delta_in_path(Some(d.as_os_str())),
        "an executable delta on PATH is seen"
    );
    // A dangling symlink names nothing runnable either.
    let broken = scratch_dir("pathprobe-broken");
    std::os::unix::fs::symlink(broken.join("absent"), broken.join("delta")).unwrap();
    assert!(
        !delta_in_path(Some(broken.as_os_str())),
        "a dangling delta symlink is not a renderer"
    );
    let _ = std::fs::remove_dir_all(&broken);
    assert!(!delta_in_path(None), "no PATH at all is not a delta");

    assert!(diff_script("less -R").ends_with("| less -R"));
    assert!(diff_script("delta --paging=always").contains("--paging=always"));
    assert!(
        diff_script("cat").contains("color.ui=always"),
        "git colors into a pipe only when forced"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A `seen_test_core` whose single agent row points at a real repo, so the
/// toggle's cwd resolution and spawn both run for real.
///
/// The pane program is stubbed to one that exits immediately. These tests
/// are about the toggle and the layout, and the real chain ends in a pager
/// that parks on the PTY waiting for input no test will send - which hangs
/// the run rather than failing it. The script's own behavior is covered by
/// the `diff_script_*` tests, which execute it for real with `cat`.
fn diff_test_core(tag: &str) -> (Core, u64, std::path::PathBuf, mpsc::Receiver<ServerMsg>) {
    let repo = scratch_repo(tag);
    set_diff_shell("/bin/echo");
    let (mut core, client_id, _p1, _p2, rx) = seen_test_core();
    core.agents = vec![bg_row("worker", repo.to_str().unwrap(), None)];
    (core, client_id, repo, rx)
}

fn toggle_diff(core: &mut Core, client_id: u64) {
    core.command(
        client_id,
        Command::ToggleDiffPane {
            agent: Some("worker".into()),
            pane: None,
        },
    );
}

#[test]
fn diff_pane_opens_a_split_beside_the_focused_pane() {
    // AC1-HP: the toggle opens a second pane in the viewed tab, spawned in
    // the row's worktree, with the original pane still in the tree.
    let (mut core, client_id, repo, _rx) = diff_test_core("open");
    let view = core.client_view(client_id).unwrap();
    let focus = core.viewed_tab(view).unwrap().focus;
    let new_pid = core.next_pane_id;

    toggle_diff(&mut core, client_id);

    assert!(core.panes.contains_key(&new_pid), "diff pane spawned");
    assert!(core.panes.contains_key(&focus), "the source pane survives");
    assert_eq!(
        core.panes.get(&new_pid).map(|p| p.cwd.as_str()),
        Some(repo.to_str().unwrap()),
        "spawned in the row's worktree"
    );
    assert_eq!(
        core.diff_pane.as_ref().map(|(_, p)| *p),
        Some(new_pid),
        "the toggle records its pane"
    );
    let panes: Vec<u64> = tree::layout(
        &core.viewed_tab(view).unwrap().root,
        tree::Rect {
            x: 0,
            y: 0,
            rows: 24,
            cols: 80,
        },
    )
    .into_iter()
    .map(|(p, _)| p)
    .collect();
    assert!(
        panes.contains(&focus) && panes.contains(&new_pid),
        "both panes are in the tree: {panes:?}"
    );
    core.reap_pane(new_pid);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn diff_pane_second_press_closes_and_restores_the_layout() {
    // AC2-HP + AC1-FR: press-press converges to exactly the pre-press
    // layout with no diff pane left behind - a second press is always a
    // close, never a second pane.
    let (mut core, client_id, repo, _rx) = diff_test_core("toggle");
    let view = core.client_view(client_id).unwrap();
    let root_before = core.viewed_tab(view).unwrap().root.clone();
    let panes_before = core.panes.len();

    toggle_diff(&mut core, client_id);
    let opened = core.diff_pane.as_ref().map(|(_, p)| *p).expect("opened");
    toggle_diff(&mut core, client_id);

    assert!(
        core.diff_pane.is_none(),
        "no diff pane recorded after close"
    );
    assert!(!core.panes.contains_key(&opened), "its PTY is reaped");
    assert_eq!(
        core.viewed_tab(view).unwrap().root,
        root_before,
        "the layout is restored exactly"
    );
    assert_eq!(core.panes.len(), panes_before, "no pane leaked");

    // A third press opens again - the toggle never wedges closed.
    toggle_diff(&mut core, client_id);
    let reopened = core.diff_pane.as_ref().map(|(_, p)| *p).expect("reopened");
    assert_ne!(reopened, opened, "a fresh pane, not the old id");
    core.reap_pane(reopened);
    let _ = std::fs::remove_dir_all(&repo);
}
#[test]
fn diff_pane_refuses_a_reaped_worktree_before_spawn() {
    // A spawn silently ignores a cwd that is not a directory and lands in
    // the server's own cwd - which would render some OTHER repo's diff
    // under this row's name. Refuse instead, visibly.
    let (mut core, client_id, repo, mut rx) = diff_test_core("gone");
    let _ = std::fs::remove_dir_all(&repo);
    let panes_before = core.panes.len();

    toggle_diff(&mut core, client_id);

    assert_eq!(core.panes.len(), panes_before, "nothing spawned");
    assert!(core.diff_pane.is_none());
    assert!(drain_notices(&mut rx)
        .iter()
        .any(|t| t.contains("worktree is gone")));
}

#[test]
fn diff_pane_unknown_row_says_so_rather_than_diffing_something_else() {
    // AC1-UI: a press that cannot resolve a worktree still produces
    // feedback. A silent no-op reads as a dead keybind.
    let (mut core, client_id, _p1, _p2, mut rx) = seen_test_core();
    let panes_before = core.panes.len();

    core.command(
        client_id,
        Command::ToggleDiffPane {
            agent: Some("nobody-here".into()),
            pane: None,
        },
    );

    assert_eq!(core.panes.len(), panes_before, "nothing spawned");
    assert!(drain_notices(&mut rx)
        .iter()
        .any(|t| t.contains("no worktree to diff")));
}
#[test]
fn diff_pane_refuses_a_name_two_rows_share() {
    // Registry names are not unique. Resolving to the first match would
    // render one worker's worktree under another worker's row - a wrong
    // answer the operator has no way to spot. Refuse instead.
    let repo_a = scratch_repo("dupe-a");
    let repo_b = scratch_repo("dupe-b");
    set_diff_shell("/bin/echo");
    let (mut core, client_id, _p1, _p2, mut rx) = seen_test_core();
    core.agents = vec![
        bg_row("twin", repo_a.to_str().unwrap(), None),
        bg_row("twin", repo_b.to_str().unwrap(), None),
    ];
    let panes_before = core.panes.len();

    core.command(
        client_id,
        Command::ToggleDiffPane {
            agent: Some("twin".into()),
            pane: None,
        },
    );

    assert_eq!(core.panes.len(), panes_before, "nothing spawned");
    assert!(core.diff_pane.is_none());
    assert!(
        drain_notices(&mut rx)
            .iter()
            .any(|t| t.contains("more than one row")),
        "the ambiguity is named, not silently resolved"
    );
    let _ = std::fs::remove_dir_all(&repo_a);
    let _ = std::fs::remove_dir_all(&repo_b);
}
#[test]
fn diff_pane_says_so_when_the_tab_dies_under_the_toggle() {
    // AC1-UI, the path that is easiest to leave silent: switching source
    // closes the old pane first, and if that pane was the tab's last one
    // the tab retires and the view re-anchors - so the reopen has no tab
    // to split. The press must still say something.
    let (mut core, client_id, repo_a, mut rx) = diff_test_core("tabdies");
    let repo_b = scratch_repo("tabdies-b");
    core.agents
        .push(bg_row("worker-b", repo_b.to_str().unwrap(), None));
    let view = core.client_view(client_id).unwrap();

    toggle_diff(&mut core, client_id);
    let first = core.diff_pane.as_ref().map(|(_, p)| *p).expect("first");
    // Close everything else in the tab, leaving the diff pane alone in it.
    let others: Vec<u64> = tree::layout(&core.viewed_tab(view).unwrap().root, vp_of(&core, view))
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| *p != first)
        .collect();
    for p in others {
        core.close_pane(p);
    }
    let _ = drain_notices(&mut rx);

    core.command(
        client_id,
        Command::ToggleDiffPane {
            agent: Some("worker-b".into()),
            pane: None,
        },
    );

    assert!(
        drain_notices(&mut rx)
            .iter()
            .any(|t| t.contains("diff pane")),
        "a press that cannot land still reports"
    );
    assert!(
        core.diff_pane.is_none(),
        "no phantom record for a pane that never landed"
    );
    let _ = std::fs::remove_dir_all(&repo_a);
    let _ = std::fs::remove_dir_all(&repo_b);
}

/// The viewport the core would tile `view`'s tab into.
fn vp_of(core: &Core, view: (u64, TabId)) -> tree::Rect {
    core.tab_rect(view.1)
}

#[test]
fn diff_pane_refuses_a_split_too_narrow_to_fit() {
    // AC2-EDGE: below the tree's minimum the split is refused pre-spawn,
    // the layout is untouched, and a notice says why. Silence here reads
    // as a dead keybind.
    let (mut core, client_id, repo, mut rx) = diff_test_core("narrow");
    let view = core.client_view(client_id).unwrap();
    // Clamp the viewed tab to a width two panes cannot share.
    for c in core.clients.iter_mut().filter(|c| c.view.1 == view.1) {
        c.dims = (24, tree::MIN_COLS);
    }
    let root_before = core.viewed_tab(view).unwrap().root.clone();
    let panes_before = core.panes.len();

    toggle_diff(&mut core, client_id);

    assert_eq!(
        core.viewed_tab(view).unwrap().root,
        root_before,
        "layout untouched by a refused split"
    );
    assert_eq!(
        core.panes.len(),
        panes_before,
        "the pre-spawned pane is reaped"
    );
    assert!(core.diff_pane.is_none());
    assert!(
        drain_notices(&mut rx)
            .iter()
            .any(|t| t.contains("split refused")),
        "the refusal is visible"
    );
    let _ = std::fs::remove_dir_all(&repo);
}
#[test]
fn detached_live_worker_row_is_paneless_but_not_exited() {
    let (mut core, _client_id, p1, _p2, _rx) = seen_test_core();
    let mut worker = agent_in("test", p1, Some(AgentBadge::Working), false);
    worker.name = "detached-worker".into();
    worker.harness = Some("codex".into());
    worker.harness_session_id = Some("session-detached".into());
    core.agents = vec![worker];

    core.detach_worker_pane(p1).unwrap();

    let row = core
        .agent_rows()
        .into_iter()
        .find(|row| row.name == "detached-worker")
        .expect("detached worker remains visible");
    assert_eq!(row.pane_id, None, "detached worker is paneless");
    assert!(!row.exited, "detached worker is still alive");
    assert!(
        core.panes.contains_key(&p1),
        "detachment did not reap the PTY"
    );
    assert!(
        core.session.find_pane(p1).is_none(),
        "detachment removed the pane from every visible tree"
    );

    core.reap_pane(p1);
}
#[test]
fn detached_child_exit_tombstones_member_and_clears_roster() {
    let (mut core, _client_id, p1, _p2, _rx) = seen_test_core();
    let mut worker = agent_in("test", p1, Some(AgentBadge::Working), false);
    worker.name = "detached-worker".into();
    worker.harness = Some("codex".into());
    worker.harness_session_id = Some("session-detached".into());
    core.agents = vec![worker];
    core.detach_worker_pane(p1).unwrap();
    let child = core.panes[&p1].pty.child_pid().unwrap();
    assert_eq!(
        unsafe { libc::kill(child as libc::pid_t, libc::SIGKILL) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !core.panes[&p1].pty.is_reap_ready() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        core.panes[&p1].pty.is_reap_ready(),
        "child exit was observed"
    );

    assert!(matches!(core.reap_dead_children(vec![p1]), Flow::Continue));
    assert!(!core.panes.contains_key(&p1));
    assert!(!core.detached_panes.contains_key(&p1));
    let member = core.squad_members[&1]
        .iter()
        .find(|member| member.worker.as_deref() == Some("detached-worker"))
        .unwrap();
    assert!(
        member.tombstone,
        "natural detached exit leaves a dead marker"
    );
    assert!(!member.detached, "dead member is no longer detached");

    for pane in core.panes.keys().copied().collect::<Vec<_>>() {
        core.reap_pane(pane);
    }
}

#[test]
fn reap_sweeps_attach_map_and_row_reverts_to_watch_only() {
    // x-0090 AC1-FR: killing the mapped pane sweeps the map (eager) AND the
    // lazy `panes` liveness check reverts the row to watch-only attachable,
    // so a next click re-attaches into a fresh pane rather than a corpse.
    let (mut core, _client_id, _p1, p2, _rx) = seen_test_core();
    core.agents = vec![bg_row("spawn-fix-c3d4", "/tmp/seen", Some("deadbee1"))];
    core.attached.insert("deadbee1".into(), p2);

    core.reap_pane(p2);
    assert!(
        !core.attached.values().any(|&p| p == p2),
        "eager sweep drops the dead pane's mapping"
    );
    let rows = core.agent_rows();
    let row = rows.iter().find(|r| r.name == "spawn-fix-c3d4").unwrap();
    assert_eq!(row.pane_id, None, "reverts to watch-only");
    assert_eq!(
        row.attach_id.as_deref(),
        Some("deadbee1"),
        "re-attachable after the pane dies"
    );
}

#[test]
fn agent_rows_union_covers_merged_bare_and_watch_only() {
    // x-0090 US2: agent_rows() is a pane union. seen_test_core gives squad 1
    // with p1 in tab 1 and p2 in tab 2 (both bare `/bin/cat` panes). Enrich
    // p1 from the registry, leave p2 bare, add one paneless watch-only row.
    let (mut core, _c, p1, p2, _rx) = seen_test_core();
    core.agents = vec![
        agent_in("test", p1, Some(AgentBadge::Working), false),
        bg_row("spawn-fix-c3d4", "/elsewhere", Some("deadbee1")),
    ];
    let rows = core.agent_rows();
    // Pane rows first in (tab, pane) order, watch-only appended last.
    assert_eq!(rows.len(), 3, "two pane rows + one watch-only");

    // Merged row: named + badged from the registry, carries its tab ref.
    assert_eq!(rows[0].pane_id, Some(p1));
    assert_eq!(rows[0].name, "w", "registry name wins on a merged row");
    assert_eq!(rows[0].badge, Some(AgentBadge::Working));
    assert_eq!(rows[0].tab, Some(1));
    assert_eq!(rows[0].attach_id, None, "a pane row never carries attach");

    // Bare pane: labelled from its own entry (cwd basename here), no badge.
    assert_eq!(rows[1].pane_id, Some(p2));
    assert_eq!(rows[1].name, "seen", "bare pane labelled from PaneEntry");
    assert_eq!(rows[1].badge, None);
    assert_eq!(rows[1].tab, Some(2));

    // Watch-only appended last: paneless, orphan squad, still attachable.
    assert_eq!(rows[2].pane_id, None, "watch-only has no pane");
    assert_eq!(rows[2].name, "spawn-fix-c3d4");
    assert_eq!(rows[2].squad, None, "cwd matches no squad -> orphan");
    assert_eq!(rows[2].attach_id.as_deref(), Some("deadbee1"));
    assert_eq!(rows[2].tab, None);
}
#[test]
fn node_token_matches_whole_ids_only() {
    // Locked 6: exact node-id token, non-alphanumeric boundaries. `-` is
    // part of the id shape, so it cannot be the boundary test.
    assert!(name_has_node_token("x-54fa", "x-54fa"));
    assert!(name_has_node_token("tgt-x-54fa", "x-54fa"));
    assert!(name_has_node_token("run x-54fa now", "x-54fa"));
    assert!(name_has_node_token("x-54fa.retry", "x-54fa"));
    // Prefix/suffix of a longer token never matches.
    assert!(!name_has_node_token("x-54fab", "x-54fa"));
    assert!(!name_has_node_token("ax-54fa", "x-54fa"));
    assert!(!name_has_node_token("x-54fa", "x-54f"));
    // Second occurrence with clean boundaries still matches.
    assert!(name_has_node_token("x-54fab x-54fa", "x-54fa"));
    assert!(!name_has_node_token("anything", ""));
    // A node whose FIRST char is multi-byte (a non-ASCII `id_prefix` is
    // legal config) must not panic when a rejected match forces the scan
    // to advance - `start + 1` would land inside the char (gemini review
    // of PR #211). Both the advance-then-match and the pure-reject walk.
    assert!(name_has_node_token(
        "a\u{3093}-54fa \u{3093}-54fa",
        "\u{3093}-54fa"
    ));
    assert!(!name_has_node_token("a\u{3093}-54fa", "\u{3093}-54fa"));
}
/// A paneless registry row for the routing tests: `name`/`cwd`/`attach_id`
/// are the join surfaces; everything else is the quiet default.
pub(super) fn bg_row(name: &str, cwd: &str, attach: Option<&str>) -> RegistryAgent {
    RegistryAgent {
        model: None,
        route: None,
        spawned_by_session: None,
        session_id: None,
        harness_session_id: None,
        predecessor_session_ids: Vec::new(),
        related_session_id: None,
        forked_from_session_id: None,
        name: name.into(),
        cwd: cwd.into(),
        exited: false,
        dnd: false,
        liveness: agents_view::Liveness::Alive,
        badge: None,
        reason: None,
        mux: None,
        answerable: None,
        attach_id: attach.map(str::to_owned),
        external: false,
        account: None,
        claude_session_uuid: None,
        log_path: None,
        updated_at: None,
        role_level: None,
        role_scope: None,
        harness: None,
        ..Default::default()
    }
}

#[test]
fn subline_from_joins_branch_and_tail_and_degrades() {
    // Both present -> "branch · tail".
    assert_eq!(
        subline_from(Some("main"), "/code/footnote"),
        Some("main · footnote".into())
    );
    // Branch unresolved -> tail alone (AC1-ERR degradation).
    assert_eq!(
        subline_from(None, "/code/footnote"),
        Some("footnote".into())
    );
    // Trailing slash is trimmed before taking the tail.
    assert_eq!(
        subline_from(None, "/code/footnote/"),
        Some("footnote".into())
    );
    // No cwd -> no subline (AC1-EDGE: no sub-row emitted).
    assert_eq!(subline_from(None, ""), None);
    assert_eq!(subline_from(Some("main"), ""), Some("main".into()));
}
#[test]
fn agent_rows_composes_subline_from_branch_map() {
    // A paneless orphan row joins the off-loop branch map on its cwd; a cwd
    // absent from the map degrades to the tail alone (US4 wire composition).
    let mut core = empty_core();
    core.agents = vec![
        bg_row("worker", "/tmp/repos/footnote", Some("j1")),
        bg_row("other", "/tmp/repos/regready", Some("j2")),
    ];
    core.branch_by_cwd = [("/tmp/repos/footnote".to_string(), "main".to_string())]
        .into_iter()
        .collect();
    let rows = core.agent_rows();
    let footnote = rows.iter().find(|r| r.name == "worker").unwrap();
    assert_eq!(footnote.subline.as_deref(), Some("main · footnote"));
    // (x-6851 US3) Every row now carries its cwd basename on the wire.
    assert_eq!(footnote.cwd_base.as_deref(), Some("footnote"));
    let regready = rows.iter().find(|r| r.name == "other").unwrap();
    assert_eq!(
        regready.subline.as_deref(),
        Some("regready"),
        "no branch in map -> tail only"
    );
    assert_eq!(regready.cwd_base.as_deref(), Some("regready"));
}

#[test]
fn subline_with_title_joins_the_harness_title_beside_the_label() {
    // A row whose harness title differs from its label
    // renders the title in the subline slot, joined onto the branch
    // subline; a title that EQUALS the label renders nothing (the label
    // already says it), and no title falls back to the base subline.
    let mut a = bg_row("w1", "/tmp/repos/footnote", Some("j1"));
    a.harness_title = Some("lead-title".into());
    assert_eq!(
        subline_with_title(&a, Some("main · footnote".into())),
        Some("lead-title · main · footnote".into())
    );
    assert_eq!(
        subline_with_title(&a, None),
        Some("lead-title".into()),
        "no base subline -> title alone"
    );
    a.harness_title = Some("w1".into());
    assert_eq!(
        subline_with_title(&a, Some("main · footnote".into())),
        Some("main · footnote".into()),
        "title == label -> base subline untouched"
    );
    a.harness_title = None;
    assert_eq!(
        subline_with_title(&a, None),
        None,
        "no title, no base -> none"
    );
}
// (x-0f42 external-lifecycle sync and x-54fa routed-backlog families) moved verbatim into its own module: this file is over the
// shrink-only line, and test motion is the sanctioned shrink.
#[path = "server/tests/external_lifecycle_and_backlog_tests.rs"]
mod external_lifecycle_and_backlog_tests;

#[path = "server/tests/row_set_tests.rs"]
mod row_set_tests;

#[path = "server/tests/agent_launcher_tests.rs"]
mod agent_launcher_tests;
#[test]
fn idle_shell_takeover_verdicts() {
    // Take-over: the tab's only pane is a plain, pristine idle shell (the reported bug).
    assert!(idle_shell_takeover(1, None, true));
    // Refuse: the shell has run or started something (foreground program, exec, or a
    // background/stopped job) - not pristine, so `.` must not reap it.
    assert!(!idle_shell_takeover(1, None, false));
    // Refuse: more than one leaf - `.` is scoped to the "only pane" case; splits use h/j/k/l.
    assert!(!idle_shell_takeover(2, None, true));
    // Refuse: an agent / `pane run` pane (cmd set) is never a disposable shell.
    assert!(!idle_shell_takeover(1, Some("claude attach ab12"), true));
}

/// A scratch store dir for write-through tests, installed via the per-thread
/// path override so no test mutates the shared environment (no env race).
struct StoreScratch {
    dir: std::path::PathBuf,
}
impl StoreScratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fno-srv-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::squad_store::set_test_path(&dir);
        StoreScratch { dir }
    }
}
impl Drop for StoreScratch {
    fn drop(&mut self) {
        crate::squad_store::clear_test_path();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn named_member_squad(core: &mut Core, sid: u64, name: &str, pid: u64, attach: &str) {
    core.session.add_squad(
        sid,
        vec!["/repo".into()],
        Some(name.into()),
        Tab {
            name: None,
            id: sid,
            root: Node::Leaf(pid),
            focus: pid,
        },
    );
    core.squad_members.insert(
        sid,
        vec![crate::squad_store::StoredMember {
            attach_id: attach.into(),
            tombstone: false,
            tombstone_reason: None,
            detached: false,
            tab_name: None,
            cwd: None,
            worker: None,
            harness: None,
            harness_session_id: None,
            pane_id: None,
        }],
    );
    core.attached.insert(attach.into(), pid);
}

fn stored_member(id: &str, tombstone: bool) -> crate::squad_store::StoredMember {
    crate::squad_store::StoredMember {
        attach_id: id.into(),
        tombstone,
        tombstone_reason: None,
        detached: false,
        tab_name: None,
        cwd: None,
        worker: None,
        harness: None,
        harness_session_id: None,
        pane_id: None,
    }
}

#[test]
fn live_ids_from_marks_live_registry_and_roster_rows() {
    // AC1-HP hinges on a FRESH liveness read at first attach (self.agents is
    // still empty then). Pure over the raw file contents: an exited registry
    // row is dead, a live one and a roster worker are live.
    let reg = r#"{"agents":[
            {"name":"w","cwd":"/x","status":"live","provider":"claude","short_id":"c19cd2c3"},
            {"name":"d","cwd":"/x","status":"exited","provider":"claude","short_id":"deadbeef"}
        ]}"#;
    let roster = r#"{"workers":{"k":{"sessionId":"aa11bb22-xyz","cwd":"/y"}}}"#;
    let live = live_ids_from(Some(reg), Some(roster), 0);
    assert!(live.contains("c19cd2c3"), "a live registry row is live");
    assert!(!live.contains("deadbeef"), "an exited row is not live");
    assert!(
        live.contains("aa11bb22"),
        "a roster worker's short_id is live"
    );
    // Missing files (None) yield an empty live set.
    assert!(live_ids_from(None, None, 0).is_empty());
}
#[test]
fn picker_attach_spawn_failure_persists_no_member() {
    // AC1-ERR: a fresh attach whose spawn fails records no mapping AND no
    // member - the row stays watch-only, recoverable on retry, and no phantom
    // is written to the store.
    set_attach_program(&["/nonexistent/definitely-not-a-real-binary-xyz"]);
    let (mut core, client_id, _p1, _p2, _rx) = seen_test_core();
    core.agents = vec![bg_row("bg", "/tmp/seen", Some("deadbee2"))];

    core.command(client_id, Command::attach_agent("deadbee2"));

    assert!(
        !core.attached.contains_key("deadbee2"),
        "no mapping on spawn failure"
    );
    assert!(
        core.squad_members
            .get(&1)
            .is_none_or(|m| m.iter().all(|x| x.attach_id != "deadbee2")),
        "no phantom member persisted"
    );
    let loaded = crate::squad_store::load();
    assert!(
        loaded
            .squads
            .iter()
            .all(|s| s.members.iter().all(|m| m.attach_id != "deadbee2")),
        "nothing written to the store"
    );
}
// ---- (x-d285) mux gestures through the canonical re-entry plan -------

/// A verdict shaped like the resolver's output for a routed claude row.
/// `FNO_ACCOUNT` is the positive marker: only the verdict's env prefix
/// sets it on an attach pane.
fn staged_reentry_verdict() -> ReentryVerdict {
    ReentryVerdict {
        argv: vec!["/bin/cat".into(), "deadbee1".into()],
        env: vec![
            "FNO_ACCOUNT=makers".into(),
            "CLAUDE_CONFIG_DIR=/acct/makers/cfg".into(),
        ],
        config_dir: Some("/acct/makers/cfg".into()),
        plan_ms: 0,
    }
}

fn claude_bg_row(name: &str, cwd: &str, attach: Option<&str>) -> RegistryAgent {
    let mut row = bg_row(name, cwd, attach);
    row.harness = Some("claude".into());
    row
}

#[test]
fn reentry_verdict_parse_accepts_only_a_resolved_plan() {
    // The machine grammar: a resolved verdict with argv + env parses; an
    // unresolved one, malformed JSON, a missing argv, or an empty argv is
    // the same refusal - nothing may spawn off it.
    let ok = br#"{"resolved":true,"argv":["claude","attach","deadbee1"],"env":{"FNO_ACCOUNT":"makers"},"claude_config_dir":"/acct/cfg"}"#;
    let verdict = ReentryVerdict::from_plan_json(ok).expect("a resolved plan parses");
    assert_eq!(verdict.argv, vec!["claude", "attach", "deadbee1"]);
    assert_eq!(verdict.env, vec!["FNO_ACCOUNT=makers"]);
    assert_eq!(
        verdict.config_dir.as_deref(),
        Some(std::path::Path::new("/acct/cfg"))
    );
    assert_eq!(
        verdict.prefixed_argv(),
        vec!["env", "FNO_ACCOUNT=makers", "claude", "attach", "deadbee1"],
        "the env assignments prefix the provider argv"
    );
    for bad in [
        br#"{"resolved":false}"#.as_slice(),
        br#"{"argv":["claude"]}"#.as_slice(),
        br#"not json"#.as_slice(),
        br#"{"resolved":true,"argv":[]}"#.as_slice(),
        // A non-string env value parsed as an empty assignment would
        // launch with a blanked namespace; it refuses like the rest.
        br#"{"resolved":true,"argv":["claude"],"env":{"CLAUDE_CONFIG_DIR":5}}"#.as_slice(),
    ] {
        assert!(
            ReentryVerdict::from_plan_json(bad).is_err(),
            "refused: {}",
            String::from_utf8_lossy(bad)
        );
    }
    // No env means the argv runs bare, with no env(1) prefix.
    let bare = br#"{"resolved":true,"argv":["claude"]}"#;
    assert_eq!(
        ReentryVerdict::from_plan_json(bare)
            .unwrap()
            .prefixed_argv(),
        vec!["claude"]
    );
}
#[test]
fn attach_gesture_refusal_is_a_notice_and_starts_no_pane() {
    // The resolver's refusal routes back as a one-line notice; no pane,
    // no mapping, and no staged verdict leaks into a later gesture.
    let (mut core, client_id, _p1, _p2, mut rx) = seen_test_core();
    core.agents = vec![claude_bg_row("routed-glm", "/tmp/seen", Some("deadbee1"))];
    let panes_before = core.panes.len();

    core.handle(CoreMsg::ReentryPlanReady {
        id: client_id,
        request: Box::new(ReentrySpawnRequest::Attach {
            attach_id: "deadbee1".into(),
            placement: PanePlacement::default(),
        }),
        verdict: Err("route settings missing: floor-only file".into()),
    });

    assert_eq!(core.panes.len(), panes_before, "no pane started");
    assert!(!core.attached.contains_key("deadbee1"));
    assert!(core.reentry_verdict.is_none());
    let notices = drain_notices(&mut rx).join("\n");
    assert!(
        notices.contains("floor-only file"),
        "the refusal names its reason: {notices}"
    );
}
/// Run the bulk-restore apply half synchronously and return its rows.
/// The production claude-plan split (WorkspaceRestore -> off-loop
/// resolution -> apply) is exercised by the handler split itself; these
/// tests drive the apply half directly so the gates, rows and rerun
/// semantics are deterministic.
#[test]
fn recruit_consumes_staged_batch_plans() {
    // The picker is N spawns under one gesture: every claude id's plan
    // arrives keyed by attach id. A verdict spawns with its account; a
    // refusal skips the id with the reason named. (The batch fire itself
    // needs a runtime; this stages the replay state directly.)
    set_attach_program(&["/bin/cat"]);
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec!["/x".into()],
        None,
        Tab {
            name: None,
            id: 5,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    core.agents = vec![
        claude_bg_row("routed-a", "/x", Some("deadbee1")),
        claude_bg_row("routed-b", "/x", Some("deadbee2")),
    ];
    core.batch_plans
        .insert("deadbee1".into(), Ok(staged_reentry_verdict()));
    core.batch_plans.insert(
        "deadbee2".into(),
        Err("row records no launch account".into()),
    );
    let new_pid = core.next_pane_id;

    core.command(
        1,
        Command::RecruitAgents {
            squad: "team".into(),
            ids: vec!["deadbee1".into(), "deadbee2".into()],
        },
    );

    assert_eq!(
        core.panes[&new_pid].account.as_deref(),
        Some("makers"),
        "the planned id spawned with its verdict account"
    );
    assert!(
        !core.attached.contains_key("deadbee2"),
        "the refused id started no pane"
    );
    let notices = drain_notices(&mut rx).join("\n");
    assert!(
        notices.contains("no launch account"),
        "the refusal is visible in the recruit report: {notices}"
    );
    core.reap_pane(new_pid);
}
#[test]
fn recruit_refuses_blank_name_and_empty_ids() {
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec!["/x".into()],
        None,
        Tab {
            name: None,
            id: 5,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RecruitAgents {
            squad: "  ".into(),
            ids: vec!["c19cd2c3".into()],
        },
    );
    core.command(
        1,
        Command::RecruitAgents {
            squad: "team".into(),
            ids: vec![],
        },
    );
    assert_eq!(
        core.session.squads.len(),
        1,
        "no workspace created on refusal"
    );
    assert!(core.squad_members.is_empty());
}
#[test]
fn recruit_refuses_a_name_persisted_but_not_live() {
    // codex P2: recruiting into a name that exists only in the store (another
    // server created it, or restore skipped it) must NOT create a new live
    // squad - that would upsert by name and drop the persisted members.
    let _s = StoreScratch::new("recruit-persisted");
    crate::squad_store::upsert(
        "ghost",
        "",
        &["/repo".into()],
        &[stored_member("c19cd2c3", false)],
    )
    .unwrap();
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec!["/x".into()],
        None,
        Tab {
            name: None,
            id: 5,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RecruitAgents {
            squad: "ghost".into(),
            ids: vec!["deadbeef".into()],
        },
    );
    // The persisted entry is untouched; no live squad was minted for it.
    let loaded = crate::squad_store::load();
    assert_eq!(loaded.squads.len(), 1);
    assert_eq!(
        loaded.squads[0].members,
        vec![stored_member("c19cd2c3", false)],
        "the persisted members are not clobbered"
    );
    assert!(
        !core
            .session
            .squads
            .iter()
            .any(|s| s.name.as_deref() == Some("ghost")),
        "no live squad created for the persisted name"
    );
}

#[test]
fn renaming_a_members_tab_persists_the_tab_name() {
    // x-0f9d US4: renaming the tab hosting a persisted member writes the
    // chosen name into the store, re-derived at persist time, so a restart
    // can restore the tab named (AC1-HP persistence half).
    let _s = StoreScratch::new("tab-name-persist");
    let mut core = empty_core();
    // A live named squad whose member's pane (1) is the leaf of tab id 1.
    named_member_squad(&mut core, 1, "harden", 1, "c19cd2c3");
    core.clients.push(client(1, 1, (24, 80), false));

    // Unnamed first: the store carries no tab name.
    core.persist_squad(1);
    let loaded = crate::squad_store::load();
    assert_eq!(
        loaded.squads[0].members[0].tab_name, None,
        "unnamed -> None"
    );

    // Rename the hosting tab; the rename handler persists it.
    core.command(
        1,
        Command::RenameTab {
            tab: 1,
            name: "reviews".into(),
        },
    );
    let loaded = crate::squad_store::load();
    assert_eq!(
        loaded.squads[0].members[0].tab_name.as_deref(),
        Some("reviews"),
        "the chosen tab name is persisted for restore"
    );

    // Clearing the name (blank rename) drops it from the store too.
    // Re-register the sender: the prior rename's layout push reaped the
    // test client's dropped receiver (as in rename_tab_round_trips).
    core.clients.push(client(1, 1, (24, 80), false));
    core.command(
        1,
        Command::RenameTab {
            tab: 1,
            name: "".into(),
        },
    );
    let loaded = crate::squad_store::load();
    assert_eq!(
        loaded.squads[0].members[0].tab_name, None,
        "clearing the name clears the stored tab_name"
    );
}
#[test]
fn squad_rename_after_tab_rename_keeps_the_tab_name() {
    // x-0f9d US4 / codex review: persist_squad refreshes the AUTHORITATIVE
    // in-memory member list, so a later RenameSquad (which persists that
    // list verbatim through squad_store::rename) does not erase a
    // freshly-renamed tab name.
    let _s = StoreScratch::new("tab-name-survives-squad-rename");
    let mut core = empty_core();
    named_member_squad(&mut core, 1, "harden", 1, "c19cd2c3");
    core.clients.push(client(1, 1, (24, 80), false));

    // Name the tab (persists tab_name AND refreshes squad_members in place).
    core.command(
        1,
        Command::RenameTab {
            tab: 1,
            name: "reviews".into(),
        },
    );
    // Rename the squad; it writes the in-memory member list to the store.
    core.clients.push(client(1, 1, (24, 80), false));
    core.command(
        1,
        Command::RenameSquad {
            squad: 1,
            name: "hardened".into(),
        },
    );

    let loaded = crate::squad_store::load();
    let sq = loaded
        .squads
        .iter()
        .find(|s| s.name == "hardened")
        .expect("renamed squad persisted");
    assert_eq!(
        sq.members[0].tab_name.as_deref(),
        Some("reviews"),
        "the tab name survives the squad rename"
    );
}

#[test]
fn dismiss_member_removes_a_tombstone_and_refuses_unknown() {
    let _s = StoreScratch::new("dismiss");
    let mut core = empty_core();
    core.session.add_squad(
        7,
        vec!["/repo".into()],
        Some("harden".into()),
        Tab {
            name: None,
            id: 5,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.squad_members.insert(
        7,
        vec![
            stored_member("c19cd2c3", true),
            stored_member("deadbeef", false),
        ],
    );
    core.persist_squad(7);
    core.clients.push(client(1, 5, (24, 80), false));
    // Dismiss the live (non-tombstone) member: refused, nothing removed.
    core.command(
        1,
        Command::DismissMember {
            squad: 7,
            attach_id: "deadbeef".into(),
        },
    );
    assert_eq!(
        core.squad_members[&7].len(),
        2,
        "a live member is not dismissable"
    );
    // Dismiss the tombstone: removed + persisted.
    core.command(
        1,
        Command::DismissMember {
            squad: 7,
            attach_id: "c19cd2c3".into(),
        },
    );
    assert_eq!(core.squad_members[&7].len(), 1);
    let loaded = crate::squad_store::load();
    assert_eq!(
        loaded.squads[0].members,
        vec![stored_member("deadbeef", false)]
    );
    // An unknown workspace is refused.
    core.command(
        1,
        Command::DismissMember {
            squad: 999,
            attach_id: "c19cd2c3".into(),
        },
    );
}
#[test]
fn record_worker_member_keeps_repeated_names_as_distinct_members() {
    let _s = StoreScratch::new("record-worker-repeated-name");
    let mut core = empty_core();
    core.session.add_squad(
        7,
        vec!["/repo".into()],
        Some("workers".into()),
        leaf_tab(7, 100),
    );
    let mut first = bg_row("reused-name", "/repo", None);
    first.harness = Some("codex".into());
    first.harness_session_id = Some("session-one".into());
    let mut second = first.clone();
    second.harness = Some("claude".into());
    second.harness_session_id = Some("session-two".into());
    core.agents = vec![first, second];

    core.record_worker_member(7, "reused-name", 100, "/repo", Some("session-one"));
    core.record_worker_member(7, "reused-name", 101, "/repo", Some("session-two"));

    let members = &core.squad_members[&7];
    assert_eq!(members.len(), 2);
    assert!(members.iter().any(|member| {
        member.harness.as_deref() == Some("codex")
            && member.harness_session_id.as_deref() == Some("session-one")
    }));
    assert!(members.iter().any(|member| {
        member.harness.as_deref() == Some("claude")
            && member.harness_session_id.as_deref() == Some("session-two")
    }));
}

#[test]
fn record_worker_member_does_not_collapse_ambiguous_registry_names() {
    let _s = StoreScratch::new("record-worker-ambiguous-name");
    let mut core = empty_core();
    core.session.add_squad(
        7,
        vec!["/repo".into()],
        Some("workers".into()),
        leaf_tab(7, 100),
    );
    let first = bg_row("reused-name", "/repo", None);
    let mut second = first.clone();
    second.harness = Some("claude".into());
    second.harness_session_id = Some("session-two".into());
    core.agents = vec![first, second];

    core.record_worker_member(7, "reused-name", 100, "/repo", None);
    core.record_worker_member(7, "reused-name", 101, "/repo", None);

    assert_eq!(core.squad_members[&7].len(), 2);
}

#[test]
fn persist_squad_does_not_copy_one_identity_to_repeated_name_members() {
    let _s = StoreScratch::new("persist-worker-repeated-name");
    let mut core = empty_core();
    core.session.add_squad(
        7,
        vec!["/repo".into()],
        Some("workers".into()),
        leaf_tab(7, 100),
    );
    core.squad_members.insert(
        7,
        vec![
            crate::squad_store::StoredMember {
                attach_id: String::new(),
                tombstone: false,
                tombstone_reason: None,
                detached: false,
                tab_name: None,
                cwd: None,
                worker: Some("reused-name".into()),
                harness: None,
                harness_session_id: None,
                pane_id: None,
            },
            crate::squad_store::StoredMember {
                attach_id: String::new(),
                tombstone: false,
                tombstone_reason: None,
                detached: false,
                tab_name: None,
                cwd: None,
                worker: Some("reused-name".into()),
                harness: None,
                harness_session_id: None,
                pane_id: None,
            },
        ],
    );
    let mut row = bg_row("reused-name", "/repo", None);
    row.harness = Some("codex".into());
    row.harness_session_id = Some("session-one".into());
    core.agents = vec![row];

    core.persist_squad(7);

    let members = &crate::squad_store::load().squads[0].members;
    assert!(members
        .iter()
        .all(|member| member.harness.is_none() && member.harness_session_id.is_none()));
}
#[test]
fn resumed_pane_resolves_fno_id_from_its_resume_birthright() {
    // (x-b029) AC3-HP: a pane the daemon re-homed through the resume path
    // resolves its fno_id from the (harness, session) record the resume
    // stamped at birth, even though the registry FILE's row still points
    // at the pre-restart pane. The record maps to the row's OWN id - the
    // pane never reports the harness session id when they differ.
    let mut core = empty_core();
    let uuid = "01a05fce-0000-7ccc-8000-000000000000";
    let own_id = "0f6a4b2e-1111-4222-8333-444444444444";
    let mut row = bg_row("t-resumed-agy", "/repo", None);
    row.harness = Some("codex".into());
    row.harness_session_id = Some(uuid.into());
    row.session_id = Some(own_id.into());
    row.mux = Some(("test".into(), 999));
    core.agents = vec![row];
    core.worker_session_pane
        .insert(("codex".into(), uuid.into()), 77);
    // The stale registry ref alone does not resolve the new pane...
    assert_eq!(core.fno_id_for_pane(999), Some(own_id.to_string()));
    // The resume birthright maps the recorded harness session to the row,
    // so the re-homed pane reads the row's own id, not the harness session.
    assert_eq!(core.fno_id_for_pane(77), Some(own_id.to_string()));
    // A pane the daemon never re-homed and no row hosts stays untracked.
    assert_eq!(core.fno_id_for_pane(78), None);
}

#[test]
fn persist_squad_writes_named_workspace_and_dupe_is_taken() {
    let _s = StoreScratch::new("persist-named");
    let mut core = empty_core();
    named_member_squad(&mut core, 7, "harden", 100, "c19cd2c3");
    core.persist_squad(7);
    let loaded = crate::squad_store::load();
    assert_eq!(loaded.squads.len(), 1);
    assert_eq!(loaded.squads[0].name, "harden");
    assert_eq!(loaded.squads[0].origins, vec!["/repo".to_string()]);
    // A live named squad is taken; a persisted-but-not-live one is too.
    assert!(core.named_squad_taken("harden"), "live name is taken");
    core.session.squads.clear();
    assert!(core.named_squad_taken("harden"), "persisted name is taken");
    assert!(!core.named_squad_taken("nope-zzz"));
}

#[test]
fn persist_squad_unnamed_same_origin_upserts_one_row_across_persists() {
    // x-e447 AC-HP1: a repo's home squad derives its durable key from its
    // origin, so two server lifetimes (a fresh in-memory squad each) upsert
    // onto ONE row instead of appending. The old random mint minted a fresh
    // key per lifetime, so upsert never matched the prior row.
    let _s = StoreScratch::new("persist-unnamed-origin");
    let repo = "/repo/e447";
    let expected_key = crate::squad_store::origin_key(&[repo.to_string()]);
    // Lifetime 1: a fresh unnamed home squad, persisted.
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec![repo.into()],
        None,
        Tab {
            name: None,
            id: 1,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.persist_squad(1);
    assert_eq!(crate::squad_store::load().squads.len(), 1);
    // Lifetime 2: the in-memory squad is GONE (process restarted); a fresh
    // one for the same repo persists again. The derived key matches, so it
    // upserts onto the same row instead of appending.
    let mut core = empty_core();
    core.session.add_squad(
        1,
        vec![repo.into()],
        None,
        Tab {
            name: None,
            id: 1,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.persist_squad(1);
    let loaded = crate::squad_store::load();
    assert_eq!(
        loaded.squads.len(),
        1,
        "second lifetime upserts onto one row, not a second"
    );
    assert_eq!(
        loaded.squads[0].key, expected_key,
        "key is the derived origin key, not a fresh mint"
    );
    assert_eq!(loaded.squads[0].origins, vec![repo.to_string()]);
}
#[test]
fn churn_tombstones_and_user_close_de_recruits() {
    // AC4-EDGE: a worker dying on its own tombstones its member (survives as
    // a persisted, tombstoned entry). AC3-EDGE: the user closing the pane
    // de-recruits it (gone from the store).
    let _s = StoreScratch::new("churn-vs-user");
    let mut core = empty_core();
    named_member_squad(&mut core, 7, "harden", 100, "c19cd2c3");
    core.persist_squad(7);

    // Churn: the member is tombstoned, not removed.
    let ctx = core.member_ctx(100);
    assert!(ctx.is_some(), "pane 100 resolves to a persisted member");
    core.reconcile_member_close(ctx, true);
    let after_churn = crate::squad_store::load();
    assert_eq!(after_churn.squads[0].members.len(), 1);
    assert!(
        after_churn.squads[0].members[0].tombstone,
        "churn tombstones"
    );

    // User close of the still-live squad de-recruits the member.
    let ctx = core.member_ctx(100);
    core.reconcile_member_close(ctx, false);
    let after_user = crate::squad_store::load();
    assert_eq!(after_user.squads.len(), 1, "workspace survives");
    assert!(
        after_user.squads[0].members.is_empty(),
        "member de-recruited"
    );
}

#[test]
fn cross_squad_member_move_de_recruits_from_the_source() {
    // (x-d6a8, codex P1) Moving a persisted member's pane into another
    // squad's tab de-recruits it from the source workspace - its
    // squad_members entry must not linger under a squad it no longer lives
    // in. Regression for the codex re-review finding on move_pane_cross_tab.
    let _s = StoreScratch::new("cross-squad-member-move");
    let mut core = empty_core();
    // Source workspace "harden" (squad 7): member pane 100 in tab 7, plus a
    // second tab (pane 101) so the squad SURVIVES the move.
    named_member_squad(&mut core, 7, "harden", 100, "c19cd2c3");
    core.session.squad_mut(7).unwrap().tabs.push(Tab {
        name: None,
        id: 70,
        root: Node::Leaf(101),
        focus: 101,
    });
    core.persist_squad(7);
    // Destination squad 8 with a tab to drop into.
    core.session.add_squad(
        8,
        vec!["/other".into()],
        Some("review".into()),
        Tab {
            name: None,
            id: 8,
            root: Node::Leaf(200),
            focus: 200,
        },
    );

    let src = core.session.find_pane(100).expect("member pane live");
    let dst = core.session.find_pane(200).expect("anchor pane live");
    assert_ne!(src.0, dst.0, "precondition: a cross-squad move");
    core.move_pane_cross_tab(100, src, 200, dst, Dir::Right)
        .expect("cross-squad move");

    // The pane moved into squad 8...
    assert_eq!(
        core.session.find_pane(100).map(|(s, _)| s),
        Some(8),
        "pane moved to squad 8"
    );
    // ...and its membership no longer lingers in the persisted source.
    let loaded = crate::squad_store::load();
    let harden = loaded
        .squads
        .iter()
        .find(|s| s.name == "harden")
        .expect("source workspace survives its second tab");
    assert!(
        harden.members.iter().all(|m| m.attach_id != "c19cd2c3"),
        "the moved member is de-recruited from the source workspace"
    );
}
#[test]
fn user_close_of_last_member_pane_de_persists_the_workspace() {
    // AC3-EDGE corner: closing the workspace's last pane (squad removed from
    // the session) drops the whole entry, so it never returns at restart.
    let _s = StoreScratch::new("last-pane");
    let mut core = empty_core();
    named_member_squad(&mut core, 7, "harden", 100, "c19cd2c3");
    core.persist_squad(7);
    let ctx = core.member_ctx(100);
    // Simulate the squad already gone (its last tab was removed by close).
    core.session.squads.clear();
    core.reconcile_member_close(ctx, false);
    let loaded = crate::squad_store::load();
    assert!(loaded.squads.is_empty(), "de-persisted with its last pane");
}

#[test]
fn rerun_refused_for_a_busy_or_unknown_agent_pane() {
    // AC-ERR: false-ready is the forbidden direction - a working/blocked
    // agent pane refuses, and an unknown (liveness-only) badge fails closed.
    assert!(rerun_allowed(&[agent(7, Some(AgentBadge::Working), false)], "main", 7).is_err());
    assert!(rerun_allowed(&[agent(7, Some(AgentBadge::Blocked), false)], "main", 7).is_err());
    assert!(rerun_allowed(&[agent(7, None, false)], "main", 7).is_err());
}

#[test]
fn rerun_allowed_for_an_idle_or_exited_agent_pane() {
    // A done agent (idle) or an exited one (the pane is a shell again) allows.
    assert_eq!(
        rerun_allowed(&[agent(7, Some(AgentBadge::Done), false)], "main", 7),
        Ok(())
    );
    assert_eq!(
        rerun_allowed(&[agent(7, Some(AgentBadge::Working), true)], "main", 7),
        Ok(())
    );
}

#[test]
fn rerun_guard_is_scoped_to_the_current_session() {
    // Pane ids collide across sessions: a FOREIGN session's idle (Done) agent
    // on the same pane number must NOT clear the guard for THIS session's busy
    // agent - that would be the forbidden write into a working composer.
    let rows = [
        agent_in("other", 5, Some(AgentBadge::Done), false),
        agent_in("main", 5, Some(AgentBadge::Working), false),
    ];
    assert!(
        rerun_allowed(&rows, "main", 5).is_err(),
        "our busy agent must gate regardless of a foreign idle row on pane 5"
    );
    // And a foreign busy row must not spuriously gate our plain-shell pane.
    let foreign_only = [agent_in("other", 5, Some(AgentBadge::Working), false)];
    assert_eq!(rerun_allowed(&foreign_only, "main", 5), Ok(()));
}

#[path = "server/tests/core_fixture.rs"]
mod core_fixture;
pub(super) use core_fixture::empty_core;

fn placement_core() -> Core {
    let mut core = empty_core();
    core.session.add_squad(
        7,
        vec!["/repo/default".into()],
        Some("review".into()),
        Tab {
            name: None,
            id: 11,
            root: Node::Leaf(1),
            focus: 1,
        },
    );
    core.next_pane_id = 2;
    core.next_squad_id = 8;
    core
}
#[test]
fn pane_placement_resolves_named_and_stale_targets_before_spawn() {
    let core = placement_core();
    assert_eq!(
        core.resolve_placement_target(&PaneTarget::SquadName("review".into()), None),
        Ok(Some(7))
    );
    assert_eq!(
        core.resolve_placement_target(&PaneTarget::SquadId(7), None),
        Ok(Some(7))
    );
    assert!(core
        .resolve_placement_target(&PaneTarget::SquadName("missing".into()), None)
        .is_err());
    assert!(core
        .resolve_placement_target(&PaneTarget::SquadId(99), None)
        .is_err());
}
#[test]
fn pane_placement_split_refusal_falls_back_to_new_tab() {
    // AC3-FR (x-9f75): a split refused at min-size no longer reaps and dead-ends - the pane lands as a
    // NEW TAB in the same squad, the original tab is untouched, and the caller is told to notice.
    let mut core = placement_core();
    core.sizing.areas.insert(11, (24, 16));
    core.claim_eligible.insert(2);
    let before = core.session.squad(7).unwrap().tabs[0].clone();
    let (sid, tid, fell_back) = core
        .place_spawned_pane(Some(7), "/repo/child", 2, Some(Dir::Right))
        .unwrap();
    assert!(
        fell_back,
        "the caller must know to emit the tab-full notice"
    );
    assert_eq!(sid, 7);
    let squad = core.session.squad(7).unwrap();
    assert_eq!(squad.tabs.len(), 2, "a new tab was added");
    assert_eq!(squad.tabs[0], before, "the crowded tab is untouched");
    let landed = squad.tabs.iter().find(|t| t.id == tid).unwrap();
    assert_eq!(
        landed.root,
        Node::Leaf(2),
        "pane landed as the new tab's leaf"
    );
    assert!(
        core.claim_eligible.contains(&2),
        "the pane is not reaped, so its claim eligibility survives"
    );
}
#[test]
fn attach_new_tab_anchors_pane_in_row_cwd() {
    // US5 contract (x-9f75): attaching a watch-only row spawns the pane in the ROW's own cwd, not the
    // viewer's squad cwd. Asserting the existing behavior so it becomes contract, not accident (the
    // interactive cwd chooser is a deferred follow-up; these defaults are the floor).
    let root = std::env::temp_dir().join(format!("fno-row-cwd-{}", std::process::id()));
    let row_cwd = root.join("agent-home");
    std::fs::create_dir_all(&row_cwd).unwrap();
    let marker = row_cwd.join("cwd.txt");
    // The attach spawn writes its pwd then idles, standing in for the real
    // `claude attach <id>` (the id rides as $0 for `sh -c`, harmless).
    set_attach_program(&["/bin/sh", "-c", "pwd > cwd.txt; sleep 30"]);
    let (mut core, client_id, _p1, _p2, _rx) = seen_test_core();
    core.agents = vec![bg_row(
        "home-agent",
        &row_cwd.to_string_lossy(),
        Some("deadbee2"),
    )];

    core.command(client_id, Command::attach_agent("deadbee2"));

    // A loaded CI runner can take several seconds just to spawn the PTY +
    // start the shell; 15s matches the PTY-wait convention elsewhere and
    // keeps this off the flake list. Readiness is NON-EMPTY CONTENT, not
    // existence: `pwd > cwd.txt` creates the file on redirect, BEFORE pwd
    // writes into it, so an exists() gate can hand the read an empty string
    // and the canonicalize below then fails as a confusing NotFound.
    let content = || {
        std::fs::read_to_string(&marker)
            .ok()
            .filter(|s| !s.trim().is_empty())
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    while content().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let reported =
        content().expect("pane shell never wrote cwd.txt within 15s (spawn slow or failed)");
    assert_eq!(
        std::fs::canonicalize(reported.trim()).unwrap(),
        std::fs::canonicalize(&row_cwd).unwrap(),
        "the attach pane is anchored in the row's own cwd"
    );
    if let Some(&pid) = core.attached.get("deadbee2") {
        core.reap_pane(pid);
    }
    let _ = std::fs::remove_dir_all(root);
}

fn client(id: u64, view_tab: TabId, dims: (u16, u16), passive: bool) -> Client {
    Client {
        id,
        reliable_tx: mpsc::channel(1).0,
        dirty: Arc::default(),
        notify: Arc::new(Notify::new()),
        synced_modes: Modes::default(),
        view: (1, view_tab),
        visible: HashSet::new(),
        dims,
        passive,
        last_press: None,
    }
}
#[test]
fn observer_attach_never_spawns_a_pane() {
    // Locked Decision 5: an observer (0,0) attach to a session it does not
    // match must register read-only without ever creating a squad/PTY.
    let mut core = empty_core();
    let (tx, _rx) = mpsc::channel::<ServerMsg>(8);
    core.attach(
        1,
        0,
        0,
        "/nowhere".into(),
        "/nowhere".into(),
        tx,
        Arc::default(),
        Arc::new(Notify::new()),
    );
    assert_eq!(
        core.panes.len(),
        0,
        "observer attach must never spawn a PTY"
    );
    assert_eq!(core.clients.len(), 1);
    assert!(core.clients[0].passive, "the (0,0) client is passive");
}

#[test]
fn bye_flushes_the_final_dirty_frame_first() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (mut writer, mut reader) = tokio::io::duplex(4096);
            let dirty: DirtyMap = Arc::default();
            let frame = Frame {
                rows: 1,
                cols: 1,
                cells: vec![crate::proto::Cell::default()],
                cursor_row: 0,
                cursor_col: 0,
                cursor_visible: true,
                scroll_offset: 0,
                history_lines: 0,
            };
            dirty.lock().unwrap().insert(7, frame.clone());
            let stats: PaneStats = Arc::default();
            assert!(write_reliable(
                &mut writer,
                &ServerMsg::Bye {
                    reason: "done".into(),
                },
                &dirty,
                &stats,
            )
            .await
            .unwrap());

            assert_eq!(
                read_msg::<_, ServerMsg>(&mut reader).await.unwrap(),
                ServerMsg::Frame { pane_id: 7, frame }
            );
            assert_eq!(
                read_msg::<_, ServerMsg>(&mut reader).await.unwrap(),
                ServerMsg::Bye {
                    reason: "done".into()
                }
            );
            assert!(dirty.lock().unwrap().is_empty());
        });
}

// Pane-counter and frame-emission family (plus the x-a600 repaint
// gesture tests) moved verbatim into its own module: this file is over
// the shrink-only line, and test motion is the sanctioned shrink.
#[path = "server/tests/pane_frame_flow_tests.rs"]
mod pane_frame_flow_tests;

#[test]
fn cold_attach_delivers_every_pane_frame_on_the_reliable_channel() {
    // AC1-FR (x-0296): the cold-attach snapshot must NOT depend on the
    // droppable dirty map or later PTY output. Deterministic mechanism
    // guard: after attach, the client's reliable queue holds the Layout
    // followed by a Frame for EVERY pane in it. Pre-fix this fails 100%
    // (seeds sat only in the dirty map); no timing involved.
    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let p1 = core.spawn_pane(24, 40, "/tmp").expect("pane 1");
    let p2 = core.spawn_pane(24, 40, "/tmp").expect("pane 2");
    core.session.add_squad(
        1,
        vec!["/tmp/x0296".into()],
        None,
        Tab {
            name: None,
            id: 1,
            root: Node::Branch {
                axis: Axis::Horizontal,
                children: vec![(0.5, Node::Leaf(p1)), (0.5, Node::Leaf(p2))],
            },
            focus: p1,
        },
    );
    let (tx, mut rx) = mpsc::channel::<ServerMsg>(32);
    core.attach(
        9,
        24,
        80,
        "/tmp/x0296".into(),
        "/tmp/x0296".into(),
        tx,
        Arc::default(),
        Arc::new(Notify::new()),
    );
    let mut msgs = Vec::new();
    while let Ok(m) = rx.try_recv() {
        msgs.push(m);
    }
    let layout_at = msgs
        .iter()
        .position(|m| matches!(m, ServerMsg::Layout { .. }))
        .expect("attach queues a Layout reliably");
    let layout_panes: HashSet<u64> = match &msgs[layout_at] {
        ServerMsg::Layout { panes, .. } => panes.iter().map(|(pid, _)| *pid).collect(),
        _ => unreachable!(),
    };
    assert_eq!(
        layout_panes,
        HashSet::from([p1, p2]),
        "both panes are in the attach Layout"
    );
    let framed: HashSet<u64> = msgs[layout_at + 1..]
        .iter()
        .filter_map(|m| match m {
            ServerMsg::Frame { pane_id, .. } => Some(*pane_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        framed, layout_panes,
        "every Layout pane's initial frame must ride the reliable \
             channel AFTER the Layout - a dirty-map-only seed is droppable \
             and a passive reattach never recovers it (x-0296)"
    );
}
/// A one-squad, two-tab (one pane each) `Core` with a client attached and
/// viewing it - the shared rig for the x-4328 seen-bit tests below. The
/// returned receiver must stay alive for the test's duration: dropping it
/// closes the client's channel, which `push_layout` reads as "gone" and
/// prunes the client, breaking every later `Command::FocusPane` (no
/// client view to act on).
pub(super) fn seen_test_core() -> (Core, u64, u64, u64, mpsc::Receiver<ServerMsg>) {
    // attach() runs restore_squads() -> squad_store::load(), which defaults
    // to the real $HOME/.fno/squads.json; a dev box with a live store then
    // imports its squads and breaks squad/row-count asserts that pass on a
    // fresh-home CI runner. Point the store at a nonexistent per-thread path
    // (missing file reads as an empty store; the writer creates its parent
    // only when a real write happens) and restore becomes a no-op. TEST_PATH
    // is thread-local and one test == one thread: no leaks, no teardown.
    let scratch = std::env::temp_dir().join(format!(
        "fno-seen-store-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&scratch); // sweep any stale same-pid dir
    crate::squad_store::set_test_path(&scratch);

    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let p1 = core.spawn_pane(24, 40, "/tmp/seen").expect("pane 1");
    let p2 = core.spawn_pane(24, 40, "/tmp/seen").expect("pane 2");
    core.session.add_squad(
        1,
        vec!["/tmp/seen".into()],
        None,
        Tab {
            name: None,
            id: 1,
            root: Node::Leaf(p1),
            focus: p1,
        },
    );
    core.session.squad_mut(1).unwrap().tabs.push(Tab {
        name: None,
        id: 2,
        root: Node::Leaf(p2),
        focus: p2,
    });
    let (tx, rx) = mpsc::channel::<ServerMsg>(32);
    let client_id = 9;
    core.attach(
        client_id,
        24,
        80,
        "/tmp/seen".into(),
        "/tmp/seen".into(),
        tx,
        Arc::default(),
        Arc::new(Notify::new()),
    );
    (core, client_id, p1, p2, rx)
}
#[test]
fn bounded_scroll_target_caps_a_burst_but_spares_a_single_notch() {
    // A big same-direction fold is capped to one viewport (24) either way...
    assert_eq!(bounded_scroll_target(0, 300, 24), 24, "up burst capped");
    assert_eq!(bounded_scroll_target(300, 0, 24), 276, "down burst capped");
    // ...a move already within a screen (a lone wheel notch) is untouched...
    assert_eq!(
        bounded_scroll_target(0, MOUSE_WHEEL_LINES, 24),
        MOUSE_WHEEL_LINES
    );
    // ...a boundary reversal the in-order fold landed at +3 survives the cap
    // (it never re-introduces the netting bug)...
    assert_eq!(bounded_scroll_target(0, 3, 24), 3);
    // ...and a true no-op fold stays put.
    assert_eq!(bounded_scroll_target(10, 10, 24), 10);
}

/// Graft a second squad hosting one fresh pane onto a `seen_test_core`, so a
/// `pane focus` test has somewhere to travel TO. Returns the new pane id and
/// its tab id.
fn add_second_squad(core: &mut Core) -> (u64, TabId) {
    let p3 = core.spawn_pane(24, 40, "/tmp/other").expect("pane 3");
    core.session.add_squad(
        2,
        vec!["/tmp/other".into()],
        Some("other".into()),
        Tab {
            name: None,
            id: 3,
            root: Node::Leaf(p3),
            focus: p3,
        },
    );
    (p3, 3)
}

#[test]
fn pane_focus_moves_the_viewer_across_squads_and_reports_where() {
    // AC1-HP: the command is a goto, not a nudge. A client viewing squad 1 ends
    // up on squad 2's tab with the named pane focused, and the receipt names
    // the RESOLVED location rather than echoing the request.
    let (mut core, client_id, _p1, _p2, _rx) = seen_test_core();
    let (p3, t3) = add_second_squad(&mut core);
    let view_before = core
        .clients
        .iter()
        .find(|c| c.id == client_id)
        .unwrap()
        .view;
    assert_eq!(view_before.0, 1, "precondition: viewing squad 1");

    let reply = core.pane_focus(p3);
    match reply {
        ServerMsg::PaneFocused {
            pane,
            squad_id,
            squad_name,
            tab_id,
            tab_name,
            tab_ordinal,
            clients_moved,
        } => {
            assert_eq!((pane, squad_id, tab_id), (p3, 2, t3));
            assert_eq!(squad_name.as_deref(), Some("other"));
            assert_eq!((tab_name.as_deref(), tab_ordinal), (None, Some(1)));
            // The count is the anti-lie field: it must reflect a client that
            // actually ended up looking at the pane.
            assert_eq!(clients_moved, 1);
        }
        other => panic!("expected PaneFocused, got {other:?}"),
    }
    let c = core.clients.iter().find(|c| c.id == client_id).unwrap();
    assert_eq!(c.view, (2, t3), "the viewer really moved");
    assert_eq!(
        core.session.squad(2).unwrap().tabs[0].focus,
        p3,
        "and the pane is focused, not merely on screen"
    );
}

#[test]
fn pane_focus_refuses_a_dead_pane_fail_closed() {
    // AC1-EDGE: an id no live pane owns is DEAD_PANE with the same `no such
    // pane` wording `Command::FocusPane` already refuses with, and the
    // viewer does not move a millimetre on the way to the refusal.
    let (mut core, client_id, _p1, _p2, _rx) = seen_test_core();
    let before = core
        .clients
        .iter()
        .find(|c| c.id == client_id)
        .unwrap()
        .view;
    match core.pane_focus(999_999) {
        ServerMsg::Err { code, msg } => {
            assert_eq!(code, err_code::DEAD_PANE);
            assert!(msg.contains("no such pane"), "wording: {msg}");
        }
        other => panic!("expected Err, got {other:?}"),
    }
    assert_eq!(
        core.clients
            .iter()
            .find(|c| c.id == client_id)
            .unwrap()
            .view,
        before,
        "a refusal moves nobody"
    );
}

#[test]
fn pane_focus_refuses_when_only_passive_observers_are_attached() {
    // AC1-ERR: no non-passive client is a REFUSAL with its OWN code, never a
    // silent pass and never DEAD_PANE. A passive observer is read-only at the
    // server with no viewport to move, so it must not satisfy the check -
    // otherwise the command reports success for a pane nobody can see.
    let (mut core, client_id, p1, _p2, _rx) = seen_test_core();
    core.clients.retain(|c| c.id != client_id);
    let (tx, _rx2) = mpsc::channel::<ServerMsg>(32);
    core.attach(
        77,
        0, // rows == cols == 0 is what makes a client passive
        0,
        "/tmp/seen".into(),
        "/tmp/seen".into(),
        tx,
        Arc::default(),
        Arc::new(Notify::new()),
    );
    assert!(core.is_passive(77), "precondition: the observer is passive");
    match core.pane_focus(p1) {
        ServerMsg::Err { code, msg } => {
            assert_eq!(
                code,
                err_code::NO_CLIENT,
                "distinct from DEAD_PANE: 'nobody is watching' is not 'your \
                     pane is gone'"
            );
            assert!(msg.contains("no attached client"), "wording: {msg}");
        }
        other => panic!("expected Err, got {other:?}"),
    }
}
#[test]
fn a_re_run_evicts_and_does_not_self_reinsert() {
    // AC1-EDGE: Done(focused) -> seen; the pane re-runs to Working (the
    // level-triggered evict in push_layout drops it); it finishes to
    // Done again WITHOUT a fresh focus action - it must stay unseen
    // until re-focused, not re-arm itself just because focus never left.
    let (mut core, client_id, p1, _p2, _rx) = seen_test_core();
    core.agents = vec![agent_in("test", p1, Some(AgentBadge::Done), false)];
    core.command(client_id, Command::FocusPane(p1));
    assert!(core.seen.contains(&p1), "precondition: seen after focus");

    core.agents = vec![agent_in("test", p1, Some(AgentBadge::Working), false)];
    core.push_layout(true);
    assert!(!core.seen.contains(&p1), "a Working tick evicts it");

    core.agents = vec![agent_in("test", p1, Some(AgentBadge::Done), false)];
    core.push_layout(true);
    assert!(
        !core.seen.contains(&p1),
        "the second Done must stay unseen until re-focused - insert is \
             a one-shot side effect of FocusPane, never a per-pass level \
             check on \"is this still the focused pane\""
    );

    core.command(client_id, Command::FocusPane(p1));
    assert!(core.seen.contains(&p1), "re-focusing re-arms seen");
}

#[test]
fn focusing_while_working_never_seeds_a_later_done() {
    // AC2-EDGE: a focus action that lands while the badge is `Working`
    // must not mark the pane seen once it later finishes unattended.
    let (mut core, client_id, p1, _p2, _rx) = seen_test_core();
    core.agents = vec![agent_in("test", p1, Some(AgentBadge::Working), false)];
    core.command(client_id, Command::FocusPane(p1));
    assert!(
        !core.seen.contains(&p1),
        "a Working-time focus never sets the done-seen bit"
    );

    core.agents = vec![agent_in("test", p1, Some(AgentBadge::Done), false)];
    core.push_layout(true);
    assert!(
        !core.seen.contains(&p1),
        "finishing without a fresh focus action stays unseen"
    );
}

#[test]
fn evict_is_per_pane_not_per_focus() {
    // A non-focused Done pane's seen bit (set earlier) is untouched by a
    // push_layout pass that focuses a DIFFERENT pane; eviction keys on
    // the pane's OWN current badge, never on which pane is focused.
    let (mut core, client_id, p1, p2, _rx) = seen_test_core();
    core.agents = vec![agent_in("test", p1, Some(AgentBadge::Done), false)];
    core.command(client_id, Command::FocusPane(p1));
    assert!(core.seen.contains(&p1));

    core.agents = vec![
        agent_in("test", p1, Some(AgentBadge::Done), false),
        agent_in("test", p2, Some(AgentBadge::Working), false),
    ];
    core.command(client_id, Command::FocusPane(p2));
    assert!(
        core.seen.contains(&p1),
        "p1 stays seen: it is still Done, just no longer focused"
    );
    assert!(!core.seen.contains(&p2), "p2 never reached Done");
}
#[test]
fn link_gate_opens_one_pane_for_a_tagged_passive_client() {
    let (mut core, pane) = template_core();
    let vt = &mut core.panes.get_mut(&pane).unwrap().vt;
    vt.feed("line\r\n".repeat(200).as_bytes());
    core.clients.push(client(2, 5, (0, 0), true));
    let tick = |pane| CoreMsg::PaneScroll {
        id: 2,
        pane,
        delta: 3,
    };
    let hello = |identity: String| crate::proto::LinkHello {
        name: "me".into(),
        pane,
        identity,
    };
    let offset = |core: &Core| core.panes[&pane].vt.display_offset();
    core.handle(tick(pane));
    assert_eq!(offset(&core), 0, "an untagged observer stays read-only");
    core.handle(CoreMsg::Link(2, hello("pid:0".into())));
    core.handle(tick(pane));
    assert_eq!(offset(&core), 0, "a stale pane reading is refused");
    let child = core.panes[&pane].pty.child_pid().unwrap();
    core.handle(CoreMsg::Link(2, hello(format!("pid:{child}"))));
    core.handle(tick(pane));
    assert_ne!(offset(&core), 0, "the link drives its own pane");
    assert!(!core.links.may_send(2, &tick(pane + 1)), "only that pane");
    let command = CoreMsg::Command {
        id: 2,
        cmd: Command::ReapAgents,
    };
    assert!(!core.links.may_send(2, &command), "and no command");
    core.reap_pane(pane);
}
// The freshness contract: a fingerprint over the daemon-side region verifies
// against the server's re-hash of the same grid (else every answer would
// fail closed as stale - a false negative that breaks the feature), and a
// grid that advanced hashes differently (the true-positive stale that keeps
// an answer off a moved-on pane).
#[test]
fn answer_fingerprint_matches_unchanged_grid_and_rejects_advanced() {
    let grid = "scrollback\n\nDo you want to proceed?\n  ❯ 1. Yes\n  2. No\n";
    let daemon_fp = *blake3::hash(bottom_non_empty_lines(grid, 8).as_bytes()).as_bytes();
    // Server re-reads the identical grid -> same fingerprint (answer lands).
    let server_fp = *blake3::hash(bottom_non_empty_lines(grid, 8).as_bytes()).as_bytes();
    assert_eq!(daemon_fp, server_fp, "unchanged grid must verify");
    // The pane advanced (a new line appended) -> different fingerprint.
    let advanced = format!("{grid}Running the tool now...\n");
    let advanced_fp = *blake3::hash(bottom_non_empty_lines(&advanced, 8).as_bytes()).as_bytes();
    assert_ne!(daemon_fp, advanced_fp, "advanced grid must read stale");
}

fn block(complete: bool, truncated: bool, implicit: bool, text: &str) -> vt::BlockRead {
    vt::BlockRead {
        seq: Some(0),
        exit: Some(0),
        complete,
        truncated,
        implicit,
        text: text.to_string(),
    }
}

#[test]
fn copy_source_precedence_selection_then_block_then_none() {
    // AC-happy: an active selection wins even when a completed block exists.
    assert_eq!(
        copy_source(Some("sel".into()), || Ok(block(true, false, false, "blk"))),
        Some("sel".into())
    );
    // AC-happy: no selection -> the newest completed block copies.
    assert_eq!(
        copy_source(None, || Ok(block(true, false, false, "blk"))),
        Some("blk".into())
    );
    // AC-error: no selection and no block (BLOCK_UNAVAILABLE) -> notice.
    assert_eq!(copy_source(None, || Err(())), None);
}

#[test]
fn copy_source_refuses_open_truncated_and_implicit_blocks() {
    // The open (still-running) block never copies.
    assert_eq!(
        copy_source(None, || Ok(block(false, false, false, "partial"))),
        None
    );
    // AC-edge: a truncated (head-evicted) block refuses rather than copy wrong text.
    assert_eq!(
        copy_source(None, || Ok(block(true, true, false, "trunc"))),
        None
    );
    // A markerless pane's implicit whole-output block keeps the old notice.
    assert_eq!(
        copy_source(None, || Ok(block(true, false, true, "whole"))),
        None
    );
}

// ---- the keeper contract (re-adopt, sweep, list) --------------------
// The re-adoption spawn helpers live in server/tests/keeper_adopt_tests.rs,
// their only consumers (shrink-only file budget).

#[test]
fn emergency_roster_kills_plain_child_and_spares_keeper_child() {
    let plain =
        ChildGuard::spawn(std::process::Command::new("/bin/sh").args(["-c", "exec sleep 30"]));
    let keeper =
        ChildGuard::spawn(std::process::Command::new("/bin/sh").args(["-c", "exec sleep 30"]));
    let plain_pid = plain.id();
    let keeper_pid = keeper.id();
    let roster = HashSet::from([
        PaneChild {
            pid: plain_pid,
            keeper_hosted: false,
        },
        PaneChild {
            pid: keeper_pid,
            keeper_hosted: true,
        },
    ]);

    kill_plain_children(&roster);
    assert!(
        unsafe { libc::kill(plain_pid as libc::pid_t, 0) } != 0,
        "plain roster child must be killed"
    );
    assert_eq!(
        unsafe { libc::kill(keeper_pid as libc::pid_t, 0) },
        0,
        "keeper roster child must survive"
    );

    // The guards kill and reap at scope end, after the assertions, so
    // what they read is unchanged and the test leaves no zombie.
}

#[path = "server_tests_keeper.rs"]
mod server_tests_keeper;
