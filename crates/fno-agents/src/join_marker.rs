//! A session joins fno at its first fno skill or verb.
//!
//! Joining is opt-in: a session the user opened but never pointed at fno owns
//! no registry row, holds no spawn-gate slot and gets no using-fno preamble.
//! The join is one marker file, `<agents home>/sessions/<harness>/<sid>/joined`,
//! plus the register act `/fno:fno-me` runs. The marker outlives the row, so a
//! joined session whose row went missing mints back at its next session
//! report. `crates/fno/src/join_marker.rs` reads the same path at the front
//! door; the layout is a wire format between the two crates.
//!
//! Every hook command runs with `FNO_HOOK_CONTEXT=1`. 25 hook scripts run fno
//! verbs inside every session, so a join fired from a hook would join every
//! session at its first SessionStart. The join act refuses there. The prompt
//! and Skill hooks that ARE deliberate triggers clear the variable for their
//! one call.

use std::path::{Path, PathBuf};

use crate::paths::AgentsHome;

pub const HOOK_CONTEXT_ENV: &str = "FNO_HOOK_CONTEXT";

/// A session id is a path segment here: reject anything that could walk out
/// of the sessions tree.
fn segment_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s != "." && s != ".." && !s.contains(['/', '\\', '\0'])
}

pub fn marker_path(root: &Path, harness: &str, session_id: &str) -> Option<PathBuf> {
    (segment_ok(harness) && segment_ok(session_id)).then(|| {
        root.join("sessions")
            .join(harness)
            .join(session_id)
            .join("joined")
    })
}

pub fn is_joined(root: &Path, harness: &str, session_id: &str) -> bool {
    marker_path(root, harness, session_id).is_some_and(|p| p.exists())
}

pub(crate) fn write_marker(root: &Path, harness: &str, session_id: &str) -> Result<(), String> {
    let path = marker_path(root, harness, session_id)
        .ok_or_else(|| format!("not a session id: {session_id:?}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("marker dir: {e}"))?;
    }
    std::fs::write(&path, b"").map_err(|e| format!("marker write: {e}"))
}

fn env_set(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

#[derive(Debug, PartialEq, Eq)]
pub enum JoinOutcome {
    Joined,
    AlreadyJoined,
    /// A spawned worker has its row from the spawn: the marker is the whole act.
    SpawnedWorker,
}

/// The join act: the register `/fno:fno-me` runs, then the marker. Refuses in
/// hook context. A second join changes nothing.
pub fn join(root: &Path, harness: &str, session_id: &str) -> Result<JoinOutcome, String> {
    if env_set(HOOK_CONTEXT_ENV) {
        return Err(format!(
            "{HOOK_CONTEXT_ENV} is set: a hook's fno call never joins its session"
        ));
    }
    marker_path(root, harness, session_id)
        .ok_or_else(|| format!("not a session id: {session_id:?}"))?;
    if is_joined(root, harness, session_id) {
        return Ok(JoinOutcome::AlreadyJoined);
    }
    if env_set("FNO_AGENT_SELF") {
        write_marker(root, harness, session_id)?;
        return Ok(JoinOutcome::SpawnedWorker);
    }
    crate::mail_cli::hold::register_existing_session(harness, session_id, false)?;
    write_marker(root, harness, session_id)?;
    Ok(JoinOutcome::Joined)
}

/// Whether the using-fno preamble is due: only for a joined session (a
/// spawned worker counts as joined), then once and again after each
/// compaction boundary. `record` stamps it taught.
pub fn preamble_due(
    root: &Path,
    harness: &str,
    session_id: &str,
    transcript: Option<&Path>,
    record: bool,
) -> bool {
    if !is_joined(root, harness, session_id) && !env_set("FNO_AGENT_SELF") {
        return false;
    }
    crate::mail_teach::lesson_due_at(
        root,
        crate::mail_teach::Lesson::UsingFno,
        session_id,
        transcript,
        None,
        record,
    )
}

// ---------------------------------------------------------------------------
// `fno-agents report --kind join --harness <h> --session-id <id>
//      [--check] [--preamble-due [--mark] [--compacted] [--transcript <path>]]`
//
// Without --check it joins (refused in hook context, exit 3). --preamble-due
// prints `due` when the preamble is due, nothing otherwise; with --compacted
// it is due for any joined session. Rides the report
// action so no verb is added.
// ---------------------------------------------------------------------------

pub fn run_join(args: &[String], home: &AgentsHome) -> i32 {
    let mut harness: Option<String> = None;
    let mut session: Option<String> = None;
    let mut transcript: Option<PathBuf> = None;
    let (mut check, mut due, mut mark, mut compacted) = (false, false, false, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--harness" => &mut harness,
            "--session-id" => &mut session,
            "--transcript" => {
                match it.next() {
                    Some(v) => transcript = Some(PathBuf::from(v)),
                    None => {
                        eprintln!("report --kind join: --transcript needs a value");
                        return 2;
                    }
                }
                continue;
            }
            "--check" => {
                check = true;
                continue;
            }
            "--preamble-due" => {
                due = true;
                continue;
            }
            "--mark" => {
                mark = true;
                continue;
            }
            "--compacted" => {
                compacted = true;
                continue;
            }
            other => {
                eprintln!("report --kind join: unexpected argument {other:?}");
                return 2;
            }
        };
        match it.next() {
            Some(v) => *slot = Some(v.clone()),
            None => {
                eprintln!("report --kind join: {arg} needs a value");
                return 2;
            }
        }
    }
    let (Some(harness), Some(session)) = (harness, session) else {
        eprintln!("report --kind join: --harness and --session-id are required");
        return 2;
    };
    let root = home.root();
    if !check {
        match join(root, &harness, &session) {
            Ok(JoinOutcome::Joined) => eprintln!(
                "fno: this {harness} session joined fno; skills/using-fno/SKILL.md names both surfaces"
            ),
            Ok(_) => {}
            Err(msg) => {
                eprintln!("report --kind join: {msg}");
                if !due {
                    return 3;
                }
            }
        }
    }
    // A carrier that fires exactly at a compaction says so: opencode, pi and
    // agy transcripts carry no boundary shape the stamp could read.
    let due_now = if compacted {
        let joined = is_joined(root, &harness, &session) || env_set("FNO_AGENT_SELF");
        if joined && mark {
            preamble_due(root, &harness, &session, transcript.as_deref(), true);
        }
        joined
    } else {
        preamble_due(root, &harness, &session, transcript.as_deref(), mark)
    };
    if due && due_now {
        println!("due");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_for(session: &str) -> usize {
        crate::state::try_load_registry(&AgentsHome::shared_registry_json())
            .ok()
            .flatten()
            .map(|r| {
                r.entries
                    .iter()
                    .filter(|e| e.harness_session_id.as_deref() == Some(session))
                    .count()
            })
            .unwrap_or(0)
    }

    struct EnvClear(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl EnvClear {
        fn new(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|k| (*k, std::env::var_os(k))).collect();
            for k in keys {
                std::env::remove_var(k);
            }
            Self(saved)
        }
    }
    impl Drop for EnvClear {
        fn drop(&mut self) {
            for (k, v) in &self.0 {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn session_join_writes_one_row_and_marker_and_refuses_hook_context() {
        let _root = crate::paths::DeclaredRoot::declare("session-join");
        let _env = EnvClear::new(&[HOOK_CONTEXT_ENV, "FNO_AGENT_SELF"]);
        let home = AgentsHome::from_env();
        std::fs::create_dir_all(home.root()).unwrap();
        let sid = "0f1e2d3c-aaaa-bbbb-cccc-000000000001";

        // AC2: a hook's call writes nothing.
        std::env::set_var(HOOK_CONTEXT_ENV, "1");
        assert!(join(home.root(), "claude", sid).is_err());
        assert!(!is_joined(home.root(), "claude", sid));
        assert_eq!(rows_for(sid), 0);
        std::env::remove_var(HOOK_CONTEXT_ENV);

        // AC1: one row and the marker; a second join changes nothing.
        assert_eq!(join(home.root(), "claude", sid), Ok(JoinOutcome::Joined));
        assert!(is_joined(home.root(), "claude", sid));
        assert_eq!(rows_for(sid), 1);
        // A join is not a hold: the row carries no bus-only stamp, and it
        // has a birth date the orphan sweep can age.
        let row = crate::state::load_registry(&AgentsHome::shared_registry_json())
            .unwrap()
            .entries
            .into_iter()
            .find(|e| e.harness_session_id.as_deref() == Some(sid))
            .unwrap();
        assert_eq!(row.delivery_policy, None);
        assert!(!row.created_at.is_empty());
        assert_eq!(
            join(home.root(), "claude", sid),
            Ok(JoinOutcome::AlreadyJoined)
        );
        assert_eq!(rows_for(sid), 1);

        assert!(marker_path(home.root(), "claude", "../x").is_none());
    }

    #[test]
    fn session_join_gates_the_preamble() {
        let _root = crate::paths::DeclaredRoot::declare("session-join-due");
        let _env = EnvClear::new(&[HOOK_CONTEXT_ENV, "FNO_AGENT_SELF"]);
        let home = AgentsHome::from_env();
        let root = home.root();
        let sid = "s-due";
        let dir = std::env::temp_dir().join(format!("sj-due-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let boundary = |epoch: i64| {
            let ts = chrono::DateTime::from_timestamp(epoch, 0)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let p = dir.join(format!("t-{epoch}.jsonl"));
            std::fs::write(
                &p,
                format!("{{\"type\":\"summary\",\"subtype\":\"compact_boundary\",\"timestamp\":\"{ts}\"}}\n"),
            )
            .unwrap();
            p
        };
        let t1 = boundary(1_000);

        // AC4: an unjoined session never reads due.
        assert!(!preamble_due(root, "codex", sid, Some(&t1), true));
        assert!(!preamble_due(root, "codex", sid, Some(&t1), true));

        // AC3: joined, taught once, due once more after a newer boundary.
        write_marker(root, "codex", sid).unwrap();
        assert!(preamble_due(root, "codex", sid, Some(&t1), true));
        assert!(!preamble_due(root, "codex", sid, Some(&t1), true));
        let t2 = boundary(2_000);
        assert!(preamble_due(root, "codex", sid, Some(&t2), true));
        assert!(!preamble_due(root, "codex", sid, Some(&t2), true));

        // The mail lesson keeps its own stamp.
        assert!(crate::mail_teach::teach_if_due_at(
            root,
            sid,
            Some(&t2),
            None,
            false
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
