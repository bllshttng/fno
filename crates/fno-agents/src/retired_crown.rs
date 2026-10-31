//! The retired-crown marker: one file per session that gave up a lead role on
//! purpose. A stop, `org done` and the old holder's side of a succession each
//! write it. The lead wake, the team-revival sweep, scheduler wakes and
//! workspace restore read it, because a stopped lead otherwise looks the same
//! as a lead that went idle or died. Only `fno agents resume` and a new role
//! grant clear it. `crates/fno/src/restore_gate.rs` reads the same files.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::paths::AgentsHome;
use crate::state::RegistryEntry;

/// The store's directory name under the agents home.
pub(crate) const DIR: &str = "retired-crowns";

fn marker_path(home: &AgentsHome, session: &str) -> Option<PathBuf> {
    // The id becomes a file name, so a separator can never escape the store.
    let safe =
        !session.is_empty() && session != "." && session != ".." && !session.contains(['/', '\\']);
    safe.then(|| home.root().join(DIR).join(format!("{session}.json")))
}

/// Record the retirement. Best effort: a failed write costs the guard, never
/// the stop or the vacate that called it, so the failure only warns.
pub(crate) fn write(home: &AgentsHome, session: &str, scope: &str, by: &str) {
    let Some(path) = marker_path(home, session) else {
        return;
    };
    let body = json!({
        "harness_session_id": session,
        "scope": scope,
        "by": by,
        "at": crate::daemon::now_rfc3339_like(),
    });
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
            std::fs::write(&tmp, body.to_string())?;
            std::fs::rename(&tmp, &path)
        });
    if let Err(error) = written {
        eprintln!("retired crown: marker for {session} not written: {error}");
    }
}

/// Lift the retirement: the session holds a role or runs again on request.
pub(crate) fn clear(home: &AgentsHome, session: &str) {
    if let Some(path) = marker_path(home, session) {
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("retired crown: marker for {session} not cleared: {error}");
            }
        }
    }
}

/// Every marker, by session id, with who retired the crown. A missing store is
/// empty. An unreadable marker is skipped, so a lead keeps its wakes.
pub(crate) fn read_all(home: &AgentsHome) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(dir) = std::fs::read_dir(home.root().join(DIR)) else {
        return out;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(body) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        else {
            continue;
        };
        if let Some(session) = body["harness_session_id"].as_str() {
            let by = body["by"].as_str().unwrap_or("unknown");
            out.insert(session.to_string(), by.to_string());
        }
    }
    out
}

fn marked(row: &RegistryEntry, retired: &HashMap<String, String>) -> bool {
    row.harness_session_id
        .as_deref()
        .is_some_and(|sid| retired.contains_key(sid))
}

/// The rows a wake or revival may consider: every row but a marked crown,
/// plus one named line per crown it left out.
pub(crate) fn split_rows(
    rows: Vec<RegistryEntry>,
    retired: &HashMap<String, String>,
) -> (Vec<RegistryEntry>, Vec<String>) {
    let mut kept = Vec::with_capacity(rows.len());
    let mut named = Vec::new();
    for row in rows {
        let scope = row.role_scope.as_deref().unwrap_or("").trim().to_string();
        if !scope.is_empty() && marked(&row, retired) {
            let by = row
                .harness_session_id
                .as_deref()
                .and_then(|sid| retired.get(sid))
                .map_or("unknown", String::as_str);
            named.push(format!("{scope}: retired crown {} (by {by})", row.name));
        } else {
            kept.push(row);
        }
    }
    (kept, named)
}

/// The team-revival sweep's input: a stop the user ran stands, even while the
/// roster still lists a pid for the session.
pub(crate) fn unretired(rows: &[RegistryEntry], home: &AgentsHome) -> Vec<RegistryEntry> {
    split_rows(rows.to_vec(), &read_all(home)).0
}

/// True when a scheduler wake must not reach this session: it carries a marker
/// and its row is terminal or gone. A vacated session that still runs keeps its
/// ordinary wakes, because the marker retires the crown and not the session.
pub(crate) fn refuses_wake(session: &str) -> bool {
    refuses_wake_in(&AgentsHome::from_env(), session)
}

fn refuses_wake_in(home: &AgentsHome, session: &str) -> bool {
    let Some(path) = marker_path(home, session) else {
        return false;
    };
    if !path.is_file() {
        return false;
    }
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return false;
    };
    !registry.entries.iter().any(|row| {
        row.harness_session_id.as_deref() == Some(session)
            && crate::spawn_gate::status_is_liveish(&row.status)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentStatus;

    fn crowned(name: &str, session: &str, scope: Option<&str>) -> RegistryEntry {
        RegistryEntry {
            name: name.to_string(),
            harness_session_id: Some(session.to_string()),
            role_scope: scope.map(str::to_string),
            status: AgentStatus::Live,
            ..Default::default()
        }
    }

    #[test]
    fn a_marked_crown_is_left_out_by_name_and_a_successor_keeps_the_scope() {
        let retired = HashMap::from([("s-old".to_string(), "stop".to_string())]);
        let (kept, named) = split_rows(
            vec![
                crowned("old", "s-old", Some("fno")),
                crowned("new", "s-new", Some("fno")),
            ],
            &retired,
        );
        assert_eq!(named, vec!["fno: retired crown old (by stop)".to_string()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "new");
    }

    #[test]
    fn a_wake_is_refused_only_for_a_marked_session_with_no_live_row() {
        let td = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(td.path());
        crate::registry_store::seed_raw(
            home.registry_json(),
            json!({
                "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
                "agents": [
                    {"name": "gone", "cwd": "/tmp", "created_at": "2026-10-01T00:00:00Z",
                     "status": "exited", "harness": "claude", "harness_session_id": "s-gone"},
                    {"name": "live", "cwd": "/tmp", "created_at": "2026-10-01T00:00:00Z",
                     "status": "live", "harness": "claude", "harness_session_id": "s-live"},
                ],
            })
            .to_string(),
        );
        assert!(!refuses_wake_in(&home, "s-gone"), "no marker, no refusal");
        write(&home, "s-gone", "fno", "stop");
        write(&home, "s-live", "fno", "org-done");
        write(&home, "s-no-row", "fno", "stop");
        assert!(refuses_wake_in(&home, "s-gone"));
        assert!(refuses_wake_in(&home, "s-no-row"));
        assert!(
            !refuses_wake_in(&home, "s-live"),
            "a running session keeps its wakes"
        );
    }

    #[test]
    fn a_marker_on_a_row_with_no_role_names_nothing() {
        let retired = HashMap::from([("s-old".to_string(), "org-done".to_string())]);
        let (kept, named) = split_rows(vec![crowned("old", "s-old", None)], &retired);
        assert!(named.is_empty());
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn write_read_clear_round_trip_and_a_path_id_is_refused() {
        let td = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(td.path());
        write(&home, "s-1", "fno", "stop");
        write(&home, "../escape", "fno", "stop");
        let all = read_all(&home);
        assert_eq!(all.get("s-1").map(String::as_str), Some("stop"));
        assert_eq!(all.len(), 1);
        assert!(!td.path().join("escape.json").exists());
        clear(&home, "s-1");
        assert!(read_all(&home).is_empty());
    }
}
