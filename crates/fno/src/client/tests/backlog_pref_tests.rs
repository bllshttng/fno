//! The sideline menu's backlog and share rows, the org board gate, and the
//! Settings card-layout toggle.

use super::*;

#[test]
fn sideline_menu_lists_backlog_and_leaves_experimental_to_settings() {
    let menu = build_sideline_menu(Anchor::Center, None, experimental::Flags::default());
    let labels: Vec<(&str, &str)> = menu
        .popup
        .rows
        .iter()
        .filter_map(|row| match row {
            PopupRow::Entry { glyph, label, .. } => Some((glyph.as_str(), label.as_str())),
            _ => None,
        })
        .collect();
    // The experimental toggles live on the settings modal's own tab: a
    // header here read as if every row below it were experimental.
    assert!(!labels
        .iter()
        .any(|(_, l)| *l == "fleet org board" || *l == "pane share"));
    assert!(!labels.iter().any(|(_, l)| l.contains("backlog view")));
    assert!(!menu
        .popup
        .rows
        .iter()
        .any(|row| matches!(row, PopupRow::Header(h) if h == "experimental")));
    let backlog_hint = menu
        .popup
        .rows
        .iter()
        .find_map(|row| match row {
            PopupRow::Entry { label, hint, .. } if label == "backlog" => Some(hint.clone()),
            _ => None,
        })
        .expect("the backlog row is always shown");
    let want = crate::keys::key_for("open-backlog-board")
        .map(|k| format!("prefix {k}"))
        .unwrap_or_default();
    assert_eq!(backlog_hint, want, "the row hint is the live chord");
    // Rows and actions pair by entry index; a swapped pair would run the
    // wrong action.
    let at = |label: &str| labels.iter().position(|(_, l)| *l == label).expect(label);
    assert_eq!(menu.actions[at("backlog")], AuxAction::OpenBacklogView);
    assert_eq!(menu.actions[at("keybindings")], AuxAction::OpenKeybinds);
    let on = build_sideline_menu(
        Anchor::Center,
        None,
        experimental::Flags {
            org_board: true,
            pane_share: true,
            share: None,
        },
    );
    // With pane share on, the menu carries the share row, still paired.
    let at = on
        .popup
        .rows
        .iter()
        .filter(|row| matches!(row, PopupRow::Entry { .. }))
        .position(
            |row| matches!(row, PopupRow::Entry { label, .. } if label == "share focused pane"),
        )
        .expect("the share row shows while pane share is on");
    assert_eq!(on.actions[at], AuxAction::Share(share_flow::Act::Start));
}

#[test]
fn org_board_opens_only_while_its_experimental_flag_is_on() {
    let mut v = two_pane_view();
    v.experimental_org_board = false;
    org_board::open(&mut v);
    assert!(v.org_board.is_none(), "off: nothing opens");
    assert!(
        v.notice
            .as_ref()
            .is_some_and(|(notice, _)| notice.contains("fleet org board is off")),
        "off: the notice names where to enable it"
    );
    v.experimental_org_board = true;
    org_board::open(&mut v);
    assert!(v.org_board.is_some(), "on: the board opens");
}

#[test]
fn settings_general_lists_the_sideline_card_layout_toggle() {
    let mut v = two_pane_view();
    v.settings_tab = SettingsTab::General;
    let modal = v.build_settings_modal();
    assert!(modal.popup.rows.iter().any(|row| matches!(
        row,
        PopupRow::Entry { label, .. } if label == "sideline card layout"
    )));
    assert!(modal.actions.contains(&AuxAction::ToggleSidelineLayout));
}

#[test]
fn sideline_menu_names_the_sweep_entry_off_dead() {
    let menu = build_sideline_menu(Anchor::Center, None, experimental::Flags::default());
    let i = menu
        .popup
        .rows
        .iter()
        .position(|row| {
            matches!(
                row,
                PopupRow::Entry { glyph, label, .. }
                    if glyph == "♺" && label == "sweep threads"
            )
        })
        .expect("sweep threads entry");
    let action_i = menu
        .popup
        .rows
        .iter()
        .take(i + 1)
        .filter(|row| matches!(row, PopupRow::Entry { .. }))
        .count()
        - 1;
    assert_eq!(menu.actions[action_i], AuxAction::OpenSweep);
    assert!(crate::popup::menu_glyph_is_bmp("♺"));
    assert!(!crate::popup::menu_glyph_is_bmp("📄"));
    assert_eq!(
        menu.actions
            .iter()
            .filter(|action| **action == AuxAction::Detach)
            .count(),
        1,
        "the global detach slot remains distinct"
    );
}
