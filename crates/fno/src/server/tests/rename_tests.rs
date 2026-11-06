use super::*;

#[test]
fn rename_squad_blank_is_refused_when_a_live_squad_holds_the_derived_identity() {
    let mut core = empty_core();
    core.session
        .add_squad(1, vec!["/x".into()], Some("work".into()), leaf_tab(5, 1));
    core.session
        .add_squad(2, vec!["/x".into()], None, leaf_tab(6, 2));
    core.clients.push(client(1, 5, (24, 80), false));
    core.command(
        1,
        Command::RenameSquad {
            squad: 1,
            name: "".into(),
        },
    );
    assert_eq!(core.session.squads[0].name.as_deref(), Some("work"));
}
