//! AC7 (x-007c): the spawn command-to-phase table's integrity.
//!
//! Every phase key is a SESSION_PHASES value and no command appears under two
//! phases. The Python package-data copy's byte identity is the cli-ci
//! "Generated copies match their sources" step, not a test here.

use std::collections::BTreeSet;

const CANONICAL: &str = include_str!("../src/spawn_phase.toml");

#[test]
fn table_keys_are_session_phases_and_commands_are_unique() {
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
    for (phase, commands) in phases {
        assert!(
            allowed.contains(phase.as_str()),
            "unknown phase key {phase}"
        );
        let commands: Vec<String> = commands.clone().try_into().expect("command list");
        for command in commands {
            assert!(
                seen.insert(command.clone()),
                "command {command} appears under two phases"
            );
        }
    }
    assert!(!seen.is_empty(), "the table maps no commands");
}
