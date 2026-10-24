//! Keyboard-scroll (v118) server-op tests: the shared-offset moves and
//! edge anchors ride the SAME offset the wheel drives. Split out of
//! `server_tests.rs` (file budget).

use super::*;

#[test]
fn pane_scroll_moves_shared_offset_and_edges_anchor() {
    // The scroll-mode wire ops move the SAME shared offset the wheel drives,
    // and the edge arm anchors exactly (g = oldest history line, G = live
    // bottom) - never past either clamp.
    let (mut core, _cid, p1, _p2, _rx) = seen_test_core();
    let hist = 60;
    core.panes
        .get_mut(&p1)
        .unwrap()
        .vt
        .feed("row\r\n".repeat(hist).as_bytes());
    assert_eq!(core.scroll_offset(p1), 0, "starts at the live bottom");

    // j/k-equivalent relative moves clamp both ways.
    core.pane_scroll(_cid, p1, 5);
    assert_eq!(core.scroll_offset(p1), 5, "5 lines up into history");
    core.pane_scroll(_cid, p1, -10);
    assert_eq!(core.scroll_offset(p1), 0, "10 lines down clamps at live");

    // A missing pane is a notice, never a panic.
    core.pane_scroll(_cid, 9999, 3);
    core.pane_scroll_edge(_cid, 9999, true);

    // g/G: top anchors at the full history (36 = 60 fed lines minus the
    // 24-row viewport), bottom snaps to live.
    core.pane_scroll(_cid, p1, 5);
    core.pane_scroll_edge(_cid, p1, true);
    assert_eq!(
        core.scroll_offset(p1),
        hist - 24,
        "top anchors at the oldest history line"
    );
    core.pane_scroll_edge(_cid, p1, false);
    assert_eq!(core.scroll_offset(p1), 0, "bottom snaps to live");
}
