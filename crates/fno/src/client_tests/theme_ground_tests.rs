//! The theme-switch ground repaint and user-theme tests, moved out of
//! client_tests.rs for the shrink-only budget.

use super::tests::two_pane_view;
use super::*;

#[test]
fn apply_theme_stages_the_ground_repaint_and_the_drain_applies_it() {
    // Paint on: a named theme stages OSC 11 bytes and the base as the
    // compositor ground; the drain consumes it, moves the ground, and
    // latches the exit restore.
    let theme = Theme::from_name("footnote-superscript").0;
    let staged = theme_ground::ground_repaint(&theme, true).expect("paint on stages a repaint");
    assert!(
        staged.osc.starts_with(b"\x1b]11;"),
        "OSC 11 leads the set bytes: {:?}",
        String::from_utf8_lossy(&staged.osc[..24.min(staged.osc.len())])
    );
    let hex = crate::theme::color_hex(theme.base).unwrap();
    assert!(
        String::from_utf8_lossy(&staged.osc).contains(hex.as_str()),
        "the theme base rides the set bytes"
    );
    assert_eq!(staged.ground, Some(theme.base));

    // terminal paints no ground: the switch RESTORES the user's scheme.
    let term = Theme::from_name("terminal").0;
    let staged = theme_ground::ground_repaint(&term, true).unwrap();
    assert_eq!(staged.osc, crate::theme::GROUND_RESTORE.to_vec());
    assert_eq!(staged.ground, None);

    // The kill switch off stages nothing - the takeover never ran.
    assert!(theme_ground::ground_repaint(&theme, false).is_none());

    // The INFERRED light pick (no config named a theme; COLORFGBG
    // picked paper) never repaints the ground - the terminal's own light bg
    // and fg stay. An explicit paper pick and the dark default keep it.
    let paper = Theme::from_name("footnote-paper").0;
    assert!(
        !theme_ground::ground_paint_allowed(true, true, &paper),
        "inferred paper must not paint"
    );
    assert!(
        theme_ground::ground_paint_allowed(true, false, &paper),
        "explicit paper still paints"
    );
    assert!(
        theme_ground::ground_paint_allowed(true, true, &theme),
        "the dark default keeps its paint"
    );
    assert!(
        !theme_ground::ground_paint_allowed(false, false, &paper),
        "the kill switch still wins"
    );

    // The drain applies all three effects.
    let mut v = two_pane_view();
    v.pending_ground = Some(theme_ground::PendingGround {
        osc: crate::theme::GROUND_RESTORE.to_vec(),
        ground: Some(theme.base),
    });
    let mut comp = compositor::Compositor::new(None);
    assert_eq!(comp.ground(), None);
    let mut guard = launch::TerminalGuard::test_guard();
    theme_ground::drain_pending_ground(&mut v, &mut comp, &mut guard);
    assert!(v.pending_ground.is_none(), "the drain consumes the stage");
    assert_eq!(comp.ground(), Some(theme.base), "the ground moved");
    assert!(guard.ground_latched(), "exit restore latched");
}
