//! The sideline footer row: three buttons, each opening what its key opens.

use super::*;

#[test]
fn footer_buttons_open_what_their_keys_open() {
    // The footer row is three buttons: `+ session` opens the composer
    // (prefix+i), `+ workspace` the create overlay (prefix+N), `menu` the
    // MENU popup (prefix+P). A wide panel shows each label with its key;
    // the default panel keeps every seat with the keys alone.
    let mut v = two_pane_view();
    v.term = (30, 100);
    let footer = v
        .display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::NewSquad))
        .unwrap();
    // (x-cd67 US1; the strip row since R15) outer row == display index -
    // offset + 1.
    let trow = (footer - v.sideline_offset()) as u16 + 1;
    for (width, want) in [
        (28u16, "i \u{b7} N \u{b7} P"),
        (60, "+ session (i) \u{b7} + workspace (N) \u{b7} menu (P)"),
    ] {
        v.sideline_width = width;
        let panel_w = v.panel_w() as usize;
        let line = frame_text(&v.compose())
            .lines()
            .nth(trow as usize)
            .unwrap_or_default()
            .to_string();
        assert!(line.starts_with(want), "width {width}: {line:?}");
        let buttons = v.footer_buttons(panel_w);
        let (session, menu) = (
            buttons.session.expect("session seat"),
            buttons.menu.expect("menu seat"),
        );
        assert!(matches!(
            v.chrome_hit(trow, session.start as u16),
            Some(ChromeHit::ToggleComposer)
        ));
        assert!(matches!(
            v.chrome_hit(trow, menu.start as u16),
            Some(ChromeHit::OpenSidelineMenu { .. })
        ));
        assert!(matches!(
            v.chrome_hit(trow, session.end as u16 + 3),
            Some(ChromeHit::OpenCreate)
        ));
    }
}

#[tokio::test]
async fn agents_tab_space_reaches_the_reply() {
    // Space on an agent row docks the peek under the list with the reply
    // input focused; Esc in the reply closes the dock whole.
    let mut done = agent_row("w1", 4, Some(AgentBadge::Done), false);
    done.harness = Some("codex".into());
    let mut v = wide_view(vec![done]);
    v.sideline_width = 60;
    let at = v
        .display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::Agent(_)))
        .unwrap();
    v.selector = Some(at);
    let mut buf: Vec<u8> = Vec::new();
    selector_keys(&mut v, b" ", &mut buf).await.unwrap();
    assert_eq!(v.peek_input.as_ref().map(|(n, _)| n.as_str()), Some("w1"));
    assert!(v.peek_is_docked(), "the peek docks under the list");
    let lines = vec![" w1".to_string()];
    let chrome = chrome::Chrome::new("w1", Anchor::Center);
    let layout = v.peek_layout(&chrome, &lines);
    assert_eq!(layout.origin.1, 0, "flush with the sideline's left edge");
    assert!(layout.framed.width < v.panel_w() as usize);
    // A press inside the dock is the dock's, never the hidden row's.
    assert!(modal_mouse(&mut v, left_click(layout.origin.0 as u16 + 1, 1)));
    // On an odd column height a full dock never covers the answered row.
    let long: Vec<String> = (0..99).map(|i| format!(" line {i}")).collect();
    v.term.0 = 29;
    for cursor in 0..29 {
        v.peek.as_mut().unwrap().cursor = cursor;
        let l = v.peek_layout(&chrome, &long);
        let row = cursor.saturating_sub(v.sideline_offset()) + TAB_BAR_ROWS as usize;
        let seat = l.origin.0..l.origin.0 + l.framed.lines.len();
        assert!(!seat.contains(&row), "cursor {cursor}");
    }

    // The Done word wears the theme's ok color, not the lane color.
    let st = agent_lattice_state(&v.layout.agents[0]);
    assert_eq!(st, LatticeState::DoneUnseen);
    let ok = lattice_style(st, v.theme.needs_you, v.theme.ok).fg;
    assert_eq!(agent_lane_fg(&v.layout.agents[0], st, ok), v.theme.ok);

    peek_keys(&mut v, b"\x1b", &mut buf).await.unwrap();
    // A lone Esc is held for a split sequence; an empty read flushes it.
    peek_keys(&mut v, b"", &mut buf).await.unwrap();
    assert!(v.peek.is_none() && !v.peek_docked, "esc closes the dock");

    // A wide panel's session button reads the task prompt.
    let text = v.footer_buttons(100).text;
    assert!(
        text.starts_with("+ describe a task for a new session"),
        "{text:?}"
    );
}
