//! Mouse-routing tests: what `route_mouse` decides per pane mode, the SGR
//! encoding of forwarded events, the scroll-fold classification, and the
//! v111 alt-click link open on a mouse-reporting pane. Lives beside
//! `server::mouse_route` + `open_link` (file-budget): the tests travel with
//! the code they answer.

use super::*;
use crate::server::mouse_route::MOUSE_WHEEL_LINES;
// This module is `server::tests`'s SIBLING (both are declared in server.rs),
// so the fixture helpers must be pub(super) and named through `tests`.
use super::tests::{empty_core, seen_test_core};

fn mouse_mode() -> Modes {
    Modes {
        mouse_click: true,
        sgr_mouse: true,
        ..Modes::default()
    }
}

#[test]
fn mouse_rows() {
    // AC3-HP: a click-mode app (?1000) gets wheel/press/release passthrough
    // and consumes nothing mux-side...
    for kind in [
        MouseKind::WheelUp,
        MouseKind::Press(MouseButton::Left),
        MouseKind::Release(MouseButton::Left),
    ] {
        assert_eq!(route_mouse(mouse_mode(), kind), MouseAction::Passthrough);
    }
    // ...but a drag it never requested is ignored, not forwarded and not
    // mux-interpreted (fighting the app's own handling).
    assert_eq!(
        route_mouse(mouse_mode(), MouseKind::Drag(MouseButton::Left)),
        MouseAction::Ignore
    );
    // A drag-mode app (?1002) does get the drag.
    let drag_mode = Modes {
        mouse_drag: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    assert_eq!(
        route_mouse(drag_mode, MouseKind::Drag(MouseButton::Left)),
        MouseAction::Passthrough
    );

    // US1/US2: a plain shell pane scrolls and selects mux-side.
    let plain = Modes::default();
    assert_eq!(
        route_mouse(plain, MouseKind::WheelUp),
        MouseAction::Scroll(MOUSE_WHEEL_LINES)
    );
    assert_eq!(
        route_mouse(plain, MouseKind::WheelDown),
        MouseAction::Scroll(-MOUSE_WHEEL_LINES)
    );
    assert_eq!(
        route_mouse(plain, MouseKind::Press(MouseButton::Left)),
        MouseAction::SelectStart
    );
    assert_eq!(
        route_mouse(plain, MouseKind::Drag(MouseButton::Left)),
        MouseAction::SelectUpdate
    );
    assert_eq!(
        route_mouse(plain, MouseKind::Release(MouseButton::Left)),
        MouseAction::SelectRelease
    );
    assert_eq!(
        route_mouse(plain, MouseKind::Press(MouseButton::Right)),
        MouseAction::Ignore
    );

    // A mouse-reporting app that never negotiated SGR is not sent garbage;
    // the mux interprets instead (Domain: SGR-only passthrough).
    let legacy = Modes {
        mouse_click: true,
        sgr_mouse: false,
        ..Modes::default()
    };
    assert_eq!(
        route_mouse(legacy, MouseKind::WheelUp),
        MouseAction::Scroll(MOUSE_WHEEL_LINES)
    );

    // Left press at pane-local (row 4, col 9) -> SGR button 0, 1-based coords.
    let press = sgr_mouse_bytes(&MouseEvent {
        row: 4,
        col: 9,
        kind: MouseKind::Press(MouseButton::Left),
        alt: false,
    });
    assert_eq!(press, b"\x1b[<0;10;5M");
    // Release terminates with lowercase m.
    let release = sgr_mouse_bytes(&MouseEvent {
        row: 4,
        col: 9,
        kind: MouseKind::Release(MouseButton::Left),
        alt: false,
    });
    assert_eq!(release, b"\x1b[<0;10;5m");
    // Drag adds the motion bit (32).
    let drag = sgr_mouse_bytes(&MouseEvent {
        row: 0,
        col: 0,
        kind: MouseKind::Drag(MouseButton::Left),
        alt: false,
    });
    assert_eq!(drag, b"\x1b[<32;1;1M");
    // Wheel up is button 64; an alt modifier adds bit 8 to every forwarded
    // code, so an unclaimed alt click reaches the app byte-identical.
    let wheel = sgr_mouse_bytes(&MouseEvent {
        row: 2,
        col: 3,
        kind: MouseKind::WheelUp,
        alt: false,
    });
    assert_eq!(wheel, b"\x1b[<64;4;3M");
    let alt_press = sgr_mouse_bytes(&MouseEvent {
        row: 2,
        col: 3,
        kind: MouseKind::Press(MouseButton::Left),
        alt: true,
    });
    assert_eq!(alt_press, b"\x1b[<8;4;3M");
}

// AC1-ERR: a wheel tick routed to a pane absent from the panes map is a
// no-op through mouse() - the top-of-fn early return fires, so no PTY write
// and no gate-state entry for the dead id.
#[test]
fn mouse_wheel_dead_pane_is_noop() {
    let mut core = empty_core();
    core.mouse(
        1,
        999,
        MouseEvent {
            row: 0,
            col: 0,
            kind: MouseKind::WheelDown,
            alt: false,
        },
    );
    assert!(
        core.wheel_gate.is_empty(),
        "no gate state is created for a dead pane"
    );
}

// v111: in a mouse-reporting pane the SelectRelease open path is unreachable
// (route_mouse passes every press/release through), so an alt click over a
// link is the mux's open gesture there. Plain clicks on the same cell keep
// forwarding to the app untouched; reaping drops a pending pair.
#[test]
fn alt_click_over_link_in_mouse_pane_opens_and_plain_click_forwards() {
    let (mut core, cid, p1, _p2, mut rx) = seen_test_core();
    {
        let e = core.panes.get_mut(&p1).unwrap();
        // The URL lands at row 0, col 0; the mode sets are what Claude Code
        // and the codex TUI both emit.
        e.vt.feed(b"https://github.com/x/y/pull/1\r\n");
        e.vt.feed(b"\x1b[?1000h\x1b[?1006h");
    }
    let click = |alt| MouseEvent {
        row: 0,
        col: 0,
        kind: MouseKind::Press(MouseButton::Left),
        alt,
    };
    let release = |alt| MouseEvent {
        row: 0,
        col: 0,
        kind: MouseKind::Release(MouseButton::Left),
        alt,
    };

    // A plain click pair forwards to the app and opens nothing.
    core.mouse(cid, p1, click(false));
    assert!(
        core.link_click_claims.is_empty(),
        "an unmodified press never claims"
    );
    core.mouse(cid, p1, release(false));
    let plain_urls: Vec<String> = {
        let mut urls = Vec::new();
        while let Ok(m) = rx.try_recv() {
            if let ServerMsg::OpenLink { url } = m {
                urls.push(url);
            }
        }
        urls
    };
    assert!(
        plain_urls.is_empty(),
        "a plain click on a mouse-reporting pane never opens: {plain_urls:?}"
    );

    // An alt click pair claims on press and opens on release.
    core.mouse(cid, p1, click(true));
    assert_eq!(
        core.link_click_claims.get(&p1),
        Some(&(0, 0, cid)),
        "an alt press over a link cell claims the pair"
    );
    core.mouse(cid, p1, release(true));
    assert!(
        core.link_click_claims.is_empty(),
        "the release consumes the claim"
    );
    let alt_urls: Vec<String> = {
        let mut urls = Vec::new();
        while let Ok(m) = rx.try_recv() {
            if let ServerMsg::OpenLink { url } = m {
                urls.push(url);
            }
        }
        urls
    };
    assert_eq!(
        alt_urls,
        vec!["https://github.com/x/y/pull/1".to_string()],
        "the release answered OpenLink with the clicked link"
    );

    // Reaping the pane drops a pending pair (the release can never come).
    core.mouse(cid, p1, click(true));
    assert_eq!(core.link_click_claims.len(), 1);
    core.reap_pane(p1);
    assert!(
        core.link_click_claims.is_empty(),
        "a dead pane's claim is purged"
    );
}

#[test]
fn scroll_delta_classifies_only_interpreted_wheel_ticks() {
    // The fold is only correct if it folds real scrolls and STOPS at anything
    // else. A plain pane's wheel is an interpreted scroll (foldable, +/-
    // MOUSE_WHEEL_LINES); a left press is a selection (not foldable) so it must
    // break the run and preserve order.
    let (core, _cid, p1, _p2, _rx) = seen_test_core();
    let ev = |kind| MouseEvent {
        row: 1,
        col: 1,
        kind,
        alt: false,
    };
    assert_eq!(
        core.scroll_delta(p1, &ev(MouseKind::WheelUp)),
        Some(MOUSE_WHEEL_LINES)
    );
    assert_eq!(
        core.scroll_delta(p1, &ev(MouseKind::WheelDown)),
        Some(-MOUSE_WHEEL_LINES)
    );
    assert_eq!(
        core.scroll_delta(p1, &ev(MouseKind::Press(MouseButton::Left))),
        None,
        "a select must stop the fold, not coalesce into it"
    );
    assert_eq!(
        core.scroll_delta(999, &ev(MouseKind::WheelUp)),
        None,
        "an unknown pane never folds"
    );
}
