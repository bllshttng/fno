//! AC7 (x-007c): the spawn verb-to-phase table's integrity.
//!
//! Every phase key is a SESSION_PHASES value and no verb appears under two
//! phases. The Python package-data copy's byte identity is the cli-ci
//! "Generated copies match their sources" step, not a test here.

use std::collections::BTreeSet;

const CANONICAL: &str = include_str!("../src/spawn_phase.toml");

#[test]
fn table_keys_are_session_phases_and_verbs_are_unique() {
    let table: toml::Table = toml::from_str(CANONICAL).expect("parse spawn_phase.toml");
    let phases = table
        .get("phases")
        .expect("[phases] section")
        .as_table()
        .expect("phase rows");
    let allowed: BTreeSet<&str> = ["think", "blueprint", "execute", "review", "ship"]
        .into_iter()
        .collect();
    let mut seen = BTreeSet::new();
    for (phase, verbs) in phases {
        assert!(
            allowed.contains(phase.as_str()),
            "unknown phase key {phase}"
        );
        let verbs: Vec<String> = verbs.clone().try_into().expect("verb list");
        for verb in verbs {
            assert!(
                seen.insert(verb.clone()),
                "verb {verb} appears under two phases"
            );
        }
    }
    assert!(!seen.is_empty(), "the table maps no verbs");
}
