use super::*;

pub(in crate::client) fn check_fixture(view: &mut View) {
    use serde_json::json;
    let lead = AgentRow {
        name: "finch".into(),
        role_scope: Some("team".into()),
        role_level: Some(1),
        ..Default::default()
    };
    let worker = |name: &str, sid: &str, node: &str| AgentRow {
        name: name.into(),
        harness_session_id: Some(sid.into()),
        node: Some(node.into()),
        ..Default::default()
    };
    let backlog = crate::backlog_model::Inputs {
        rows: vec![
            json!({"id":"x-1","status":"ready","title":"First","sessions":[{"session_id":"s1"},{"session_id":"old1"},{"session_id":"old2"}]}),
            json!({"id":"x-2","status":"ready","title":"Second","blocked_by":["x-1"],"sessions":[{"session_id":"s2"},{"session_id":"s3"}]}),
        ],
        agents: vec![
            lead,
            worker("first", "s1", "x-1"),
            worker("second", "s2", "x-2"),
            worker("third", "s3", "x-2"),
        ],
        ..Default::default()
    };
    open(view);
    let gen = view.org_generation;
    apply_fold(
        view,
        gen,
        OrgInputs {
            backlog,
            fold: Ok(
                json!({"scope_nodes":{"team":{"status":"ok","nodes":[{"id":"x-1"},{"id":"x-2"}]}},"owned_scopes":{"x-1":"team","x-2":"team"}}),
            ),
            measured_at: now(),
        },
    );
    let b = view.org_board.as_mut().unwrap();
    b.mode = OrgMode::Tree;
    b.filter = OrgSessions::Current;
    let texts = b
        .lines_with_map(60, 24)
        .0
        .into_iter()
        .map(|l| l.text)
        .collect::<Vec<_>>();
    assert!(texts[1].contains("finch"));
    assert!(texts[2].contains("x-1"));
    assert!(texts[3].contains("first"));
    assert!(texts[4].contains("x-2"));
    assert!(b.footer().starts_with("leads 1 · current 3"));
    assert!(texts[0].starts_with("Tree │ Table │ Graph · current"));
    assert!(
        texts[3].starts_with("│ └ "),
        "a worker hangs under its node"
    );
    assert!(
        texts[4].starts_with("└ ▾ x-2"),
        "the last node closes the branch"
    );
    assert!(
        !texts
            .iter()
            .any(|t| ["{", "claim:", " Q", "age ", "PR-", "unobserved"]
                .iter()
                .any(|j| t.contains(j))),
        "rows read in words: {texts:?}"
    );
    // AC3-HP: a long node title wraps whole at a 27-col column,
    // every line stays inside the column, and the map ties every wrapped
    // line back to its row.
    let node_index = b
        .rows()
        .iter()
        .position(|r| r.key == "node:x-1")
        .expect("the node row exists");
    let saved_title = b.snapshot.tree.as_ref().unwrap().leads[0].nodes[0]
        .view
        .card
        .title
        .clone();
    b.snapshot.tree.as_mut().unwrap().leads[0].nodes[0]
        .view
        .card
        .title = "an eighty character node title about org views ".repeat(2) + "end marker";
    let (wrapped, wmap) = b.body(27);
    for (j, l) in wrapped.iter().enumerate() {
        assert!(
            l.text.chars().count() <= 27,
            "line {j} of {} spans {}: {:?} | all: {:?}",
            wrapped.len(),
            l.text.chars().count(),
            l.text,
            wrapped.iter().map(|l| l.text.clone()).collect::<Vec<_>>()
        );
    }
    let owned: Vec<_> = wmap.iter().filter(|l| **l == node_index).collect();
    assert!(
        owned.len() > 1,
        "the long title wrapped onto continuation lines"
    );
    assert!(
        wrapped
            .iter()
            .any(|l| l.text.starts_with("    ") && l.text.contains("marker")),
        "continuations indent under the guide: {:?}",
        wrapped.iter().map(|l| l.text.clone()).collect::<Vec<_>>()
    );
    assert!(wrapped.iter().any(|l| l.text.contains("end marker")));
    b.snapshot.tree.as_mut().unwrap().leads[0].nodes[0]
        .view
        .card
        .title = saved_title;
    let b = view.org_board.as_mut().unwrap();
    b.cursor = 0;
    assert_eq!(
        counts_line(&json!({"ready": 3, "done": 0, "in_review": 2, "in_progress": 4})),
        "4 working · 2 in review · 3 ready"
    );
    assert!(b.footer_hints(false).starts_with("tap to focus · j/k move"));
    assert!(b.footer_hints(true).starts_with("j/k move"));
    b.filter = OrgSessions::Former;
    assert_eq!(
        b.rows()
            .iter()
            .filter(|r| matches!(&r.selected, Selected::Session(_)))
            .count(),
        2
    );
    b.filter = OrgSessions::All;
    assert_eq!(
        b.rows()
            .iter()
            .filter(|r| matches!(&r.selected, Selected::Session(_)))
            .count(),
        5
    );
    b.filter = OrgSessions::Current;
    b.mode = OrgMode::Graph;
    let graph = b.refresh_graph(60, 20);
    assert!(graph.boxes.iter().any(|d| d.key == "node:x-2"));
    b.query = "first".into();
    let graph = b.refresh_graph(60, 20);
    assert!(!graph.boxes.iter().any(|d| d.label.contains("second")));
    b.query.clear();
    b.mode = OrgMode::Tree;
    let mut departed = b.snapshot.tree.as_ref().unwrap().leads[0].nodes[0].clone();
    departed.view.card.id = "departed-node".into();
    departed.current.clear();
    departed.former.truncate(1);
    departed.former[0].view.session_id = Some("last-run".into());
    b.snapshot.tree.as_mut().unwrap().leads[0]
        .left
        .push(departed);
    assert!(b
        .rows()
        .iter()
        .any(|r| r.text.contains("left the team (24h): 1")));
    assert!(!b.rows().iter().any(|r| r.text.contains("last-run")));
    b.departures.insert("team".into());
    assert!(b.rows().iter().any(|r| r.text.contains("last-run")));
    b.departures.clear();
    assert!(!b.rows().iter().any(|r| r.text.contains("last-run")));
    b.cursor = 0;
    let prefs = tempfile::tempdir().unwrap();
    crate::view_store::set_test_path(prefs.path());
    let saved_term = view.term;
    let saved_full = view.board_full;
    view.term = (24, 100);
    view.board_full = true;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let tap = |row, col| crate::mouse::MouseReport {
        row,
        col,
        kind: MouseKind::Press(MouseButton::Left),
        shift: false,
    };
    runtime.block_on(async {
        let (mut socket, _receiver) = tokio::io::duplex(4096);
        mouse(view, tap(3, 4), &mut socket).await.unwrap();
        assert_eq!(view.org_board.as_ref().unwrap().cursor, 2, "a tap selects");
        mouse(view, tap(0, 8), &mut socket).await.unwrap();
        assert_eq!(
            view.org_board.as_ref().unwrap().mode,
            OrgMode::Table,
            "a tab tap switches mode"
        );
        view.org_board.as_mut().unwrap().mode = OrgMode::Tree;
        mouse(view, tap(2, 4), &mut socket).await.unwrap();
        assert!(view.org_board.is_some(), "a tap on a node only selects it");
        mouse(view, tap(2, 4), &mut socket).await.unwrap();
        assert!(view.org_board.is_none(), "a second tap acts like Enter");
    });
    crate::view_store::clear_test_path();
    view.term = saved_term;
    view.board_full = saved_full;
    let generation = view.org_generation;
    view.org_board = None;
    open(view);
    assert!(view.org_generation > generation);
    let mut stale = crate::org_model::OrgInputs {
        backlog: Default::default(),
        fold: Err("stale".into()),
        measured_at: 0,
    };
    apply_fold(view, generation, stale.clone());
    assert!(view.org_board.as_ref().unwrap().snapshot.error.is_none());
    stale.fold = Err("fresh failure".into());
    let generation = view.org_generation;
    apply_fold(view, generation, stale);
    assert_eq!(
        view.org_board.as_ref().unwrap().snapshot.error.as_deref(),
        Some("fresh failure")
    );
}

// AC4: the Table paints one row per worker, in lead-then-node
// order, with columns that earn a place and a basis where a reading is
// missing.
#[test]
fn table_is_one_row_per_worker_with_fitting_columns() {
    use serde_json::json;
    let mut view = View::new(
        (24, 100),
        "main".into(),
        LayoutView {
            squads: Vec::new(),
            active_squad: 0,
            panes: Vec::new(),
            focus: 0,
            area: (0, 0),
            agents: Vec::new(),
            focus_node: None,
        },
    );
    let lead = AgentRow {
        name: "finch".into(),
        role_scope: Some("team".into()),
        role_level: Some(1),
        ..Default::default()
    };
    let lead2 = AgentRow {
        name: "wren".into(),
        role_scope: Some("team2".into()),
        role_level: Some(1),
        ..Default::default()
    };
    let worker = |name: &str, sid: &str, node: &str, exited: bool| AgentRow {
        name: name.into(),
        harness_session_id: Some(sid.into()),
        node: Some(node.into()),
        exited,
        ..Default::default()
    };
    let backlog = crate::backlog_model::Inputs {
        rows: vec![
            json!({"id":"x-1","status":"ready","title":"First","sessions":[{"session_id":"s1"},{"session_id":"old1"}]}),
            json!({"id":"x-2","status":"ready","title":"Second","sessions":[{"session_id":"s2"}]}),
            json!({"id":"x-3","status":"ready","title":"Third","sessions":[{"session_id":"s3"}]}),
        ],
        agents: vec![
            lead,
            lead2,
            worker("first", "s1", "x-1", false),
            worker("second", "s2", "x-2", false),
            worker("third", "s3", "x-3", false),
            worker("gone", "old1", "x-1", true),
        ],
        ..Default::default()
    };
    open(&mut view);
    let gen = view.org_generation;
    apply_fold(
        &mut view,
        gen,
        OrgInputs {
            backlog,
            fold: Ok(
                json!({"scope_nodes":{"team":{"status":"ok","nodes":[{"id":"x-1"},{"id":"x-2"}]},"team2":{"status":"ok","nodes":[{"id":"x-3"}]}},"owned_scopes":{"x-1":"team","x-2":"team","x-3":"team2"}}),
            ),
            measured_at: now(),
        },
    );
    let b = view.org_board.as_mut().unwrap();
    b.mode = OrgMode::Table;
    b.filter = OrgSessions::All;
    let rows = b.mode_rows(60, 24);
    assert_eq!(
        rows.len(),
        4,
        "{}",
        rows.iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
            .join(" | ")
    );
    assert!(rows
        .iter()
        .all(|r| matches!(r.selected, Selected::Session(_))));
    assert_eq!(rows[0].lead, "finch", "lead order: {}", rows[0].lead);
    let lines: Vec<String> = b
        .lines_with_map(60, 24)
        .0
        .into_iter()
        .map(|l| l.text.clone())
        .collect();
    assert_eq!(lines.len(), 6, "header plus four worker rows: {lines:?}");
    assert!(lines[1].starts_with("WORKER"), "{:?}", lines[1]);
    for l in &lines[2..6] {
        assert!(l.split_whitespace().count() >= 6, "a blank cell: {l:?}");
    }
    let narrow: Vec<String> = b
        .lines_with_map(27, 24)
        .0
        .into_iter()
        .map(|l| l.text.clone())
        .collect();
    assert!(narrow[1].starts_with("WORKER"), "{:?}", narrow[1]);
    assert!(narrow[1].contains("STATE"), "{:?}", narrow[1]);
    assert!(!narrow[1].contains("NODE"), "{:?}", narrow[1]);
    for l in &narrow[2..6] {
        assert!(l.split_whitespace().count() >= 2, "a blank cell: {l:?}");
    }
}

// AC5-HP: the fixture org renders as a layered box-drawing flowchart:
// leads on top, nodes and workers beneath, connector rows between layers,
// blocked-by runs on the node label line, and every box wearing its status
// color. A tap inside a box selects it.
#[test]
fn graph_renders_a_layered_flowchart() {
    use serde_json::json;
    let mut view = org_board_for_graph();
    let colors = crate::client::org_graph::GraphColors::of(&view.theme);
    let b = view.org_board.as_mut().unwrap();
    let graph = b.refresh_graph(80, 24);
    let buf = crate::client::org_graph::frame(&graph, 80, 24, Some("lead:finch"), &colors);
    let mut corners = 0u32;
    let mut connectors = 0u32;
    let mut rows: Vec<String> = vec![String::new(); 24];
    for y in 0..24u16 {
        for x in 0..80u16 {
            let c = buf[(x, y)].symbol().chars().next().unwrap_or(' ');
            match c {
                '\u{2514}' => corners += 1,
                '\u{2534}' | '\u{252c}' => connectors += 1,
                _ => {}
            }
            rows[y as usize].push(c);
        }
    }
    assert_eq!(
        corners as usize,
        graph.boxes.len(),
        "one box per entity (rows: {:?})",
        rows
    );
    assert!(
        connectors >= 1,
        "connector rows join the layers: {:?}",
        rows
    );
    for label in ["finch", "x-1", "x-2", "first", "second", "third"] {
        assert!(
            rows.iter().any(|r| r.contains(label)),
            "label {label} paints inside its box: {rows:?}"
        );
    }
    // The selected lead's box carries the needs-you border and answers a
    // tap anywhere inside it.
    let finch = graph
        .boxes
        .iter()
        .find(|b| b.key == "lead:finch")
        .expect("the lead box exists");
    assert_eq!(
        buf[(finch.col as u16, finch.row as u16)].fg(),
        crate::ratatui_blit::rt_color(colors.needs_you),
        "the selected box's border reads needs-you"
    );
    assert_eq!(
        crate::client::org_graph::hit_test(&graph, 80, 24, finch.col + 1, finch.row + 1),
        Some("lead:finch".to_string()),
        "a tap inside the lead box selects it"
    );

    // Two registry rows sharing one label keep distinct unowned keys.
    let bare = |pane| crate::proto::AgentRow {
        name: "shared label".into(),
        pane_id: Some(pane),
        ..Default::default()
    };
    let keys: Vec<String> = vec![bare(44), bare(45)]
        .iter()
        .enumerate()
        .map(|(i, a)| crate::client::org_graph::unowned_key(a, i))
        .coll)
        .collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(
        keys[0], keys[1],
        "equal labels retain different pane identities"
    );

    // Every box of the ten-lead fleet paints inside a narrow view, and no
    // two boxes overlap: the layered layout degrades by truncation at the
    // width edge, never by collision.
    let mut view2 = View::new(
        (24, 100),
        "main".into(),
        LayoutView {
            squads: Vec::new(),
            active_squad: 0,
            panes: Vec::new(),
            focus: 0,
            area: (0, 0),
            agents: Vec::new(),
            focus_node: None,
        },
    );
    let mut rows = Vec::new();
    let mut agents = Vec::new();
    for i in 0..10 {
        let node = format!("x-{}", i + 1);
        rows.push(json!({"id": node, "status":"ready","title":"Node","scope": format!("t{i}"), "sessions":[{"session_id": format!("s{i}a")},{"session_id": format!("s{i}b")},{"session_id": format!("s{i}c")}]}));
        agents.push(AgentRow {
            name: format!("lead{i}").into(),
            role_scope: Some(format!("t{i}")),
            role_level: Some(1),
            ..Default::default()
        });
        for (w, sid) in [("wa", "a"), ("wb", "b")] {
            agents.push(AgentRow {
                name: format!("{w}{i}").into(),
                harness_session_id: Some(format!("{sid}{i}").into()),
                node: Some(node.clone()),
                ..Default::default()
            });
        }
    }
    let mut scope_nodes = serde_json::Map::new();
    let mut owned = serde_json::Map::new();
    for i in 0..10 {
        scope_nodes.insert(
            format!("t{i}"),
            json!({"status":"ok","nodes":[{"id": format!("x-{}", i + 1)}]}),
        );
        owned.insert(format!("x-{}", i + 1), json!(format!("t{i}")));
    }
    open(&mut view2);
    let gen = view2.org_generation;
    apply_fold(
        &mut view2,
        gen,
        OrgInputs {
            backlog: crate::backlog_model::Inputs {
                rows,
                agents,
                ..Default::default()
            },
            fold: Ok(json!({"scope_nodes": scope_nodes, "owned_scopes": owned})),
            measured_at: now(),
        },
    );
    let b2 = view2.org_board.as_mut().unwrap();
    b2.mode = OrgMode::Graph;
    b2.filter = OrgSessions::Current;
    let graph2 = b2.refresh_graph(120, 24);
    assert_eq!(graph2.boxes.len(), 40, "ten leads, ten nodes, twenty workers");
    let buf2 = crate::client::org_graph::frame(&graph2, 120, 24, Some("lead:lead0"), &colors);
    let mut painted = 0u32;
    for y in 0..24u16 {
        for x in 0..120u16 {
            let c = buf2[(x, y)].symbol().chars().next().unwrap_or(' ');
            if c == '\u{2514}' {
                painted += 1;
            }
        }
    }
    assert_eq!(
        painted as usize,
        graph2.boxes.len(),
        "every box paints its top-left corner"
    );
}

/// The AC5 fixture org as a graph-ready view: one lead, two nodes with
/// x-2 blocked by x-1, and three live workers.
fn org_board_for_graph() -> View {
    use serde_json::json;
    let mut view = View::new(
        (24, 100),
        "main".into(),
        LayoutView {
            squads: Vec::new(),
            active_squad: 0,
            panes: Vec::new(),
            focus: 0,
            area: (0, 0),
            agents: Vec::new(),
            focus_node: None,
        },
    );

    let lead = AgentRow {
        name: "finch".into(),
        role_scope: Some("team".into()),
        role_level: Some(1),
        ..Default::default()
    };
    let worker = |name: &str, sid: &str, node: &str| AgentRow {
        name: name.into(),
        harness_session_id: Some(sid.into()),
        node: Some(node.into()),
        ..Default::default()
    };

    let backlog = crate::backlog_model::Inputs {
        rows: vec![
            json!({"id":"x-1","status":"ready","title":"First","sessions":[{"session_id":"s1"}]}),
            json!({"id":"x-2","status":"ready","title":"Second","blocked_by":["x-1"],"sessions":[{"session_id":"s2"},{"session_id":"s3"}]}),
        ],
        agents: vec![
            lead,
            worker("first", "s1", "x-1"),
            worker("second", "s2", "x-2"),
            worker("third", "s3", "x-2"),
        ],
        ..Default::default()
    };

    open(&mut view);
    let gen = view.org_generation;
    apply_fold(
        &mut view,
        gen,
        OrgInputs {
            backlog,
            fold: Ok(
                json!({"scope_nodes":{"team":{"status":"ok","nodes":[{"id":"x-1"},{"id":"x-2"}]}},"owned_scopes":{"x-1":"team","x-2":"team"}}),
            ),
            measured_at: now(),
        },
    );
    let b = view.org_board.as_mut().unwrap();
    b.mode = OrgMode::Graph;
    b.filter = OrgSessions::Current;
    view
}
