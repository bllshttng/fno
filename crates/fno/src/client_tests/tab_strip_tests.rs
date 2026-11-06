//! The tab strip: label shapes, rollup folds, the Ｆ[no] brand mark and the
//! stamp, and the accent surviving INVERSE on the active tab.

use super::*;

#[test]
fn tab_bar_spans_label_named_tabs_and_collapse_bare_digits() {
    // x-0f9d US2 (supersedes x-c150 Locked 5): an UNNAMED tab renders
    // today's ordinal span byte-identically; a CHOSEN name renders ALONE,
    // no forced ordinal, truncated to TAB_LABEL_W.
    let mut view = two_pane_view();
    let spans = view.tab_bar_spans();
    // spans[0] is the pinned Ｆ[no] mark; the squad label follows, then tabs.
    assert_eq!(
        spans[2].text, " 1 ",
        "unnamed digit collapse: zero regression"
    );
    // The active tab keeps one cell of padding inside each bracket.
    assert_eq!(spans[3].text, "[ 2 ]");
    view.layout.squads[0].tabs[0].name = "x-abcd".into();
    view.layout.squads[0].tabs[0].named = true;
    view.layout.squads[0].tabs[1].name = "a-very-long-worktree-name".into();
    view.layout.squads[0].tabs[1].named = true;
    let spans = view.tab_bar_spans();
    assert_eq!(spans[2].text, " x-abcd ", "chosen name renders alone");
    assert_eq!(
        spans[3].text, "[ a-very-long-wo ]",
        "name alone truncates to 14, padded inside the brackets"
    );
}
#[test]
fn tab_rollup_folds_worst_live_state_ignoring_exited() {
    // Empty tab -> no rollup (AC2-EDGE).
    assert_eq!(tab_rollup_state(&[], 1, 0), None);
    // Live-idle -> the outline `○`: the tab state machine distinguishes a
    // live-idle tab from a dead one (only "no live panes" omits the glyph).
    assert_eq!(
        tab_rollup_state(&[tab_agent(Some(0), None, false)], 1, 0),
        Some(LatticeState::Idle)
    );
    // All-exited -> no rollup: exited panes are filtered before the fold,
    // leaving no live panes, so the tab renders stateless (AC2-EDGE).
    assert_eq!(
        tab_rollup_state(&[tab_agent(Some(0), Some(AgentBadge::Blocked), true)], 1, 0),
        None
    );
    // Worst-first: a blocked pane beats a working one in the same tab.
    assert_eq!(
        tab_rollup_state(
            &[
                tab_agent(Some(0), Some(AgentBadge::Working), false),
                tab_agent(Some(0), Some(AgentBadge::Blocked), false),
            ],
            1,
            0
        ),
        Some(LatticeState::Blocked)
    );
    // A pane in a DIFFERENT tab never leaks into this tab's rollup.
    assert_eq!(
        tab_rollup_state(
            &[tab_agent(Some(1), Some(AgentBadge::Blocked), false)],
            1,
            0
        ),
        None
    );
}

#[test]
fn tab_strip_rollup_surfaces_hidden_attention_with_accent() {
    // AC2-HP: a background (inactive) tab whose only pane is Blocked shows a
    // leading `▲` in the accent color at the strip, without opening it.
    let mut view = two_pane_view();
    view.layout
        .agents
        .push(tab_agent(Some(0), Some(AgentBadge::Blocked), false));
    let spans = view.tab_bar_spans();
    // spans[0] = the pinned mark, [1] = squad name, [2] = tab 0 (blocked,
    // inactive), [3] = tab 1 (no live panes).
    assert_eq!(spans[2].text, " ▲ 1 ", "blocked tab: label preceded by ▲");
    assert_eq!(
        spans[2].fg, LATTICE_ACCENT,
        "blocked rollup carries the accent"
    );
    assert_eq!(
        spans[2].flags & cell_flags::BOLD,
        cell_flags::BOLD,
        "blocked rollup carries BOLD"
    );
    // AC2-EDGE: a tab with no live panes shows no rollup glyph and no accent -
    // byte-identical to a pre-feature stateless tab.
    assert_eq!(spans[3].text, "[ 2 ]");
    assert_eq!(spans[3].fg, Color::Default);
}

#[test]
fn tab_strip_pins_the_brand_mark_in_every_workspace() {
    // US4/AC3-HP, restated by the 2026-09-27 ruling: the Ｆ[no] mark is
    // PINNED at the strip's top-left, before tab 1, in every workspace; the
    // workspace label follows as its own span (brand_label passthrough).
    assert_eq!(brand_label("fno"), "F[no]");
    assert_eq!(brand_label("footnote"), "footnote");
    let mut view = two_pane_view();
    let spans = view.tab_bar_spans();
    assert_eq!(
        spans[0].text, " Ｆ[no] ",
        "the pinned mark leads the strip in every workspace"
    );
    assert_eq!(
        spans[1].text, " footnote ",
        "the workspace label follows the mark"
    );
    // Renaming the workspace never moves or duplicates the mark.
    view.layout
        .squads
        .iter_mut()
        .find(|s| s.id == view.layout.active_squad)
        .expect("active squad")
        .name = "fno".into();
    let spans = view.tab_bar_spans();
    assert_eq!(spans[0].text, " Ｆ[no] ");
    assert_eq!(spans[1].text, " F[no] ");
}
