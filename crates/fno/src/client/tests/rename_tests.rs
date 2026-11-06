//! The rename-overlay test family, in its own module. `use super::*` reaches
//! the tests module's imports (two_pane_view, rename_keys, blocked_row, ...).

use super::*;

#[tokio::test]
async fn rename_keys_enter_sends_the_typed_name_for_the_captured_tab() {
    // AC2-HP (client half): type + Enter -> one RenameTab for the tab id
    // captured at open time.
    let mut v = two_pane_view();
    v.open_rename(RenameTarget::Tab(7));
    let mut buf: Vec<u8> = Vec::new();
    rename_keys(&mut v, b"debug\r", &mut buf).await.unwrap();
    let mut cur = std::io::Cursor::new(buf);
    let msg: ClientMsg = crate::proto::read_msg_sync(&mut cur).unwrap();
    assert_eq!(
        msg,
        ClientMsg::Command(Command::RenameTab {
            tab: 7,
            name: "debug".into()
        })
    );
    assert_eq!(v.rename, None, "submit closes the overlay");
}

// Agent-target rename: grammar-filtered buffer, refused empty
// send, and the captured-label wire command.

#[tokio::test]
async fn rename_keys_enter_sends_rename_agent_for_the_captured_label() {
    let mut v = two_pane_view();
    v.open_rename_seeded(RenameTarget::Agent("old-label".into()), "old-label".into());
    // Editing starts from the seeded current label: backspace pops the
    // last three chars, the typed tail completes the new one.
    let mut sent: Vec<u8> = Vec::new();
    rename_keys(&mut v, b"\x7f\x7f\x7ftwo\r", &mut sent)
        .await
        .unwrap();
    let mut cur = std::io::Cursor::new(sent);
    let msg: ClientMsg = crate::proto::read_msg_sync(&mut cur).unwrap();
    assert_eq!(
        msg,
        ClientMsg::Command(Command::RenameAgent {
            name: "old-label".into(),
            new_name: "old-latwo".into()
        })
    );
    assert_eq!(v.rename, None, "submit closes the overlay");
}

#[tokio::test]
async fn rename_keys_agent_target_filters_non_grammar_bytes() {
    let mut v = two_pane_view();
    v.open_rename_seeded(RenameTarget::Agent("w1".into()), "w1".into());
    // Space and symbol are OUTSIDE the registry grammar: they never enter
    // the buffer, so what is typed is what the server would keep.
    let mut sent: Vec<u8> = Vec::new();
    rename_keys(&mut v, b"a b!2", &mut sent).await.unwrap();
    let (_, name) = v.rename.clone().unwrap();
    assert_eq!(name, "w1ab2", "only grammar bytes land in the buffer");
}
#[tokio::test]
async fn selector_r_refuses_external_agent_rename() {
    let mut row = blocked_row("daemon-row", 4, None);
    row.external = true;
    let mut v = view_with_agents(vec![row]);
    let idx = agent_row_at(&v, |a| a.name == "daemon-row");
    v.selector = Some(idx);
    let mut sent: Vec<u8> = Vec::new();
    selector_keys(&mut v, b"r", &mut sent).await.unwrap();
    assert!(sent.is_empty(), "an external row cannot be renamed");
    assert!(v.rename.is_none(), "an external row opens no overlay");
    assert_eq!(
        v.notice.as_ref().map(|(message, _)| message.as_str()),
        Some("an external row's name belongs to its claude session")
    );
}
#[tokio::test]
async fn rename_agent_menu_entry_opens_the_seeded_overlay() {
    let mut v = view_with_agents(vec![blocked_row("worker-x", 3, None)]);
    let idx = agent_row_at(&v, |a| a.name == "worker-x");
    assert!(v.open_row_menu(idx, Anchor::Center));
    let key = crate::keys::menu_byte_for("rename-agent").expect("rename-agent is registered");
    let hint = v
        .row_menu
        .as_ref()
        .expect("row menu open")
        .popup
        .rows
        .iter()
        .find_map(|row| match row {
            PopupRow::Entry { label, hint, .. } if label == "Rename" => Some(hint.clone()),
            _ => None,
        })
        .expect("rename entry has a drawn hint");
    assert_eq!(hint, crate::keys::menu_key_for("rename-agent").unwrap());
    let pos = v
        .row_menu
        .as_ref()
        .expect("row menu open")
        .actions
        .iter()
        .position(|a| matches!(a, MenuAction::RenameAgent))
        .expect("rename entry present on a plain fno row");
    let mut sent: Vec<u8> = Vec::new();
    assert_eq!(
        crate::keys::menu_byte_for("rename-agent"),
        Some(key),
        "the drawn key is the dispatched key"
    );
    assert!(pos < v.row_menu.as_ref().unwrap().actions.len());
    row_menu_keys(&mut v, &[key], &mut sent).await.unwrap();
    let (target, seed) = v.rename.clone().expect("rename overlay opened");
    match target {
        RenameTarget::Agent(name) => {
            assert_eq!(name, "worker-x", "the captured label is the row's");
            assert_eq!(seed, "worker-x", "the buffer is seeded with the label");
        }
        other => panic!("expected an agent rename target, got {other:?}"),
    }
}
#[tokio::test]
async fn rename_keys_empty_enter_still_sends_the_clear() {
    // Locked 2 / AC3-HP: Enter on an EMPTY buffer sends (blank = reset to
    // auto) - the one deliberate divergence from create_keys.
    let mut v = two_pane_view();
    v.open_rename(RenameTarget::Tab(7));
    let mut buf: Vec<u8> = Vec::new();
    rename_keys(&mut v, b"\r", &mut buf).await.unwrap();
    let mut cur = std::io::Cursor::new(buf);
    let msg: ClientMsg = crate::proto::read_msg_sync(&mut cur).unwrap();
    assert_eq!(
        msg,
        ClientMsg::Command(Command::RenameTab {
            tab: 7,
            name: String::new()
        })
    );
    assert_eq!(v.rename, None);
}

#[tokio::test]
async fn rename_keys_esc_cancels_without_sending_and_swallows_the_tail() {
    // AC1-UI: Esc closes, sends nothing; same-chunk bytes after the Esc
    // die with the overlay instead of leaking into the pane.
    let mut v = two_pane_view();
    v.open_rename(RenameTarget::Tab(7));
    let mut buf: Vec<u8> = Vec::new();
    rename_keys(&mut v, b"deb\x1bx", &mut buf).await.unwrap();
    assert!(buf.is_empty(), "cancel sends no command");
    assert_eq!(v.rename, None);
}
