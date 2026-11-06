//! The default-dispatch lifecycle gesture (x-e763): with the confirm pref
//! at its default (off), Stop/Remove dispatch the same command the confirm
//! would commit, in one gesture. Lives outside client_tests.rs under the
//! file-budget gate.
use super::*;

#[tokio::test]
async fn menu_stop_on_a_live_row_dispatches_with_the_default_pref() {
    // AC10: no are-you-sure by default. The command the confirm would have
    // committed goes straight onto the wire; no overlay arms.
    let mut v = view_with_agents(vec![agent_row("w", 10, Some(AgentBadge::Working), false)]);
    assert!(!v.confirm_lifecycle, "the default is no confirm");
    let row = agent_row_at(&v, |a| a.name == "w");
    assert!(v.open_row_menu(row, Anchor::Center));
    menu_select(&mut v, super::MenuAction::Stop).await;
    let mut buf: Vec<u8> = Vec::new();
    row_menu_execute_selected(&mut v, &mut buf).await.unwrap();
    assert!(v.confirm.is_none(), "no confirm armed");
    assert_eq!(
        decode_cmds(buf),
        vec![Command::StopAgent {
            name: "w".into(),
            harness_session_id: None,
            pane_id: Some(10)
        }],
        "the stop dispatched in one gesture, carrying the row's pane"
    );
}
#[tokio::test]
async fn selector_x_on_an_unmeasured_row_arms_measure_and_remove() {
    // (x-b5d1) The `?` safety mark survives on an exited row whose exit was
    // never corroborated (unmeasured: true): removing it still measures, so
    // the server skips the stop leg that times out against an orphaned row.
    // A LIVE row with no reading is a shell under the 2026-10-06 ruling and
    // removes without a measure.
    let mut ghost = lifecycle_row("ghost", true, false);
    ghost.unmeasured = true;
    let mut v = view_with_agents(vec![ghost]);
    v.set_squad_view(1, SectionView::Expanded);
    v.selector = Some(agent_row_at(&v, |a| a.name == "ghost"));
    let mut buf: Vec<u8> = Vec::new();
    selector_keys(&mut v, b"x", &mut buf).await.unwrap();
    assert!(buf.is_empty(), "arming sends nothing");
    let c = v.confirm.as_ref().expect("the measure confirm armed");
    assert!(
        matches!(&c.action, ConfirmKind::RemoveAgent { measure: true, .. }),
        "measure arms on the unmeasured row"
    );
    assert!(v.confirm_text(c).starts_with("measure and remove ghost?"));
}
