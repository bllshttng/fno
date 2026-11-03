//! The workspace each session sits under, handed to the fleet beat.
//!
//! Only the mux server resolves a session to a workspace, so it writes the
//! answer to `fleet/workspaces.json` under the agents home and the daemon's
//! beat forwards it to the other machines (`fno_agents::fleet_workspace` reads
//! the same file). A session the server files under no named workspace has no
//! entry, and its far row groups by project as before.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::proto::AgentRow;

/// The server rewrites at most this often, and the reader's freshness bound
/// spans ten of them. The beat itself runs every 60 seconds.
const EVERY: Duration = Duration::from_secs(30);

static LAST_WRITE: Mutex<Option<Instant>> = Mutex::new(None);

/// The file under an agents home `root`.
pub fn path(root: &Path) -> PathBuf {
    root.join("fleet").join("workspaces.json")
}

/// Session id to workspace name, from the rows the server serves. `name_of`
/// answers a squad id with its name; an unnamed squad files no one.
pub(crate) fn by_session(
    rows: &[AgentRow],
    name_of: impl Fn(u64) -> Option<String>,
) -> BTreeMap<String, String> {
    rows.iter()
        .filter_map(|row| {
            let session = row
                .harness_session_id
                .as_deref()
                .filter(|s| !s.is_empty())?;
            let name = name_of(row.squad?).filter(|n| !n.is_empty())?;
            Some((session.to_string(), name))
        })
        .collect()
}

fn write(root: &Path, map: &BTreeMap<String, String>, now_secs: u64) -> std::io::Result<()> {
    let file = path(root);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::json!({"at": now_secs, "by_session": map});
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&body)?)?;
    std::fs::rename(&tmp, &file)
}

/// Write the sidecar from the rows the server serves, at most once per
/// [`EVERY`]. A failed write is said once: the beat then publishes no
/// workspace, which reads as the project fallback, never as a wrong name.
pub(crate) fn publish(root: &Path, rows: &[AgentRow], name_of: impl Fn(u64) -> Option<String>) {
    let mut last = LAST_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|at| at.elapsed() < EVERY) {
        return;
    }
    *last = Some(Instant::now());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Err(error) = write(root, &by_session(rows, name_of), now) {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| eprintln!("workspace sidecar: {}: {error}", path(root).display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_squads_file_their_sessions_and_each_write_replaces_the_last() {
        let row = |session: Option<&str>, squad| AgentRow {
            harness_session_id: session.map(str::to_string),
            squad,
            ..AgentRow::default()
        };
        let rows = [
            row(Some("s1"), Some(1)),
            row(Some("s2"), Some(2)),
            row(Some("s3"), None),
            row(None, Some(1)),
        ];
        let got = by_session(&rows, |id| (id == 1).then(|| "fno".to_string()));
        assert_eq!(got, BTreeMap::from([("s1".to_string(), "fno".to_string())]));

        let dir = tempfile::tempdir().unwrap();
        let read = || -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(path(dir.path())).unwrap()).unwrap()
        };
        write(dir.path(), &got, 1000).unwrap();
        assert_eq!(
            read(),
            serde_json::json!({"at": 1000, "by_session": {"s1": "fno"}})
        );
        write(dir.path(), &BTreeMap::new(), 1060).unwrap();
        assert_eq!(read(), serde_json::json!({"at": 1060, "by_session": {}}));
    }
}
