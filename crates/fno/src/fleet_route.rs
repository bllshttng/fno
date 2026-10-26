//! The fleet family's command spellings, claimed at the front door: `fno do
//! loops` (and the deprecated root `fno loops`), `fno agents yard` (root
//! `fno yard`), `fno agents autonomy` (root `fno autonomy`). Each execs its
//! `fno-agents` door through an existing action's argv-op lane with the tail
//! byte-verbatim; nothing routes through the deleted Python folds.
//! The agents-scoped spellings arrive here already rewritten by the agents
//! menu (`agents audit yard` -> `agents yard`, `agents admit autonomy` ->
//! `agents autonomy`, `agents loop control` -> `agents loops`).
//!
//! One dispatch line per door in main.rs; the bodies live in the fno-agents
//! modules.

use std::ffi::OsString;
use std::process::Command;

/// Which fleet command the argv names, and whether it arrived on a deprecated
/// root spelling (its move notice prints before the same exec).
#[derive(Debug, PartialEq, Eq)]
pub struct FleetClaim {
    pub door: FleetDoor,
    pub root_alias: bool,
    pub rest: Vec<OsString>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FleetDoor {
    Loops,
    Yard,
    Autonomy,
}

/// Classify the argv: the canonical group spelling or the deprecated root
/// one. Only these three groups claim; anything else forwards to Python,
/// which refuses an unknown leaf like any other dead leaf.
pub fn classify(args: &[OsString]) -> Option<FleetClaim> {
    let words: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    let (door, root_alias, skip) = match words.as_slice() {
        ["do", "loops", ..] => (FleetDoor::Loops, false, 2),
        ["loops", ..] => (FleetDoor::Loops, true, 1),
        ["agents", "yard", ..] => (FleetDoor::Yard, false, 2),
        ["yard", ..] => (FleetDoor::Yard, true, 1),
        ["agents", "autonomy", ..] => (FleetDoor::Autonomy, false, 2),
        ["autonomy", ..] => (FleetDoor::Autonomy, true, 1),
        _ => return None,
    };
    Some(FleetClaim {
        door,
        root_alias,
        rest: args[skip..].to_vec(),
    })
}

/// Answer the claim. Every door execs the fno-agents binary through an
/// EXISTING action's argv-op lane - the shrink law d-fe66560a allows no new
/// client action token: `loops` is the action, `census yard` is the roster
/// fold, `spawn-overlay autonomy` is the spawn seam's introspection (the
/// roles/accounts/plugins nesting precedent). The deprecated root spelling
/// prints the move notice the Python move table used to print,
/// byte-identical, before the same door.
pub fn run(claim: FleetClaim) -> i32 {
    let (door_word, notice) = match claim.door {
        FleetDoor::Loops => ("loops", "fno loops is now fno do loops"),
        FleetDoor::Yard => ("yard", "fno yard is now fno agents yard"),
        FleetDoor::Autonomy => ("autonomy", "fno autonomy is now fno agents autonomy"),
    };
    if claim.root_alias {
        eprintln!("{notice}");
    }
    let Some(binary) = crate::store_client::worker_binary() else {
        eprintln!(
            "fno {door_word}: refused: the fno-agents binary is unavailable \
             (set FNO_AGENTS_BIN, or install fno's Rust runtime)."
        );
        return 3;
    };
    // `loops` is itself the action token; the other two doors ride an
    // existing action as its argv op (shrink law d-fe66560a).
    let mut argv = Command::new(&binary);
    match claim.door {
        FleetDoor::Loops => {
            argv.arg("loops");
        }
        FleetDoor::Yard => {
            argv.arg("census").arg("yard");
        }
        FleetDoor::Autonomy => {
            argv.arg("spawn-overlay").arg("autonomy");
        }
    }
    match argv.args(&claim.rest).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("fno {door_word}: refused: cannot spawn the door: {e}");
            3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oss(pieces: &[&str]) -> Vec<OsString> {
        pieces.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_front_claims_all_six_spellings_and_forwards_others() {
        let claim = classify(&oss(&["do", "loops", "ls"])).unwrap();
        assert_eq!(claim.door, FleetDoor::Loops);
        assert!(!claim.root_alias);
        assert_eq!(claim.rest, oss(&["ls"]));
        let claim = classify(&oss(&["loops", "status"])).unwrap();
        assert!(claim.root_alias);
        assert_eq!(claim.door, FleetDoor::Loops);
        let claim = classify(&oss(&["agents", "yard", "--json"])).unwrap();
        assert_eq!(claim.door, FleetDoor::Yard);
        assert!(!claim.root_alias);
        let claim = classify(&oss(&["yard"])).unwrap();
        assert!(claim.root_alias);
        let claim = classify(&oss(&["agents", "autonomy", "status"])).unwrap();
        assert_eq!(claim.door, FleetDoor::Autonomy);
        let claim = classify(&oss(&["autonomy", "status"])).unwrap();
        assert!(claim.root_alias);
        // Not ours: other commands forward.
        assert!(classify(&oss(&["agents", "top"])).is_none());
        assert!(classify(&oss(&["do", "loops-fake"])).is_none());
        assert!(classify(&oss(&["agents", "yards"])).is_none());
    }
}
