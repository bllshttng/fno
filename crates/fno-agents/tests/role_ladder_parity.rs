//! parity-stage: characterization
//! parity-oracle: fno.agents.role.derive_role_level
//!
//! Characterization for the org ladder: `territory::resolve_role`, served as
//! the `role-resolve` spawn-overlay kind, is the one resolver the lead board
//! and the promote path share. The Python rung derivation (`derive_role_level`,
//! `_epic_or_refuse`, `_member_rung`) was deleted in the same change, and
//! Python `resolve_role` is now a transport to this kind. The happy-path
//! goldens hold the pairs the Python tests recorded. The refusal goldens hold
//! the team wording, so the promote refusals read as the board's do.

use common::{assert_golden, Golden};
use serde_json::{json, Value};

mod common;

/// The Python fixture's territory: epics e-1 and e-2, one feature node, and
/// projects alpha (short name a) and beta.
fn answer(scopes: &[&str]) -> Golden {
    let payload = json!({
        "kind": "role-resolve",
        "scopes": scopes,
        "projects": {"alpha": "alpha", "a": "alpha", "beta": "beta"},
        "entries": [
            {"id": "e-1", "type": "epic", "project": "alpha"},
            {"id": "e-2", "type": "epic", "project": "beta"},
            {"id": "n-1", "type": "feature", "project": "alpha"},
        ],
    });
    let answer: Value = fno_agents::spawn_overlay::resolve(payload).expect("role-resolve answers");
    Golden {
        exit: None,
        streams: vec![format!("{answer}\n")],
    }
}

#[test]
fn the_ladder_derives_rung_and_canonical_scope_together() {
    for (label, scopes) in [
        (
            "epic_set_is_canonical_order_and_dedup",
            &["e-2", "e-1", "e-2"][..],
        ),
        (
            "one_project_named_twice_is_not_a_portfolio",
            &["alpha", "a"][..],
        ),
        (
            "a_portfolio_derives_rung_0_through_aliases",
            &["a", "beta"][..],
        ),
    ] {
        assert_golden("role_ladder", label, &answer(scopes), None);
    }
}

#[test]
fn a_scope_naming_no_territory_is_refused_in_team_wording() {
    for (label, scopes) in [
        ("an_empty_scope_is_refused", &[][..]),
        ("an_unresolvable_scope_is_refused", &["banana"][..]),
        ("a_non_epic_node_is_refused_with_its_remedy", &["n-1"][..]),
        (
            "a_mixed_scope_names_each_members_rung",
            &["alpha", "e-1"][..],
        ),
    ] {
        assert_golden("role_ladder", label, &answer(scopes), None);
    }
}
