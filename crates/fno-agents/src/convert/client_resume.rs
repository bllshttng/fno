//! The client-resume strategy: the pane stops, the client relaunches the
//! session detached.
//!
//! Two measurements disagree about whether claude keeps its id here.
//! `reentry.rs` records that on 2.1.272 a STOPPED session continues under
//! the same id, while the Python spawn door says a revival forks a new one.
//! They describe different doors, and a conversion cannot afford to guess
//! between them. So this strategy never promises the id: it reads the
//! resumed id back off the roster and compares.
//!
//! A differing id without `--allow-new-id` is a rollback, not a shrug. A
//! differing id on a TEAMED row is refused even with the flag, because
//! moving a team to a new session id is succession - a separate operation
//! with its own attribution.

/// What to do with the id the relaunched session actually reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdVerdict {
    /// Same id. The conversion completes silently.
    Kept,
    /// A new id the caller authorized on an unpromoted row. The old id is
    /// recorded as the related id and the receipt says so out loud.
    AcceptedNew { old: String, new: String },
    /// Stop the new session and put the row back.
    RollBack(String),
}

/// Compare the id read back from the roster against the one converted.
///
/// `teamed` is read ONCE before any mutation, so a team granted mid-move
/// cannot flip the answer after the session was already relaunched.
pub fn id_verdict(
    original: &str,
    read_back: Option<&str>,
    allow_new_id: bool,
    teamed: bool,
) -> IdVerdict {
    let Some(read_back) = read_back.filter(|id| !id.is_empty()) else {
        // No id at all is not "a new id": nothing proves the relaunched
        // session is this session, so it is never adopted.
        return IdVerdict::RollBack(
            "the relaunched session reported no id, so it cannot be proven to be this session"
                .to_string(),
        );
    };
    if read_back == original {
        return IdVerdict::Kept;
    }
    if teamed {
        return IdVerdict::RollBack(format!(
            "the relaunch minted {read_back} rather than {original}, and this row is teamed; \
             moving a team to a new session id is succession, not conversion"
        ));
    }
    if !allow_new_id {
        return IdVerdict::RollBack(format!(
            "the relaunch minted {read_back} rather than {original}. Converting would change the \
             session id, which no caller authorized; re-run with --allow-new-id to accept it"
        ));
    }
    IdVerdict::AcceptedNew {
        old: original.to_string(),
        new: read_back.to_string(),
    }
}

/// The launch axes carried from the LIVE writer's own argv, never from a
/// default. A conversion that relaunched on the account default model would
/// silently downgrade a session the operator had pinned.
///
/// Only flags with a measured spelling on the claude resume arm are carried;
/// anything else on that argv belongs to the pane lane and is dropped rather
/// than guessed at.
pub fn carried_flags(argv: &[String]) -> Vec<String> {
    const CARRIED: [&str; 4] = ["--model", "--effort", "--permission-mode", "--add-dir"];
    let mut out = Vec::new();
    let mut iter = argv.iter().peekable();
    while let Some(arg) = iter.next() {
        if let Some((flag, value)) = arg.split_once('=') {
            if CARRIED.contains(&flag) && !value.is_empty() {
                out.push(flag.to_string());
                out.push(value.to_string());
            }
            continue;
        }
        if !CARRIED.contains(&arg.as_str()) {
            continue;
        }
        // A flag whose value is missing or is itself a flag carries nothing:
        // appending a bare `--model` to the relaunch argv makes the launch
        // fail rather than keep the pin.
        match iter.peek() {
            Some(value) if !value.starts_with('-') => {
                out.push(arg.clone());
                out.push((*value).clone());
                iter.next();
            }
            _ => {}
        }
    }
    out
}

/// The relaunch argv, after the program name. `claude --bg --resume <id>`
/// continues the session under the same id when nothing else holds it, and
/// STARTS A COPY when the session is still running. That second sentence is
/// the whole reason the pane must be proven gone before this runs, and the
/// reason the id is read back afterwards rather than assumed.
///
/// `--name` carries the row's name, never the title the transcript holds,
/// which a retasked row has outgrown. The carried flags come last so a pin
/// the operator set on the live writer beats any default the resume would
/// otherwise take.
pub fn resume_argv(session_id: &str, name: &str, carried: &[String]) -> Vec<String> {
    let mut argv = vec![
        "--bg".to_string(),
        "--resume".to_string(),
        session_id.to_string(),
        "--name".to_string(),
        name.to_string(),
    ];
    argv.extend_from_slice(carried);
    argv
}

/// Which roster row is the relaunched session.
///
/// A row still carrying the ORIGINAL session id is the answer whatever else
/// appeared, because that is the id the resume addressed. Otherwise the row
/// is identified by difference: one short id present now that was absent
/// before the launch. Two new rows prove nothing about which is ours, so
/// this answers `None` and the caller rolls back rather than adopting a row
/// that may belong to another spawn.
pub fn relaunched_row<'a>(
    before: &[String],
    after: &'a [crate::claude_roster::ClaudeAgentRow],
    original: &str,
) -> Option<&'a crate::claude_roster::ClaudeAgentRow> {
    if let Some(row) = after
        .iter()
        .find(|row| row.session_id.as_deref() == Some(original))
    {
        return Some(row);
    }
    let mut fresh = after
        .iter()
        .filter(|row| !before.iter().any(|seen| seen == &row.short_id));
    let first = fresh.next()?;
    match fresh.next() {
        Some(_) => None,
        None => Some(first),
    }
}

/// Flip a row onto the claude thread shape: a background session the
/// operator reaches with `claude attach <short_id>`.
///
/// Unlike the codex flip this KEEPS a short id and a pid, because a claude
/// thread is a real process claude hosts itself, and both the attach command
/// and the liveness ladder address it through those two fields. The row's
/// name, node and every other birth fact are left alone.
pub fn to_claude_thread(
    entry: &mut crate::state::RegistryEntry,
    short_id: &str,
    writer_pid: Option<u32>,
    session_id: &str,
) {
    entry.substrate = Some("thread".to_string());
    entry.host_mode = Some(crate::state::HOST_MODE_INTERACTIVE.to_string());
    entry.short_id = short_id.to_string();
    entry.mux = None;
    entry.pid = writer_pid;
    // The stamp belongs to whatever pid the row names. Reading it for the
    // NEW writer keeps `pid_is_ours` able to detect a recycled pid; leaving
    // the old child's stamp here would make it read a live row as dead.
    entry.pid_start_time = writer_pid.and_then(crate::daemon::process_start_time);
    entry.harness_session_id = Some(session_id.to_string());
    entry.claude_session_uuid = Some(session_id.to_string());
}

/// Whether this row holds a team. Read ONCE before the conversion mutates
/// anything, because a team granted mid-move must not decide the fate of a
/// session that was already relaunched.
///
/// Team liveness is the ROW's liveness, so the level alone answers it: an
/// exited row carries no live team whatever it records.
pub fn row_is_teamed(entry: &crate::state::RegistryEntry) -> bool {
    entry.role_level.is_some_and(|level| level > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::claude_roster::ClaudeAgentRow;

    fn roster_row(short_id: &str, session_id: Option<&str>) -> ClaudeAgentRow {
        let mut row = ClaudeAgentRow::new(short_id, Some("running"));
        row.session_id = session_id.map(str::to_string);
        row
    }

    #[test]
    fn the_resume_argv_asks_for_the_background_lane_and_carries_the_pins_last() {
        let carried = ["--model".to_string(), "glm-5.3-flash[1m]".to_string()];
        assert_eq!(
            resume_argv("sid-1", "w1", &carried),
            vec![
                "--bg",
                "--resume",
                "sid-1",
                "--name",
                "w1",
                "--model",
                "glm-5.3-flash[1m]"
            ]
        );
        assert_eq!(
            resume_argv("sid-1", "w1", &[]),
            vec!["--bg", "--resume", "sid-1", "--name", "w1"]
        );
    }

    #[test]
    fn a_single_new_short_id_identifies_the_relaunch_by_difference() {
        let before = ["aaaa".to_string()];
        let after = [
            roster_row("aaaa", Some("other")),
            roster_row("cccc", Some("minted")),
        ];
        let found = relaunched_row(&before, &after, "sid-1").expect("one new row is identifiable");
        assert_eq!(found.session_id.as_deref(), Some("minted"));
    }

    #[test]
    fn two_new_rows_identify_nothing_and_no_row_is_adopted() {
        let before = ["aaaa".to_string()];
        let after = [
            roster_row("aaaa", Some("other")),
            roster_row("cccc", Some("minted")),
            roster_row("dddd", Some("someone-elses")),
        ];
        assert!(relaunched_row(&before, &after, "sid-1").is_none());
        // And a roster that grew no row at all answers the same way.
        assert!(relaunched_row(&before, &after[..1], "sid-1").is_none());
    }

    #[test]
    fn the_thread_flip_keeps_the_short_id_and_pid_the_attach_command_needs() {
        let mut entry = crate::state::RegistryEntry {
            name: "worker-one".to_string(),
            cwd: "/repo".to_string(),
            ..Default::default()
        };
        entry.harness = Some("claude".to_string());
        entry.substrate = Some("pane".to_string());
        entry.short_id = "oldshort".to_string();
        entry.mux = Some(crate::state::MuxRef {
            session: "fno".to_string(),
            pane_id: 7,
        });
        entry.pid = Some(4242);
        entry.pid_start_time = Some(99);
        entry.node = Some("x-node".to_string());

        to_claude_thread(&mut entry, "newshort", Some(5150), "sid-1");

        assert_eq!(entry.substrate.as_deref(), Some("thread"));
        // A claude thread IS a process, unlike a codex thread: the attach
        // command takes the short id and the liveness ladder takes the pid.
        assert_eq!(entry.short_id, "newshort");
        assert_eq!(entry.pid, Some(5150));
        assert!(entry.mux.is_none());
        assert_eq!(entry.harness_session_id.as_deref(), Some("sid-1"));
        assert_eq!(entry.claude_session_uuid.as_deref(), Some("sid-1"));
        // Birth facts the conversion observed nothing about stay put.
        assert_eq!(entry.name, "worker-one");
        assert_eq!(entry.node.as_deref(), Some("x-node"));
    }

    #[test]
    fn a_new_id_rolls_back_unless_it_was_authorized() {
        let verdict = id_verdict("old", Some("new"), false, false);
        let IdVerdict::RollBack(reason) = verdict else {
            panic!("an unauthorized new id must roll back");
        };
        // Both ids are named: the operator needs to know what it minted.
        assert!(reason.contains("old"), "{reason}");
        assert!(reason.contains("new"), "{reason}");
        assert!(reason.contains("--allow-new-id"), "{reason}");

        assert_eq!(
            id_verdict("old", Some("new"), true, false),
            IdVerdict::AcceptedNew {
                old: "old".to_string(),
                new: "new".to_string()
            }
        );
    }

    #[test]
    fn a_teamed_row_refuses_a_new_id_even_when_it_is_authorized() {
        let IdVerdict::RollBack(reason) = id_verdict("old", Some("new"), true, true) else {
            panic!("a teamed row must roll back");
        };
        assert!(reason.contains("succession"), "{reason}");
    }
}
