//! The workspace each session sits under in this machine's mux.
//!
//! Only the mux server resolves a session to a workspace, so it writes the
//! answer to `fleet/workspaces.json` under the agents home, as
//! `{"at": <epoch s>, "by_session": {"<harness session id>": "<name>"}}`
//! (`crates/fno/src/workspace_sidecar.rs`), and the beat forwards it. A file
//! the server stopped refreshing ages out, so a mux that quit leaves no
//! workspace behind.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::paths::AgentsHome;

/// A file older than this reads as no workspaces: ten of the server's
/// 30-second refreshes missed.
pub const MAX_AGE_S: u64 = 300;

#[derive(Deserialize)]
struct Sidecar {
    at: u64,
    by_session: HashMap<String, String>,
}

pub fn path(home: &AgentsHome) -> PathBuf {
    home.root().join("fleet").join("workspaces.json")
}

/// Session id to workspace name, empty when the file is absent, unreadable
/// or older than [`MAX_AGE_S`].
pub fn read(home: &AgentsHome, now_secs: u64) -> HashMap<String, String> {
    let Ok(raw) = std::fs::read_to_string(path(home)) else {
        return HashMap::new();
    };
    match serde_json::from_str::<Sidecar>(&raw) {
        Ok(side) if now_secs.saturating_sub(side.at) <= MAX_AGE_S => side.by_session,
        _ => HashMap::new(),
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
        assert!(read(&home, 1000).is_empty(), "no file");
        let write = |body: &str| {
            std::fs::create_dir_all(path(&home).parent().unwrap()).unwrap();
            std::fs::write(path(&home), body).unwrap();
        };
        write(r#"{"at": 1000, "by_session": {"s1": "fno"}}"#);
        assert_eq!(
            read(&home, 1000 + MAX_AGE_S)["s1"],
            "fno",
            "inclusive bound"
        );
        assert!(read(&home, 1001 + MAX_AGE_S).is_empty(), "aged out");
        write("not json");
        assert!(read(&home, 1000).is_empty(), "unparseable");
    }
}
