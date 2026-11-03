//! What only the mux server knows about a session on this machine: the
//! workspace it files the session under, and the cost, token and compaction
//! counts it serves on the session's card.
//!
//! The server writes both to `fleet/workspaces.json` under the agents home, as
//! `{"at": <epoch s>, "by_session": {"<harness session id>": "<name>"},
//! "meters": {"<harness session id>": {"cost_cents": n, "tokens": n,
//! "compactions": n}}}` (`crates/fno/src/workspace_sidecar.rs`), and the beat
//! forwards them. A file the server stopped refreshing ages out, so a mux that
//! quit leaves nothing behind.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::paths::AgentsHome;

/// A file older than this reads as nothing filed: ten of the server's
/// 30-second refreshes missed.
pub const MAX_AGE_S: u64 = 300;

/// The counts behind one session's card. A count the mux has no reading for is
/// absent, never zero.
#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize)]
pub struct Meter {
    pub cost_cents: Option<u64>,
    pub tokens: Option<u64>,
    pub compactions: Option<u64>,
}

/// One read of the file, keyed by harness session id.
#[derive(Debug, Default, PartialEq)]
pub struct Filed {
    pub workspaces: HashMap<String, String>,
    pub meters: HashMap<String, Meter>,
}

#[derive(Deserialize)]
struct Sidecar {
    at: u64,
    by_session: HashMap<String, String>,
    #[serde(default)]
    meters: HashMap<String, Meter>,
}

pub fn path(home: &AgentsHome) -> PathBuf {
    home.root().join("fleet").join("workspaces.json")
}

/// What the file holds, empty when it is absent, unreadable or older than
/// [`MAX_AGE_S`].
pub fn read(home: &AgentsHome, now_secs: u64) -> Filed {
    let Ok(raw) = std::fs::read_to_string(path(home)) else {
        return Filed::default();
    };
    match serde_json::from_str::<Sidecar>(&raw) {
        Ok(side) if now_secs.saturating_sub(side.at) <= MAX_AGE_S => Filed {
            workspaces: side.by_session,
            meters: side.meters,
        },
        _ => Filed::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_file_answers_a_stale_or_broken_one_answers_nothing_and_the_writer_agrees() {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path());
        assert_eq!(
            fno::workspace_sidecar::path(dir.path()),
            path(&home),
            "reader and writer disagree on the file"
        );
        assert_eq!(read(&home, 1000), Filed::default(), "no file");
        let write = |body: &str| {
            std::fs::create_dir_all(path(&home).parent().unwrap()).unwrap();
            std::fs::write(path(&home), body).unwrap();
        };
        write(
            r#"{"at": 1000, "by_session": {"s1": "fno"},
                "meters": {"s1": {"cost_cents": 42, "tokens": 9000, "compactions": null}}}"#,
        );
        let fresh = read(&home, 1000 + MAX_AGE_S);
        assert_eq!(fresh.workspaces["s1"], "fno", "inclusive bound");
        assert_eq!(
            fresh.meters["s1"],
            Meter {
                cost_cents: Some(42),
                tokens: Some(9000),
                compactions: None,
            }
        );
        assert_eq!(read(&home, 1001 + MAX_AGE_S), Filed::default(), "aged out");
        write(r#"{"at": 1000, "by_session": {"s1": "fno"}}"#);
        assert!(
            read(&home, 1000).meters.is_empty(),
            "an older server writes no meters"
        );
        write("not json");
        assert_eq!(read(&home, 1000), Filed::default(), "unparseable");
    }
}
