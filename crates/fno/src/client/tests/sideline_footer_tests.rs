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
