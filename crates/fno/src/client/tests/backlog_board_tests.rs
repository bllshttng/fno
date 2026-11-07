//! The backlog board's view tests, mounted by `backlog_board.rs`. Fixtures
//! build `Inputs` the way `backlog_model_tests.rs` does: pure functions
//! over fixture rows, no store, no process.

use super::*;
use crate::backlog_model;
use serde_json::json;

fn board_inputs() -> backlog_model::Inputs {
    let mut inp = backlog_model::Inputs::default();
    inp.backend = "graph".into();
    inp.order = vec!["x-1".into(), "x-2".into(), "x-3".into()];
    inp.rows = vec![
        json!({"id": "x-1", "status": "ready", "priority": "p1", "title": "First card", "project": "fno",
               "cwd": "/tmp/x519f", "sessions": [
                   {"phase": "execute", "harness": "claude",
                    "session_id": "4d4ea752-1063-4515-8366-b9946ed8f64d"}]}),
        json!({"id": "x-2", "status": "in_progress", "priority": "p2", "title": "mux card", "project": "fno"}),
        json!({"id": "x-3", "status": "ready", "priority": "p2", "title": "Other project", "project": "other"}),
    ];
    inp.flow = json!({"available": false, "reason": "fixture"});
    inp
}

fn board_with(inputs: backlog_model::Inputs) -> BoardView {
    let mut b = BoardView::new(0);
    b.inputs = Some(inputs);
    let q = b.query.to_query().expect("the default query parses");
    b.body = Some(backlog_model::board(b.inputs.as_ref().unwrap(), &q));
    b
}

// AC6-HP: before any gather the body says `reading board...`, never an
// empty board.
#[test]
fn board_render_rows() {
    let b = BoardView::new(0);
    let (lines, follow) = render(&b, 120);
    assert_eq!(lines[0], "reading board...");
    assert!(follow.is_none());

    let b = board_with(board_inputs());
    let (lines, _) = render(&b, 200);
    let stats = &lines[0];
    for word in [
        "In Progress",
        "Now",
        "Next",
        "Later",
        "Triage",
        "Done",
        "│",
        "flow: fixture",
    ] {
        assert!(stats.contains(word), "stats line missing {word}: {stats}");
    }

    let mut b = board_with(board_inputs());
    let before = b.body.as_ref().unwrap().lanes.len();
    let mut bad = board_inputs();
    bad.rows_error = Some("the store read failed".into());
    b.inputs = Some(bad);
    rederive(&mut b);
    assert_eq!(
        b.body.as_ref().unwrap().lanes.len(),
        before,
        "last good board kept"
    );
    assert!(b.errors.iter().any(|e| e.contains("the store read failed")));
    let (lines, _) = render(&b, 200);
    assert!(lines.iter().any(|l| l.starts_with("! ")), "{lines:?}");

    let mut inp = board_inputs();
    inp.flow = json!({"available": false, "reason": "no usable window start"});
    let b = board_with(inp);
    let (lines, _) = render(&b, 200);
    assert!(
        lines[0].contains("flow: no usable window start"),
        "{:?}",
        lines[0]
    );

    let mut inp = board_inputs();
    inp.flow = json!({
        "available": true,
        "window": {"since_days": 28},
        "deliveries": {"weeks": [{"week_start": "2026-11-02", "code": 0, "doc": 0}]},
        "cycle": {
            "available": false,
            "reason": "no merged PR dates in window"
        },
        "open_prs": {"count": 1}
    });
    let b = board_with(inp);
    let (lines, _) = render(&b, 200);
    assert!(
        lines
            .first()
            .is_some_and(|line| line.contains("No deliveries this week")),
        "{:?}",
        lines.first()
    );
    assert!(
        lines.first().is_some_and(|line| {
            line.contains("cycle unavailable: no merged PR dates in window")
        }),
        "{:?}",
        lines.first()
    );

    let b = board_with(board_inputs());
    let (lines, _) = render(&b, 80);
    let stacked = lines
        .iter()
        .filter(|l| {
            l.starts_with("In Progress  ") || l.starts_with("Now  ") || l.starts_with("Next  ")
        })
        .count();
    assert!(stacked >= 3, "expected stacked headers, got {lines:?}");

    let mut b = board_with(board_inputs());
    b.query.q = Some("zzz-no-such-card".into());
    rederive(&mut b);
    let total: usize = b
        .body
        .as_ref()
        .unwrap()
        .lanes
        .iter()
        .map(|l| l.cells.iter().map(|c| c.total).sum::<usize>())
        .sum();
    assert_eq!(total, 0, "no cards match");
    let (lines, _) = render(&b, 120);
    assert!(
        lines.iter().any(|l| l.contains("no cards match")),
        "{lines:?}"
    );

    // The keyed grammar reaches the board through the Find input; a bad
    // keyed query names the parse error and keeps the previous filter.
    let mut v = key_view(board_with(board_inputs()));
    v.backlog_board.as_mut().expect("board open").input =
        Some((BoardInputKind::Find, "s:ready".into()));
    input_commit(&mut v);
    let b = v.backlog_board.as_ref().expect("board open");
    let total: usize = b
        .body
        .as_ref()
        .unwrap()
        .lanes
        .iter()
        .map(|l| l.cells.iter().map(|c| c.total).sum::<usize>())
        .sum();
    assert_eq!(total, 2, "only the ready cards stay");
    assert_eq!(b.query.q.as_deref(), Some("s:ready"));
    let mut v = key_view(board_with(board_inputs()));
    {
        let b = v.backlog_board.as_mut().expect("board open");
        b.query.q = Some("s:ready".into());
        b.input = Some((BoardInputKind::Find, "stauts:ready".into()));
    }
    input_commit(&mut v);
    let b = v.backlog_board.as_ref().expect("board open");
    assert_eq!(
        b.query.q.as_deref(),
        Some("s:ready"),
        "the previous filter stays"
    );
    let notice = v
        .notice
        .as_ref()
        .map(|(text, _)| text.clone())
        .unwrap_or_default();
    assert!(
        notice.contains("did you mean 'status:'?"),
        "notice: {notice}"
    );
}

// AC4-HP: the stats line counts every column and renders the flow line.

// AC5-HP: `L` cycles project -> epic -> none and keeps the cursor on the
// same card while the new grouping still shows it.
#[test]
fn lanes_cycle_keeps_the_cursor_card() {
    let mut b = board_with(board_inputs());
    first_card(&mut b);
    let card = cursor_card_id(&b);
    cycle_lanes_b(&mut b);
    assert!(matches!(b.query.lanes, backlog_model::LanesBy::Epic));
    assert_eq!(cursor_card_id(&b), card, "cursor keeps its card");
    cycle_lanes_b(&mut b);
    assert!(matches!(b.query.lanes, backlog_model::LanesBy::None));
    cycle_lanes_b(&mut b);
    assert!(
        matches!(b.query.lanes, backlog_model::LanesBy::Project),
        "third press returns to project"
    );
}

// AC6-ERR: a failed read never repaints a good board empty.

// AC7-EDGE: an unavailable flow renders its reason, never numbers.

// AC8-EDGE: below WIDE_CELLS_AT the cells stack with `Now  12` headers.

// AC11-EDGE: a filter matching nothing totals zero and says so.

// The `t` key's view flow: a board wrapped in a live View with a wire
// buffer standing in for the socket.
fn key_view(b: BoardView) -> View {
    let mut v = super::super::tests::two_pane_view();
    v.term = (24, 80);
    v.backlog_board = Some(b);
    v
}

// x-1 carries a cwd so the prefill can select the node's project.
fn target_inputs() -> backlog_model::Inputs {
    let mut inp = board_inputs();
    if let Some(r) = inp.rows.get_mut(0) {
        r["cwd"] = json!("/r/footnote");
    }
    inp
}

// AC10-HP: `t` on an unclaimed card closes the board, prefills the dock
// with /fno:target <id> and the node's project, and writes NOTHING to the
// wire - nothing spawns before the operator's Launch press.
#[test]
fn t_key_rows() {
    let mut b = board_with(target_inputs());
    focus_card(&mut b, Some("x-1"));
    let mut v = key_view(b);
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        board_keys(&mut v, b"t", &mut sock).await.expect("t folds");
    });
    assert!(v.backlog_board.is_none(), "the board closes");
    assert!(sock.is_empty(), "nothing spawns on t");
    let l = v.launcher.as_ref().expect("the dock is open");
    assert_eq!(l.draft.message, "/fno:target x-1");
    let idx = l.draft.project_idx;
    assert_eq!(l.draft.projects[idx], "/r/footnote");
    assert_eq!(l.draft.node.as_deref(), Some("x-1"));

    let mut b = board_with(board_inputs());
    focus_card(&mut b, Some("x-2")); // status in_progress -> claimed
    let mut v = key_view(b);
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        board_keys(&mut v, b"t", &mut sock).await.expect("t folds");
    });
    assert!(
        v.backlog_board.is_some(),
        "the board stays open on the refusal"
    );
    assert!(v.launcher.is_none(), "the dock never opens");
    let notice = v.notice.as_ref().map(|(t, _)| t.as_str()).unwrap_or("");
    assert_eq!(
        notice,
        "x-2 is already being worked; open its session instead"
    );

    let mut b = board_with(target_inputs());
    focus_card(&mut b, Some("x-1"));
    let mut v = key_view(b);
    open_detail(&mut v);
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        node_detail::detail_keys(&mut v, b"t", &mut sock)
            .await
            .expect("t folds");
    });
    assert!(v.backlog_board.is_none(), "the board closes");
    let l = v.launcher.as_ref().expect("the dock is open");
    assert_eq!(l.draft.message, "/fno:target x-1");

    let mut b = board_with(target_inputs());
    focus_card(&mut b, Some("x-1"));
    let mut v = key_view(b);
    // The dock is already open holding a draft.
    super::super::agent_launcher::open(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.draft.message = "fix the flake".into();
        l.draft.revision += 1;
    }
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        board_keys(&mut v, b"t", &mut sock).await.expect("t folds");
    });
    let l = v.launcher.as_ref().expect("the dock stays open");
    assert_eq!(l.draft.message, "fix the flake", "the kept draft survives");
    let notice = v.notice.as_ref().map(|(t, _)| t.as_str()).unwrap_or("");
    assert!(notice.contains("holds a draft"), "notice: {notice}");
}
// `F` toggles the full-screen board and back; the first Esc folds the
// full screen back to the column, the second closes the board.

// The docked board's column owns no agents rows. A press anywhere in it
// resolves no sideline row, no drag source, and no chrome hit - the board is
// keyboard-driven, and a click must never act on a phantom agent row.
#[test]
fn board_column_resolves_no_agents_rows_or_chrome_hits() {
    let v = sideline_backlog_view();
    // A cell well inside the board column (panel 28 wide), below the strip.
    assert_eq!(v.sideline_row_at(10, 14), None);
    assert!(v.row_drag_source_at(10, 14).is_none());
    assert!(v.press_hold_row_at(10, 14).is_none());
    assert!(v.chrome_hit(10, 14).is_none());
}

// the board is a modal like the composer - prefix chords still
// resolve while it holds the keyboard (which-key parity). Before the fix the
// prefix byte fell into the board's byte catch-all: `^B C` toggled nothing
// and `^B ?` opened the board's own keys overlay instead of the keybinds.
#[test]
fn chord_rows() {
    let mut v = key_view(board_with(board_inputs()));
    // "Holds the keyboard" is now explicit: a windowed board owns the input
    // only after the operator opened or clicked it, which in the real flow
    // also points the sideline at the backlog view.
    v.region_owner = crate::client::region_focus::RegionOwner::Board;
    v.sideline_view = crate::view_store::SidelineView::Backlog;
    let mut scanner = crate::keys::Scanner::default();
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let flow =
            super::super::overlay_keys::route(&mut v, &mut scanner, &[0x02, b'C'], &mut sock)
                .await
                .expect("the board owns the chunk")
                .expect("route runs");
        assert!(matches!(flow, StdinFlow::Continue));
    });
    assert!(v.org.is_expanded(), "^B C opened the org fold");
    assert!(v.backlog_board.is_some(), "the board survives the chord");
    // The org modal holds the keyboard while it is up; close it so the
    // next chord reaches the board.
    v.aux = None;
    // `^B ?` opens the GLOBAL keybinds, never the board's own keys overlay
    // (that stays on the bare key).
    let mut scanner = crate::keys::Scanner::default();
    rt.block_on(async {
        super::super::overlay_keys::route(&mut v, &mut scanner, &[0x02, b'?'], &mut sock)
            .await
            .expect("the board owns the chunk")
            .expect("route runs");
    });
    assert!(
        v.keys_modal.is_none() && v.backlog_board.as_ref().is_some_and(|b| b.keys_footer),
        "^B ? opened the board's footer grid, not the modal"
    );
    assert!(
        v.backlog_board
            .as_ref()
            .map(|b| !b.keys_overlay)
            .unwrap_or(false),
        "the board's keys overlay did not arm behind the chord"
    );

    let mut v = key_view(board_with(board_inputs()));
    // The board holds the keyboard until the composer chord hands it over;
    // the sideline rides the backlog view as every real open does.
    v.region_owner = crate::client::region_focus::RegionOwner::Board;
    v.sideline_view = crate::view_store::SidelineView::Backlog;
    let mut scanner = crate::keys::Scanner::default();
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        super::super::overlay_keys::route(&mut v, &mut scanner, &[0x02, b'i'], &mut sock)
            .await
            .expect("the board owns the chunk")
            .expect("route runs");
    });
    assert!(v.launcher.is_some(), "^B i opened the composer");
    assert!(v.backlog_board.is_some(), "the board stays docked");
    let mut scanner = crate::keys::Scanner::default();
    rt.block_on(async {
        super::super::overlay_keys::route(&mut v, &mut scanner, &[b'\t'], &mut sock)
            .await
            .expect("the composer owns the chunk")
            .expect("route runs");
    });
    let l = v.launcher.as_ref().expect("the composer is still open");
    assert_ne!(
        l.focus,
        super::agent_launcher::Focus::Harness,
        "Tab cycled the composer's tab; the byte reached the composer, not the board"
    );
    assert!(v.backlog_board.is_some(), "the board stays docked");
}

// a chord that opens a lower-priority modal over the docked board hands the
// keyboard to that modal: `^B i` opens the composer, and a Tab then cycles
// the composer's tab - it never falls through to the board's folder.

// The composed frame paints the backlog inside the sideline column: the
// filter bar, the board pane and the detail pane are the column's
// content, title rows carrying the focus mark instead of frames.
#[tokio::test]
async fn compose_rows() {
    let mut v = key_view(board_with(board_inputs()));
    v.sideline_view = crate::view_store::SidelineView::Backlog;
    let text = crate::vt::frame_text(&v.compose());
    assert!(text.contains("Search:"), "filter bar: {text}");
    assert!(text.contains("In Progress"), "column header: {text}");
    assert!(text.contains("mux card"), "card row: {text}");

    let mut v = key_view(board_with(board_inputs()));
    v.sideline_view = crate::view_store::SidelineView::Backlog;
    v.board_full = true;
    let text = crate::vt::frame_text(&v.compose());
    assert!(
        text.lines().any(|l| l.starts_with("Search:")),
        "filter bar at column 0: {text}"
    );
    assert!(
        text.lines().any(|l| l.starts_with("backlog \u{b7} kanban")),
        "board title at column 0: {text}"
    );
    assert!(
        text.lines().any(|l| l.contains("details \u{b7} x-")),
        "detail title paints: {text}"
    );
    assert!(
        !text.contains('\u{256d}'),
        "no box-drawing frame glyph anywhere: {text}"
    );
    assert!(
        !text.contains("e/p/s/S edit") && text.contains("c cols"),
        "hint carries the column key and no edit keys: {text}"
    );
    // One esc chip on the full board (the filter bar's, top right); a tap
    // returns it to the docked column, and a tap on the column's chip,
    // keyboard elsewhere, closes the column. The chip rides no frame now,
    // so the tap goes through the real press path at the recorded span.
    let chip = tap_recorded_chip(&mut v).await;
    assert_eq!(chip, 1, "one chip on the full board");
    assert!(
        !v.board_full && v.backlog_board.is_some(),
        "full returns to docked"
    );
    v.region_owner = crate::client::region_focus::RegionOwner::Pane;
    let chip = tap_recorded_chip(&mut v).await;
    assert_eq!(chip, 1, "one chip on the docked column");
    assert!(v.backlog_board.is_none(), "the docked column closes");

    // A press on a painted node id opens the node the sideline card tap's
    // way: full-screen and docked, the span the paint recorded drills into
    // the details pane (the fixture cards carry no plan and no link). The
    // shape gate needs a real-shaped id, so the mux card renames to one.
    let mut inp = board_inputs();
    inp.order[1] = "x-24c8".into();
    if let Some(r) = inp.rows.get_mut(1) {
        r["id"] = json!("x-24c8");
    }
    let mut v = key_view(board_with(inp));
    v.sideline_view = crate::view_store::SidelineView::Backlog;
    v.board_full = true;
    v.compose();
    let s = painted_id("x-24c8");
    let (off_row, off_col) = (s.row, s.col - 1);
    press_span(&mut v, s.row, s.col).await;
    let d = v
        .backlog_board
        .as_ref()
        .expect("board stays open")
        .detail
        .as_ref()
        .expect("the tap drills in");
    assert_eq!(d.node_id, "x-24c8");
    assert!(
        !backlog_style::painted_spans()
            .iter()
            .any(|sp| { sp.row == off_row && off_col >= sp.col && off_col < sp.col + sp.len }),
        "the glyph column records no span"
    );

    v.board_full = false;
    v.backlog_board.as_mut().expect("board open").detail = None;
    v.compose();
    let s = painted_id("x-24c8");
    press_span(&mut v, s.row, s.col).await;
    let d = v
        .backlog_board
        .as_ref()
        .expect("board stays open")
        .detail
        .as_ref()
        .expect("the docked tap drills in");
    assert_eq!(d.node_id, "x-24c8");

    // The cascade's middle leg: no plan in the vault opens the link the
    // node stores, through the PR tap's opener. The fixture link names no
    // openable scheme, so the attempt lands as the refusal notice - and
    // the details pane never opens over it.
    v.backlog_board.as_mut().expect("board open").detail = None;
    v.backlog = vec![crate::proto::BacklogCard {
        id: "x-24c8".into(),
        slug: "mux-card".into(),
        priority: "p2".into(),
        state: crate::proto::CardState::Ready,
        pane_id: None,
        attach_id: None,
        where_hint: None,
        project: None,
        lane: None,
        plan_path: None,
        head: false,
        link: Some("linear:x-24c8".into()),
    }];
    crate::client::node_link::open(&mut v, "x-24c8".into()).await;
    let notice = v
        .notice
        .as_ref()
        .map(|(text, _)| text.clone())
        .unwrap_or_default();
    assert!(
        notice.contains("refused to open linear:x-24c8"),
        "the link leg runs the PR opener: {notice}"
    );
    assert!(
        v.backlog_board
            .as_ref()
            .expect("board open")
            .detail
            .is_none(),
        "the link outranks the details pane"
    );
}

/// Press the topmost recorded esc chip through the real mouse path: the
/// press lands on the recorded span, `chip_at` routes it as Esc.
async fn tap_recorded_chip(v: &mut View) -> usize {
    v.compose();
    let chips: Vec<crate::chrome::CloseSpan> = v
        .close_chips
        .borrow()
        .iter()
        .copied()
        .filter(|s| s.len == 3)
        .collect();
    let Some(s) = chips.last() else {
        return 0;
    };
    let (row, col) = (s.row as u16, s.col as u16 + 1);
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let mut scanner = crate::keys::Scanner::default();
    for kind in [
        crate::proto::MouseKind::Press(crate::proto::MouseButton::Left),
        crate::proto::MouseKind::Release(crate::proto::MouseButton::Left),
    ] {
        let rep = crate::mouse::MouseReport {
            kind,
            row,
            col,
            shift: false,
            alt: false,
        };
        crate::client::region_focus::mouse_pre_pass(v, &mut scanner, vec![rep], &mut sock)
            .await
            .unwrap();
    }
    chips.len()
}

/// The recorded span of one painted node id.
fn painted_id(id: &str) -> backlog_style::NodeSpan {
    backlog_style::painted_spans()
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .expect("card id painted")
}

/// One left press through the real mouse path at painted screen coords.
async fn press_span(v: &mut View, row: usize, col: usize) {
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let mut scanner = crate::keys::Scanner::default();
    crate::client::region_focus::mouse_pre_pass(
        v,
        &mut scanner,
        vec![crate::mouse::MouseReport {
            kind: crate::proto::MouseKind::Press(crate::proto::MouseButton::Left),
            row: row as u16,
            col: col as u16,
            shift: false,
            alt: false,
        }],
        &mut sock,
    )
    .await
    .unwrap();
}

// Full screen paints the filter bar, the board pane and the two-row hint;
// the hint carries the edit keys (the footer's replacement).

// ----: D1/D2 proof - distinct attributes, no INVERSE, real frames ----

/// A view whose sideline shows the fixture backlog, like the operator's.
fn sideline_backlog_view() -> View {
    let (rows, cols) = (24u16, 100u16);
    let mut view = View::new(
        (rows, cols),
        "main".into(),
        LayoutView {
            squads: vec![SquadMeta {
                id: 1,
                name: "main".into(),
                canonical_cwd: "/code/main".into(),
                tabs: vec![TabMeta {
                    id: 0,
                    name: "0".into(),
                    named: false,
                    panes: Vec::new(),
                }],
                active_tab: 0,
                panes: 1,
            }],
            active_squad: 1,
            panes: vec![(
                10,
                Rect {
                    x: 0,
                    y: 0,
                    rows: rows - 1,
                    cols: cols - 28,
                },
            )],
            focus: 10,
            area: (rows - 1, cols - 28),
            agents: vec![],
            focus_node: None,
        },
    );
    view.sideline_view = crate::view_store::SidelineView::Backlog;
    view.backlog_board = Some(board_with(board_inputs()));
    view
}

fn cell_at(frame: &crate::proto::Frame, r: usize, c: usize, cols: usize) -> crate::proto::Cell {
    frame.cells[r * cols + c]
}

// The hierarchy proof: in the composed full-screen board, a column header
// cell, a card-id cell, a meta cell and the cursor band carry DISTINCT
// attribute sets, and no cell carries INVERSE.
#[test]
fn board_cells_rows() {
    let mut view = sideline_backlog_view();
    view.board_full = true;
    let frame = view.compose();
    let cols = 100usize;
    let text = crate::vt::frame_text(&frame);
    // The cursor card row and the stacked column header directly above
    // it (the panes share rows, so anchors stay within the board pane).
    let band_row = text
        .lines()
        .enumerate()
        .find(|(_, l)| {
            // Pad-width agnostic: the frame's side padding may grow.
            let body = l.trim_start_matches('│').trim_start();
            body.starts_with("▸●") && l.contains("x-2")
        })
        .expect("cursor card row")
        .0;
    let head_row = band_row - 1;
    let head_line = text.lines().nth(head_row).expect("head line");
    assert!(
        head_line
            .trim_start_matches('│')
            .trim_start()
            .starts_with("In Progress"),
        "column header above the cursor row: {head_line}"
    );
    let head_col = head_line.find("In Progress").expect("head text") + 1;
    let head = cell_at(&frame, head_row, head_col, cols);
    assert!(
        head.flags & crate::proto::cell_flags::BOLD != 0,
        "header bold at row {head_row}: {text}"
    );
    // The stats line (contains `·`) reads dim.
    let meta_row = text
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("In Progress 1 \u{b7}"))
        .expect("stats line")
        .0;
    let meta = cell_at(&frame, meta_row, 2, cols);
    assert_eq!(meta.fg, crate::proto::Color::Indexed(8), "meta dim slot");
    let band_line = text.lines().nth(band_row).expect("band line");
    let id_col = band_line.find("x-2").expect("id on the cursor row");
    let band = cell_at(&frame, band_row, id_col, cols);
    let (cursor_fg, cursor_bg, _) = crate::cursor::cursor_style(&view.theme);
    assert_eq!(band.bg, cursor_bg, "band surface");
    // The cursor row text is the one cursor pair, never the brand.
    assert_eq!(band.fg, cursor_fg, "band accent text");
    // A non-cursor card id keeps the accent slot and a plain title.
    let id_row = text
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("x-1") && l.contains("First card"))
        .expect("x-1 card row")
        .0;
    let line = text.lines().nth(id_row).expect("id row");
    let id_col = line.find("x-1").expect("id on its row");
    let id_cell = cell_at(&frame, id_row, id_col, cols);
    assert_eq!(
        id_cell.fg,
        crate::proto::Color::Indexed(3),
        "id accent slot"
    );
    let title_col = line.find("First card").expect("title on its row");
    let title_cell = cell_at(&frame, id_row, title_col, cols);
    assert_eq!(title_cell.fg, crate::proto::Color::Default, "title plain");
    // Nothing in the frame is inverse.
    for r in 0..24usize {
        for c in 0..cols {
            let cell = cell_at(&frame, r, c, cols);
            assert!(
                cell.flags & crate::proto::cell_flags::INVERSE == 0,
                "INVERSE at {r},{c}: {text}"
            );
        }
    }

    let mut view = sideline_backlog_view();
    view.board_full = true;
    let frame = view.compose();
    let text = crate::vt::frame_text(&frame);
    assert!(
        text.lines().any(|l| l.starts_with("backlog \u{b7} kanban")),
        "board title at column 0: {text}"
    );
    assert!(text.contains("In Progress"), "{text}");
}

// The full-screen board keeps the hierarchy on the terminal's own bg.

// Evidence shots (FNO_UX_SHOTS): the sideline column, the full board and
// the node detail, each composed for real.
#[test]
fn ux_shot_rows() {
    use crate::frame_html::write_shot;
    let view = sideline_backlog_view();
    let frame = view.compose();
    write_shot(
        &frame,
        "ux-shot-backlog-sideline",
        "the backlog as a sideline view",
    );

    let mut view = sideline_backlog_view();
    view.board_full = true;
    let frame = view.compose();
    write_shot(
        &frame,
        "ux-shot-backlog-full-board",
        "the full-screen backlog board",
    );

    let mut view = sideline_backlog_view();
    if let Some(b) = view.backlog_board.as_mut() {
        b.detail = Some(node_detail::NodeDetailOverlay {
            node_id: "x-1".into(),
            trail: vec![],
            sel: 0,
            scroll: 0,
        });
    }
    let frame = view.compose();
    write_shot(&frame, "ux-shot-backlog-detail", "the node detail overlay");
}

// The same frames under the user's theme (Catppuccin) were evidence shots
// with no assertion behind them; the palette contract lives in
// `backlog_panel_cells_carry_distinct_attributes` and the theme's own
// tests. Removed under the shrink-only test cap: the two variants guarded
// no contract of their own.

#[test]
fn comment_key_opens_input_and_dead_edit_keys() {
    // c opens the comment input from the detail pane; the seven dead edit
    // keys open nothing from either surface.
    let mut v = key_view(board_with(board_inputs()));
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Some(b) = v.backlog_board.as_mut() {
        b.detail = Some(node_detail::NodeDetailOverlay {
            node_id: "x-1".into(),
            trail: vec![],
            sel: 0,
            scroll: 0,
        });
    }
    if let Some(b) = v.backlog_board.as_mut() {
        focus_card(b, Some("x-1"));
    }
    rt.block_on(async {
        node_detail::detail_keys(&mut v, b"c", &mut sock)
            .await
            .expect("c folds");
    });
    assert_eq!(
        v.backlog_board
            .as_ref()
            .expect("board")
            .input
            .as_ref()
            .map(|(k, _)| *k),
        Some(BoardInputKind::Comment),
        "c from the detail opens the comment input"
    );
    for key in [b'e', b'p', b's', b'S', b'D', b'N', b'E'] {
        let mut v = key_view(board_with(board_inputs()));
        if let Some(b) = v.backlog_board.as_mut() {
            b.detail = Some(node_detail::NodeDetailOverlay {
                node_id: "x-1".into(),
                trail: vec![],
                sel: 0,
                scroll: 0,
            });
        }
        if let Some(b) = v.backlog_board.as_mut() {
            focus_card(b, Some("x-1"));
        }
        let mut sock: Vec<u8> = Vec::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _ = node_detail::detail_keys(&mut v, &[key], &mut sock).await;
            let _ = board_keys(&mut v, &[key], &mut sock).await;
        });
        let b = v.backlog_board.as_ref().expect("board");
        assert!(
            b.input.is_none(),
            "dead key {key} opens nothing: {:?}",
            b.input
        );
    }
}
// AC4-HP: Space on value rows builds a multi-select set; the board keeps
// cards in either status; `any` clears the set.
#[test]
fn facet_rows() {
    let mut inp = board_inputs();
    if let Some(r) = inp.rows.get_mut(2) {
        r["status"] = json!("done");
    }
    let mut v = key_view(board_with(inp));
    // The status facet's value list sorts: any, done, in_progress, ready.
    if let Some(b) = v.backlog_board.as_mut() {
        b.facet = Some(FacetPick {
            facet: 2,
            sel: 0,
            value_sel: Some(3), // ready
        });
    }
    facet_toggle(&mut v);
    if let Some(b) = v.backlog_board.as_mut() {
        b.facet = Some(FacetPick {
            facet: 2,
            sel: 0,
            value_sel: Some(2), // in_progress
        });
    }
    facet_toggle(&mut v);
    let b = v.backlog_board.as_ref().expect("board open");
    let set = b.query.sets.get("status").expect("status set built");
    assert!(set.contains(&"ready".to_string()) && set.contains(&"in_progress".to_string()));
    let kept: Vec<&str> = b
        .body
        .as_ref()
        .unwrap()
        .lanes
        .iter()
        .flat_map(|l| l.cells.iter())
        .flat_map(|c| c.cards.iter())
        .map(|c| c.id.as_str())
        .collect();
    let mut sorted = kept.clone();
    sorted.sort();
    assert_eq!(sorted, ["x-1", "x-2"], "either status stays, done drops");
    // Both rows carry the ticked mark.
    let b = v.backlog_board.as_mut().expect("board open");
    b.facet = Some(FacetPick {
        facet: 2,
        sel: 0,
        value_sel: Some(0),
    });
    let popup = facet_popup(b).expect("value popup");
    let marks: Vec<&str> = popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Entry { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .filter(|l| l.starts_with('['))
        .collect();
    assert_eq!(
        marks,
        vec!["[ ] done", "[x] in_progress", "[x] ready"],
        "{marks:?}"
    );
    // `any` clears the set.
    facet_toggle(&mut v);
    let b = v.backlog_board.as_ref().expect("board open");
    assert!(b.query.sets.get("status").is_none(), "any clears the set");

    let b = board_with(board_inputs());
    let board = b.body.as_ref().unwrap();
    let lines = filter_bar_lines(&b, board, 120);
    assert!(
        lines
            .iter()
            .any(|l| l.text.contains("Labels: none on any node")),
        "{lines:?}"
    );

    let b = board_with(board_inputs());
    let names: Vec<&str> = visible_facets(b.body.as_ref().unwrap())
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert!(!names.contains(&"tag"), "empty tag facet hides: {names:?}");
    let mut inp = board_inputs();
    if let Some(r) = inp.rows.get_mut(0) {
        r["tags"] = json!(["infra"]);
    }
    let mut b = board_with(inp);
    rederive(&mut b);
    let names: Vec<&str> = visible_facets(b.body.as_ref().unwrap())
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert!(names.contains(&"tag"), "a tagged row reveals the facet");
}

// AC3-EDGE bar half: with no tags anywhere, the bar names the hidden facet.

// AC3-EDGE picker half: the tag facet hides while empty and shows once a
// row carries one.

// The paint memos must rebuild only when their key moves. A build
// counter makes the contract mechanical: same key, one build; any key
// field, a rebuild. Both slots answer to the same contract, so one test
// walks both.
#[test]
fn memo_rebuilds_only_when_the_key_moves() {
    let b = board_with(board_inputs());
    let builds = std::cell::Cell::new(0);
    let body_key = |gen: u64, row: usize| crate::client::backlog_board::BodyKey {
        gen,
        lane: 0,
        col: 0,
        row,
        w: 120,
        view: backlog_model::View::Kanban,
        query: None,
        errors: 0,
        columns: vec!["ready".into()],
    };
    let detail_key =
        |mtime: Option<std::time::SystemTime>| crate::client::backlog_board::DetailKey {
            gen: 1,
            node: "x-1".into(),
            sel: None,
            w: 80,
            doc: Some(("x-1".into(), "/plans/x-1.md".into(), mtime, String::new())),
        };
    let build = |builds: &std::cell::Cell<usize>| {
        builds.set(builds.get() + 1);
        (Vec::new(), None)
    };
    let _ = b.board_body_cached(body_key(0, 0), || build(&builds));
    let _ = b.board_body_cached(body_key(0, 0), || build(&builds));
    assert_eq!(builds.get(), 1, "same key reads the memo");
    let _ = b.board_body_cached(body_key(0, 1), || build(&builds));
    assert_eq!(builds.get(), 2, "a moved cursor rebuilds");
    let _ = b.board_body_cached(body_key(1, 1), || build(&builds));
    assert_eq!(builds.get(), 3, "a new read rebuilds");
    // The detail slot keys on the document identity: a doc re-read (mtime
    // or node move) rebuilds even with the same node and read.
    let _ = b.detail_lines_cached(detail_key(None), || build(&builds));
    let _ = b.detail_lines_cached(detail_key(None), || build(&builds));
    assert_eq!(builds.get(), 4, "same identity reads the memo");
    let later = std::time::SystemTime::now();
    let _ = b.detail_lines_cached(detail_key(Some(later)), || build(&builds));
    assert_eq!(builds.get(), 5, "a re-read doc rebuilds");
}
/// The board remembers its state. Set a filter, a search text, the
/// list view and a selection, close, reopen: the fresh board holds the same
/// query, and its first gather parks the cursor back on the saved card.
#[test]
fn board_remembers_filters_search_view_and_selection_across_reopen() {
    let prefs = tempfile::tempdir().expect("tempdir");
    crate::view_store::set_test_path(prefs.path());
    // Session one: filter to priority p2, search "mux", list view, one
    // lane, cursor on x-2.
    let mut b = board_with(board_inputs());
    b.query.sets.insert("priority", vec!["p2".into()]);
    b.query.q = Some("mux".into());
    b.query.view = backlog_model::View::List;
    b.query.lanes = backlog_model::LanesBy::None;
    rederive(&mut b);
    focus_card(&mut b, Some("x-2"));
    assert_eq!(cursor_card_id(&b).as_deref(), Some("x-2"), "fixture cursor");
    save_board_prefs(&mut b);
    drop(b);
    // Close, reopen through the real open path: the query comes back, the
    // saved card rides along as the pending focus.
    let mut v = key_view(board_with(board_inputs()));
    v.backlog_board = None;
    backlog_board_open_fresh(&mut v);
    {
        let b = v.backlog_board.as_ref().expect("reopen opens the board");
        assert_eq!(
            b.query.sets.get("priority").map(Vec::as_slice),
            Some(&["p2".to_string()][..])
        );
        assert_eq!(b.query.q.as_deref(), Some("mux"));
        assert_eq!(b.query.view, backlog_model::View::List);
        assert_eq!(b.query.lanes, backlog_model::LanesBy::None);
        assert_eq!(b.pending_focus.as_deref(), Some("x-2"));
    }
    // The first gather parks the cursor on the saved card (x-2 is the one
    // card the restored filter set keeps).
    let gen = v.backlog_board.as_ref().expect("open").gen;
    apply_fold(
        &mut v,
        gen,
        BoardMsg::Gathered {
            inputs: board_inputs(),
        },
    );
    let b = v.backlog_board.as_ref().expect("gather keeps the board");
    assert_eq!(
        cursor_card_id(b).as_deref(),
        Some("x-2"),
        "selection restored"
    );
}

#[test]
fn tab_cycles_kanban_list_and_graph_views() {
    let mut inputs = board_inputs();
    if let Some(row) = inputs.rows.get_mut(1) {
        if let Some(obj) = row.as_object_mut() {
            obj.insert("parent".into(), json!("x-1"));
        }
    }
    inputs.rows.push(json!({
        "id": "x-4", "status": "ready", "priority": "p2",
        "title": "Rate limit the checkout api per key", "project": "fno", "parent": "x-1"
    }));
    inputs.order.push("x-4".into());
    inputs.rows.push(json!({
        "id": "x-done", "status": "done", "priority": "p2", "parent": "x-1"
    }));
    inputs.order.push("x-done".into());
    let mut view = key_view(board_with(inputs));
    toggle_view(&mut view);
    assert_eq!(
        view.backlog_board.as_ref().unwrap().query.view,
        backlog_model::View::List
    );
    toggle_view(&mut view);
    assert_eq!(
        serde_json::to_value(view.backlog_board.as_ref().unwrap().query.view).unwrap(),
        "graph"
    );
    let b = view.backlog_board.as_ref().unwrap();
    let (lines, follow) = render(b, 120);
    assert!(
        follow.is_some_and(|line| lines.get(line).is_some_and(|row| !row.band)),
        "the graph cursor uses its node border, not a stripe across other nodes"
    );
    let painted = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let cursor_rows = lines
        .iter()
        .filter(|line| {
            line.roles
                .iter()
                .any(|role| matches!(role, crate::text_style::TextStyle::Tab))
        })
        .count();
    assert_eq!(
        cursor_rows, 5,
        "exactly the focused box wears the inverse cursor border: {painted}"
    );
    assert!(
        painted.contains("family"),
        "graph header missing: {painted}"
    );
    assert!(
        painted.contains("x-1") && painted.contains("x-2"),
        "family nodes missing: {painted}"
    );
    assert!(
        painted.contains("Rate limit the checkout api") && painted.contains("per key"),
        "the graph keeps a long title readable across its node lines: {painted}"
    );
    assert!(painted.contains("─"), "graph connectors missing: {painted}");
    assert_eq!(
        cursor_card_id(b).as_deref(),
        Some("x-2"),
        "Tab keeps the focused card"
    );
    move_row(&mut view, true);
    assert_eq!(
        cursor_card_id(view.backlog_board.as_ref().unwrap()).as_deref(),
        Some("x-4"),
        "j moves to the next node in the layer"
    );
    move_col(&mut view, false);
    assert_eq!(
        edit_target(view.backlog_board.as_ref().unwrap()).as_deref(),
        Some("x-1"),
        "h moves to the parent layer and keeps actions on its node"
    );
    move_col(&mut view, true);
    move_row(&mut view, true);
    move_row(&mut view, true);
    let (lines, follow) = render(view.backlog_board.as_ref().unwrap(), 120);
    assert!(
        follow.is_some(),
        "a focused Done stub remains visible: {lines:?}"
    );
}

/// `x` (reset filters) returns the query to `any` and the store's
/// memory to the defaults, so the next open starts clean too.
#[test]
fn reset_filters_returns_every_filter_to_any_and_clears_the_memory() {
    let prefs = tempfile::tempdir().expect("tempdir");
    crate::view_store::set_test_path(prefs.path());
    let mut v = key_view(board_with(board_inputs()));
    {
        let b = v.backlog_board.as_mut().expect("fixture board");
        b.query.sets.insert("status", vec!["ready".into()]);
        b.query.q = Some("mux".into());
        save_board_prefs(b);
    }
    reset_filters(&mut v);
    let b = v.backlog_board.as_ref().expect("reset keeps the board");
    assert!(b.query.sets.is_empty());
    assert_eq!(b.query.q, None);
    assert_eq!(b.query.view, backlog_model::View::Kanban);
    assert_eq!(b.query.lanes, backlog_model::LanesBy::Project);
    assert_eq!(b.pending_focus, None);
    let remembered = crate::view_store::load_board_query().expect("reset saves the defaults");
    assert_eq!(remembered.sets.len(), 0);
    assert_eq!(remembered.q, None);
}

#[test]
fn w_opens_the_hq_page_only_where_the_browser_can_reach_it() {
    use crate::client::org_board::hq_url;
    use crate::web::LiveBridge;
    let bridge = |base: &str, loopback| {
        Ok(LiveBridge {
            base: base.into(),
            token: "tok".into(),
            loopback,
        })
    };
    let local = hq_url(bridge("http://127.0.0.1:7681", true), false, "bb16");
    assert_eq!(local.unwrap(), "http://127.0.0.1:7681/hq?t=tok");
    let none = hq_url(Err("no web bridge for session s".into()), false, "bb16");
    assert_eq!(none.unwrap_err(), "no web bridge for session s");
    let ssh = hq_url(bridge("http://127.0.0.1:7681", true), true, "iMac2D2");
    assert!(ssh
        .unwrap_err()
        .contains("loopback of iMac2D2; restart it with --bind 0.0.0.0"));
    let wide = hq_url(bridge("http://127.0.0.1:7681", false), true, "iMac2D2");
    assert_eq!(wide.unwrap(), "http://iMac2D2:7681/hq?t=tok");
}
