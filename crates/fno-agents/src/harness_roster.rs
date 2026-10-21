//! `fno-agents harness-roster`: the daemon-free JSON read of the one harness
//! roster. Python's `fno.harness_names` proxies this instead of
//! carrying a tuple copy of `provider::KNOWN_HARNESSES`; the parity gate
//! parses the same const out of the Rust source, so a name added in Rust
//! alone reaches every Python reader.
//!
//! Transport-only and unregistered in `ALL_CLIENT_ACTIONS` (the shrink law,
//! d-fe66560a, allows no new client action): every caller reaches it through
//! `resolve_binary`, like `sync-canonical`.

use crate::client_verbs::to_python_json;
use crate::harness_capabilities::HarnessContract;
use crate::provider::{KNOWN_HARNESSES, KNOWN_PROVIDERS};
use serde_json::json;

/// Takes no arguments and starts nothing; the roster is a compile-time const.
pub fn run_harness_roster(rest: &[String]) -> i32 {
    if let Some(arg) = rest.first() {
        eprintln!("fno-agents: harness-roster takes no arguments (got: {arg})");
        return 2;
    }
    println!("{}", to_python_json(&roster_json()));
    0
}

/// The thread-seam set, derived from the bundled contract: a row whose
/// `features.spawn.state` reads `native` seats on the seam with no other
/// edit, the same answer Python's harness_map.thread_seatable derives from
/// the same TOML. Roster order, so the rendered accept list is stable across
/// contract edits.
fn spawn_set(contract: &HarnessContract) -> Vec<&'static str> {
    KNOWN_HARNESSES
        .iter()
        .copied()
        .filter(|name| {
            contract
                .harness
                .get(*name)
                .and_then(|caps| caps.features.get("spawn"))
                .is_some_and(|claim| claim.state == "native")
        })
        .collect()
}

fn roster_json() -> serde_json::Value {
    let contract = HarnessContract::packaged().expect("bundled contract parses");
    let spawn = spawn_set(&contract);
    json!({ "known": KNOWN_HARNESSES, "providers": KNOWN_PROVIDERS, "spawn": spawn })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Today's Python `SPAWN_HARNESSES` tuple, pinned in roster order: the
    /// derived set must equal it name for name, so the port serves the same
    /// seam gate the hand list did.
    #[test]
    fn spawn_equals_the_python_seam_roster() {
        let payload = roster_json();
        let derived = payload["spawn"].as_array().expect("spawn is a list");
        let names: Vec<&str> = derived
            .iter()
            .map(|v| v.as_str().expect("a name"))
            .collect();
        assert_eq!(
            names,
            vec![
                "claude",
                "codex",
                "agy",
                "opencode",
                "pi",
                "cursor-agent",
                "grok",
                "zcode"
            ],
            "the contract's spawn-native set drifted from the seam roster the \
             Python list held; re-pin both sides together"
        );
    }

    /// The seam gate is DERIVED: flipping one row's spawn claim moves that
    /// harness in or out of the set with no other edit (AC1-HP).
    #[test]
    fn spawn_set_follows_the_row_claim() {
        let native = crate::harness_capabilities::CAPABILITY_TOML.replacen(
            "[harness.footnote.features.spawn]\nstate = \"capable\"",
            "[harness.footnote.features.spawn]\nstate = \"native\"",
            1,
        );
        // The replacen above must have hit its row; a missed anchor would
        // silently pin nothing.
        assert!(native != crate::harness_capabilities::CAPABILITY_TOML);
        let contract = HarnessContract::parse(&native).expect("edited contract parses");
        let derived = spawn_set(&contract);
        assert!(
            derived.contains(&"footnote"),
            "a row flipped to spawn-native must land in the derived set with no other edit"
        );
    }

    /// The rendered set must match the CONTRACT, row by row: a native row the
    /// render drops (or an absent row it adds) is exactly the drift this verb
    /// exists to prevent on the Python side. Not a self-comparison: the truth
    /// comes from the contract, never from the render under test.
    #[test]
    fn rendered_spawn_matches_the_contract_rows() {
        let contract = HarnessContract::packaged().expect("bundled contract parses");
        let payload = roster_json();
        let rendered: Vec<&str> = payload["spawn"]
            .as_array()
            .expect("spawn is a list")
            .iter()
            .map(|v| v.as_str().expect("a name"))
            .collect();
        for (name, caps) in &contract.harness {
            let native = caps
                .features
                .get("spawn")
                .is_some_and(|claim| claim.state == "native");
            assert_eq!(
                rendered.contains(name.as_str()),
                native,
                "harness {name} renders/native disagreement"
            );
        }
    }
}
