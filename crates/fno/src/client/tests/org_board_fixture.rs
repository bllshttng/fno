use super::*;
use crate::client::org_seats::tests::fleet_json;
use serde_json::{json, Value};

fn opened(map: Value, agents: Vec<AgentRow>) -> View {
    let mut view = View::new(
        (24, 200),
        "main".into(),
        LayoutView {
            squads: Vec::new(),
            active_squad: 0,
            panes: Vec::new(),
            focus: 0,
            area: (0, 0),
            agents: agents.clone(),
            focus_node: None,
        },
    );
    view.experimental_org_board = true;
    view.board_full = true;
    open(&mut view);
    let gen = view.org_generation;
    let backlog = crate::backlog_model::Inputs {
        agents,
        ..Default::default()
    };
    apply_fold(
        &mut view,
        gen,
        FleetInputs {
            fleet: Ok(map),
            backlog,
            measured_at: now(),
        },
    );
    view
}

fn text(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| l.line.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn press(view: &mut View, bytes: &[u8]) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut sock: Vec<u8> = Vec::new();
    runtime.block_on(keys(view, bytes, &mut sock)).unwrap();
}

fn local_row() -> AgentRow {
    AgentRow {
        name: "mb-03".into(),
        harness: Some("claude".into()),
        harness_session_id: Some("00000003-local-session".into()),
        pane_id: Some(7),
        ..Default::default()
    }
}

#[test]
fn keys_walk_the_tabs_and_go_up_from_a_lead() {
    let mut view = opened(fleet_json(), Vec::new());
    view.org_board.as_mut().unwrap().select("lead/m-bb16/mb-00");
    press(&mut view, b"\r");
    let b = view.org_board.as_ref().unwrap();
    assert_eq!(
        b.key, "seat/m-bb16/mb-00/mb-00",
        "enter on a lead opens its first seat, the lead's own"
    );
    press(&mut view, b"u");
    // Table, Recent, Overview, Graph: three steps right reach Graph.
    press(&mut view, b"lll");
    press(&mut view, b"u");
    let b = view.org_board.as_ref().unwrap();
    assert_eq!(b.tab, OrgTab::Graph);
    assert_eq!(b.key, "host/m-bb16", "u from a lead selects its machine");
    press(&mut view, b"h2");
    assert_eq!(view.org_board.as_ref().unwrap().tab, OrgTab::Recent);
}

#[test]
fn a_remote_seat_names_its_attach_command_and_no_action_key() {
    let mut map = fleet_json();
    // im-0's machine runs a daemon that publishes no seat facts.
    for key in [
        "session_id",
        "model",
        "model_basis",
        "started_at",
        "last_message_at",
        "cwd",
        "project",
        "context_pct",
    ] {
        map["machines"][0]["sessions"][0][key] = Value::Null;
    }
    let mut view = opened(map, vec![local_row()]);
    let b = view.org_board.as_mut().unwrap();
    b.select("seat/m-imac/mb-00/im-0");
    let footer = b.footer(true, 200, now());
    for key in ["enter", "space", "x stop", "P portal"] {
        assert!(
            !footer.contains(key),
            "a remote seat offers {key}: {footer}"
        );
    }
    let page = text(&b.frame(None, 200, 80, now()).right);
    assert!(
        page.contains("fno agents session attach 00000064 on iMac2D2"),
        "{page}"
    );
    for field in ["model:", "started:", "cwd:", "context:"] {
        let line = page.lines().find(|l| l.contains(field)).unwrap();
        assert!(line.contains("not published"), "{line}");
    }
    let last = page.lines().skip_while(|l| !l.contains("Last message"));
    assert!(last.skip(1).next().unwrap().contains("not published"));
    b.select("seat/m-bb16/mb-00/mb-03");
    let footer = b.footer(true, 200, now());
    assert!(
        footer.contains("enter attach") && footer.contains("x stop"),
        "{footer}"
    );
}

#[test]
fn graph_boxes_every_seat_under_its_lead_and_health_reads_not_sampled() {
    let mut view = opened(fleet_json(), Vec::new());
    let b = view.org_board.as_mut().unwrap();
    b.tab = OrgTab::Graph;
    let graph = text(&b.frame(None, 200, 200, now()).right);
    assert_eq!(graph.matches(">_ opus-4").count(), 26, "{graph}");
    for name in ["mb-00", "mb-16", "im-0", "im-8"] {
        assert!(graph.contains(name), "{name} has no box: {graph}");
    }
    let col = |needle: &str| {
        graph
            .lines()
            .find_map(|l| l.find(needle).map(|i| l[..i].chars().count()))
            .unwrap()
    };
    assert_eq!(
        col("im-5") - 3,
        col("\u{2261} no lead@iMac2D2"),
        "a box sits under its lead"
    );
    assert_eq!(col("im-1") - 3, col("\u{2261} mb-00@iMac2D2"));
    b.tab = OrgTab::Health;
    let health = text(&b.frame(None, 200, 200, now()).right);
    assert!(health.contains("health not sampled"), "{health}");
    assert_eq!(
        health.matches("healthy").count(),
        health.matches("not a healthy verdict").count(),
        "no healthy word without its negation: {health}"
    );
}

#[test]
fn a_failed_read_keeps_the_last_good_fleet_and_names_the_failure() {
    let mut view = opened(fleet_json(), Vec::new());
    let gen = view.org_generation;
    let at = now();
    let failed = |fleet| FleetInputs {
        fleet,
        backlog: Default::default(),
        measured_at: at,
    };
    apply_fold(
        &mut view,
        gen,
        failed(Err("fleet top timed out after 30s".into())),
    );
    let b = view.org_board.as_ref().unwrap();
    assert_eq!(b.fleet.as_ref().unwrap().seats().count(), 26);
    assert!(b
        .footer(true, 200, at + 30)
        .contains("fleet not read: fleet top timed out after 30s (failed 30s ago)"));
    apply_fold(
        &mut view,
        gen,
        failed(Ok(json!({"machines": [], "error": "primary down"}))),
    );
    let b = view.org_board.as_ref().unwrap();
    assert_eq!(b.fleet.as_ref().unwrap().seats().count(), 26);
    assert!(b
        .footer(true, 200, at)
        .contains("primary unreadable: primary down"));
    apply_fold(
        &mut view,
        gen.wrapping_sub(1),
        failed(Ok(json!({"machines": []}))),
    );
    let b = view.org_board.as_ref().unwrap();
    assert_eq!(
        b.fleet.as_ref().unwrap().seats().count(),
        26,
        "a stale read never lands"
    );
}

#[test]
fn a_tap_selects_and_a_second_tap_on_a_joined_seat_attaches() {
    let mut view = opened(fleet_json(), vec![local_row()]);
    view.org_board.as_mut().unwrap().select("lead/m-bb16/mb-00");
    // The board fills the terminal less its status row; the bottom bar
    // takes one more.
    let body_h = view.term.0 as usize - 2;
    let frame = view
        .org_board
        .as_ref()
        .unwrap()
        .frame(None, 200, body_h, now());
    let header = frame.right[1].line.text.clone();
    let graph_col = header.find("Graph").unwrap() + 35;
    let seat_row = frame
        .explorer
        .iter()
        .position(|l| l.line.text.contains("mb-03"))
        .unwrap();
    let tap = |row: usize, col: usize| crate::mouse::MouseReport {
        row: row as u16,
        col: col as u16,
        kind: MouseKind::Press(MouseButton::Left),
        shift: false,
        alt: false,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut sock: Vec<u8> = Vec::new();
    runtime.block_on(async {
        mouse(&mut view, tap(1, graph_col), &mut sock)
            .await
            .unwrap();
        assert_eq!(view.org_board.as_ref().unwrap().tab, OrgTab::Graph);
        mouse(&mut view, tap(seat_row, 5), &mut sock).await.unwrap();
        assert_eq!(
            view.org_board.as_ref().unwrap().key,
            "seat/m-bb16/mb-00/mb-03",
            "a tap selects"
        );
        assert!(sock.is_empty());
        mouse(&mut view, tap(seat_row, 5), &mut sock).await.unwrap();
    });
    assert!(view.org_board.is_none(), "a second tap attaches");
}
