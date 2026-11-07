//! The overlay pad walk: every overlay demo reachable in-process composes
//! with the shared chrome's pad guard clean. The guard lives in
//! `chrome::pad_violations` and `frame` asserts it on every paint, so a
//! regression here panics the demo that regressed it; this walk also fails
//! a demo whose overlay paints nothing, which would prove nothing.

use super::*;

/// A demo view wide enough for the centered modals, with one live agent row
/// so the sideline and a work pane paint behind the overlays.
fn demo_view() -> View {
    let mut v = view_with_agents(vec![agent_row("w", 10, Some(AgentBadge::Working), false)]);
    v.term = (24, 100);
    v
}

#[test]
fn every_overlay_demo_paints_with_the_pad_guard_clean() {
    // (demo, opener). Each opener flips the same view state the live keys
    // keep, so the compose paints the real overlay path. The pad guard runs
    // inside `frame` on every paint: a framed row that puts text beside a
    // border glyph fails the demo that painted it.
    type Opener = Box<dyn FnOnce(&mut View)>;
    let mut demos: Vec<(&str, Opener)> = Vec::new();
    demos.push((
        "confirm",
        Box::new(|v: &mut View| {
            v.confirm = Some(ConfirmAction {
                action: ConfirmKind::CloseTab { tab: 1 },
                label: "close tab".into(),
            });
        }),
    ));
    demos.push((
        "catch up",
        Box::new(|v: &mut View| v.digest = Some(vec!["catch-up line".into()])),
    ));
    demos.push((
        "keys",
        Box::new(|v: &mut View| v.keys_modal = Some(build_keys_modal())),
    ));
    demos.push((
        "row menu",
        Box::new(|v: &mut View| {
            let row = agent_row_at(v, |a| a.name == "w");
            assert!(v.open_row_menu(row, Anchor::Center));
        }),
    ));
    demos.push((
        "settings",
        Box::new(|v: &mut View| v.aux = Some(v.build_settings_modal())),
    ));
    demos.push(("needs me", Box::new(|v: &mut View| v.answers = Some(0))));
    demos.push((
        "rename tab",
        Box::new(|v: &mut View| v.rename = Some((RenameTarget::Tab(0), "x".into()))),
    ));
    demos.push((
        "new workspace",
        Box::new(|v: &mut View| v.create = Some("demo".into())),
    ));
    demos.push((
        "connections",
        Box::new(|v: &mut View| {
            v.connections = Some(crate::connections_view::ConnectionsView::new());
        }),
    ));
    demos.push((
        "feed overlay",
        Box::new(|v: &mut View| {
            v.feed = Some(crate::client::feed_view::open_overlay(None, 0));
        }),
    ));
    for (name, open) in demos {
        let mut v = demo_view();
        let base = screen_of(&v);
        open(&mut v);
        let after = screen_of(&v);
        assert_ne!(base, after, "{name} painted nothing");
    }
}

/// The composed screen of a demo view, as text.
fn screen_of(v: &View) -> String {
    frame_text(&v.compose())
}
