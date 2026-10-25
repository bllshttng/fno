//! Legacy shell-writer rendezvous hygiene.
//!
//! Until mid-September 2026 the shell append path registered a token under
//! `<events-file>.shell-writers.d/<pid>.<RANDOM>` before every append and
//! removed it afterwards. On machines that ran that window hard, cleanup
//! stopped keeping up: one state root held 21,767 token dirs dated across
//! thirteen days, found 2026-09-30. The append path now commits through the
//! store and registers nothing, so no current writer creates these; what
//! survives on disk is residue no process will ever come back to reclaim.
//!
//! One hourly pass removes every rendezvous entry older than a day (an
//! append is sub-second, so a day-old token is dead by construction), drops
//! the directory once it empties, and emits one row per run including a
//! quiet one, so a silent tick cannot be told from a dead arm. Missing
//! directories - the default on every machine running current code - are a
//! no-op, and an unreadable or future-dated entry is left exactly as found.

use super::*;

/// How old a rendezvous entry must be before the sweep removes it. The
/// registration lived for the length of one append, so anything a day old
/// has no writer left to reclaim it.
const STALE_ENTRY_AGE_SECS: u64 = 86_400;

/// How long between passes. Residue is static - nothing creates new entries
/// - so the floor bounds discovery lag of a machine that regressed, not
/// freshness.
pub(crate) const SHELL_WRITERS_SWEEP_INTERVAL_SECS: i64 = 3_600;

/// What one pass removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ShellWritersSweepReport {
    /// Stale token entries unlinked.
    pub swept: usize,
    /// Rendezvous directories removed once empty.
    pub dirs_removed: usize,
}

/// One hourly pass over the state root, the daemon home, and every project
/// space: the three places a journal (and so its rendezvous) has lived.
pub(crate) fn shell_writers_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    now: i64,
) -> ShellWritersSweepReport {
    let stamp = home.root().join("shell-writers-sweep.stamp");
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    if now.saturating_sub(last) < SHELL_WRITERS_SWEEP_INTERVAL_SECS {
        return ShellWritersSweepReport::default();
    }
    // A dispatch pause suspends the sweep without consuming its cadence: no
    // pass, no stamp write, and one positive skip row per interval so
    // intentional silence cannot read as a dead arm (the sibling sweeps'
    // shape).
    let pause = crate::loops_pause::dispatch_pause();
    if pause.is_paused() {
        let skip_stamp = home.root().join("shell-writers-sweep.skipstamp");
        let last_skip = std::fs::read_to_string(&skip_stamp)
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        if now.saturating_sub(last_skip) >= SHELL_WRITERS_SWEEP_INTERVAL_SECS {
            let _ = emitter.emit(
                "shell_writers_sweep",
                &json!({
                    "outcome": "skipped",
                    "reason": pause.skip_reason(),
                }),
            );
            let _ = std::fs::write(&skip_stamp, now.to_string());
        }
        return ShellWritersSweepReport::default();
    }
    let state_root = home.root().parent().unwrap_or(home.root()).to_path_buf();
    let mut probe_dirs = vec![state_root, home.root().to_path_buf()];
    if let Ok(spaces) = std::fs::read_dir(crate::paths::spaces_root()) {
        for space in spaces.flatten() {
            if space.path().is_dir() {
                probe_dirs.push(space.path());
            }
        }
    }
    let report = shell_writers_sweep_in(&probe_dirs);
    let _ = emitter.emit(
        "shell_writers_sweep",
        &json!({
            "outcome": "ok",
            "swept": report.swept,
            "dirs_removed": report.dirs_removed,
        }),
    );
    let _ = std::fs::write(&stamp, now.to_string());
    report
}

/// The parameterized core, so tests point the pass at their own tree instead
/// of sweeping the machine's real state root. Each probe dir is scanned for
/// exactly one child name: the rendezvous is always
/// `<events-file>.shell-writers.d`, and the journal is always named
/// `events.jsonl`.
pub(crate) fn shell_writers_sweep_in(probe_dirs: &[PathBuf]) -> ShellWritersSweepReport {
    let mut report = ShellWritersSweepReport::default();
    for dir in probe_dirs {
        let rendezvous = dir.join("events.jsonl.shell-writers.d");
        if std::fs::symlink_metadata(&rendezvous).is_err() {
            continue;
        }
        sweep_rendezvous(&rendezvous, &mut report);
    }
    report
}

fn sweep_rendezvous(rendezvous: &Path, report: &mut ShellWritersSweepReport) {
    let Ok(entries) = std::fs::read_dir(rendezvous) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue; // unreadable is not dead; leave it for the next pass
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        // A future-dated entry stays: the clock, not the sweep, owns that
        // verdict.
        let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
            continue;
        };
        if age.as_secs() <= STALE_ENTRY_AGE_SECS {
            continue;
        }
        if remove_stale_entry(&path, meta.is_dir()) {
            report.swept += 1;
        }
    }
    // Empty after the pass? The retired rendezvous has no reason to keep
    // its directory. A failed removal (a writer raced back? permissions?)
    // simply waits for the next pass.
    if std::fs::read_dir(rendezvous)
        .map(|mut inner| inner.next().is_none())
        .unwrap_or(false)
        && std::fs::remove_dir(rendezvous).is_ok()
    {
        report.dirs_removed += 1;
    }
}

/// `is_dir` comes from `symlink_metadata`, so a symlinked entry reads false
/// and unlinks as the link itself - `remove_dir_all` is for real dirs only.
fn remove_stale_entry(path: &Path, is_dir: bool) -> bool {
    if is_dir {
        std::fs::remove_dir_all(path).is_ok()
    } else {
        std::fs::remove_file(path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RENDEZVOUS: &str = "events.jsonl.shell-writers.d";

    fn token(dir: &Path, name: &str) -> PathBuf {
        let token = dir.join(name);
        std::fs::create_dir_all(&token).unwrap();
        std::fs::write(token.join("owner"), "ps output").unwrap();
        token
    }

    /// Rewrite an entry's mtime backwards, the same way session_report's
    /// orphan fixtures age their files. Seconds, not days: the age floor is
    /// exact, so the boundary test needs control below the day grain.
    fn backdate(path: &Path, seconds_ago: u64) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as libc::time_t
            - seconds_ago as libc::time_t;
        let times = [
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
        ];
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid C string from an owned path; the call only sets
        // times. NOFOLLOW, because the sweep judges the entry's own mtime:
        // a symlinked token is aged as the link, never its target.
        unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                c.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            );
        }
    }

    fn probe_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fno-shw{tag}{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn swept_directory_stays_small() {
        // The contract the node asks for by name: residue accumulates only
        // within the last day, so after a pass the directory holds exactly
        // the entries a live writer could still own.
        let dir = probe_dir("small");
        let rendezvous = dir.join(RENDEZVOUS);
        for i in 0..50 {
            let stale = token(&rendezvous, &format!("4021.{i}"));
            backdate(&stale, 2 * 86_400);
        }
        for i in 0..3 {
            token(&rendezvous, &format!("9999.{i}"));
        }
        let report = shell_writers_sweep_in(&[dir.clone()]);
        assert_eq!(
            report,
            ShellWritersSweepReport {
                swept: 50,
                dirs_removed: 0,
            }
        );
        let remaining = std::fs::read_dir(&rendezvous).unwrap().count();
        assert_eq!(remaining, 3, "the directory must stay small after a pass");
    }

    #[test]
    fn fresh_entries_and_the_directory_itself_survive() {
        let dir = probe_dir("fresh");
        let rendezvous = dir.join(RENDEZVOUS);
        token(&rendezvous, "4021.1");
        // The floor is exactly where the sweep may not overreach: an entry
        // a minute inside the window stays even though the clock runs on
        // while the pass reads it.
        let boundary = token(&rendezvous, "4021.2");
        backdate(&boundary, 86_400 - 60);
        let report = shell_writers_sweep_in(&[dir.clone()]);
        assert_eq!(report.swept, 0);
        assert!(rendezvous.is_dir());
        assert!(boundary.join("owner").is_file());
    }

    #[test]
    fn an_emptied_rendezvous_directory_is_removed() {
        let dir = probe_dir("empty");
        let rendezvous = dir.join(RENDEZVOUS);
        let stale = token(&rendezvous, "4021.1");
        backdate(&stale, 3 * 86_400);
        let report = shell_writers_sweep_in(&[dir.clone()]);
        assert_eq!(
            report,
            ShellWritersSweepReport {
                swept: 1,
                dirs_removed: 1,
            }
        );
        assert!(!rendezvous.exists());
        // A young entry in the same directory keeps it: the empty-dir rule
        // never outranks the age rule.
        let dir2 = probe_dir("empty2");
        let rendezvous2 = dir2.join(RENDEZVOUS);
        token(&rendezvous2, "4021.2");
        let report2 = shell_writers_sweep_in(&[dir2]);
        assert_eq!(report2.dirs_removed, 0);
        assert!(rendezvous2.is_dir());
    }

    #[test]
    fn missing_rendezvous_is_a_quiet_no_op() {
        let dir = probe_dir("missing");
        assert_eq!(
            ShellWritersSweepReport::default(),
            shell_writers_sweep_in(&[dir])
        );
    }

    #[test]
    fn a_symlinked_entry_unlinks_as_a_link_and_its_target_survives() {
        let dir = probe_dir("link");
        let rendezvous = dir.join(RENDEZVOUS);
        let target_dir = dir.join("elsewhere");
        std::fs::create_dir_all(&target_dir).unwrap();
        let stale = token(&rendezvous, "4021.1");
        std::fs::remove_dir_all(&stale).unwrap();
        std::os::unix::fs::symlink(&target_dir, &stale).unwrap();
        backdate(&stale, 2 * 86_400);
        let report = shell_writers_sweep_in(&[dir.clone()]);
        assert_eq!(report.swept, 1);
        assert!(target_dir.is_dir(), "the symlink target must survive");
        assert!(!rendezvous.exists(), "the emptied rendezvous is dropped");
    }
}
