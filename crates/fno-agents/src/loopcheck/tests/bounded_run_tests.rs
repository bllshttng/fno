use super::*;

// Hermetic: every child is a stub script in a tempdir, so no test touches
// a real `gh` or `fno`, and every timing case asserts wall-clock bounds
// wide enough to survive parallel test scheduling.

#[test]
fn bounded_run_retains_the_last_stderr_bytes_not_the_first() {
    let tmp = tempfile::tempdir().unwrap();
    // A uniform fill followed by a marker at the END of the stream: the
    // retained bytes must carry the marker. A cap that freezes at the
    // first chunks (head-keeping) drops exactly this marker, and the
    // uniform-fill sibling above cannot tell head from tail - only this
    // shape distinguishes them.
    let fno = write_exec(
            tmp.path(),
            "fno",
            "#!/bin/sh\nhead -c 5000 /dev/zero | tr '\\0' 'x' 1>&2\necho TAIL_MARKER 1>&2\necho '{\"ok\":true}'\n",
        );
    let cwd = std::env::temp_dir();
    match run_bounded(fno.as_os_str(), &[], &cwd, PROBE_TIMEOUT) {
        BoundedRun::Completed(out) => {
            let tail = String::from_utf8_lossy(&out.stderr_tail);
            assert!(tail.ends_with("TAIL_MARKER\n"), "retained {tail:?}");
            assert_eq!(
                out.stderr_tail.len(),
                BOUNDED_STDERR_TAIL_CAP,
                "a stream past the cap retains exactly the cap"
            );
        }
        other => panic!("expected Completed, got {}", bounded_kind(&other)),
    }
}

#[test]
fn bounded_read_diagnostic_preserves_transport_classification() {
    let timeout = GhReadError::timed_out(
        "main_run_view",
        std::time::Duration::from_secs(1),
        String::new(),
    );
    let rendered = bounded_read_diagnostic("main-head", &timeout);
    assert!(rendered.contains("main-head"), "{rendered}");
    assert!(rendered.contains("main_run_view"), "{rendered}");
    assert!(rendered.contains("outcome=timeout"), "{rendered}");
    assert!(rendered.contains("elapsed_s=1.0"), "{rendered}");

    let spawn = GhReadError::unrunnable("git_status", "spawn failed");
    let rendered = bounded_read_diagnostic("payload", &spawn);
    assert!(rendered.contains("git_status"), "{rendered}");
    assert!(rendered.contains("outcome=unrunnable"), "{rendered}");
    assert!(rendered.contains("spawn failed"), "{rendered}");
}

#[test]
fn bounded_run_kills_a_forked_process_group_on_timeout() {
    // A descendant keeps the stderr pipe open past the leader's death;
    // only a process-GROUP kill reaps it, and the run must resolve as a
    // distinct timeout inside the wall-clock budget, never as a hang.
    let tmp = tempfile::tempdir().unwrap();
    let fno = write_exec(
        tmp.path(),
        "fno",
        "#!/bin/sh\n(sleep 30 &) 1>&2\nsleep 30\n",
    );
    let cwd = std::env::temp_dir();
    let started = std::time::Instant::now();
    match run_bounded(
        fno.as_os_str(),
        &[],
        &cwd,
        std::time::Duration::from_millis(200),
    ) {
        BoundedRun::TimedOut(elapsed, _) => {
            assert!(elapsed >= std::time::Duration::from_millis(200));
            // Cleanup slack for spawn + group kill, generous for parallel
            // test scheduling: the point is "bounded", not "precise".
            assert!(started.elapsed() < std::time::Duration::from_secs(10));
        }
        other => panic!("expected TimedOut, got {}", bounded_kind(&other)),
    }
}

/// Render a `BoundedRun` variant name for assertion failures without a
/// Debug derive on the payload-carrying enum.
fn bounded_kind(run: &BoundedRun) -> &'static str {
    match run {
        BoundedRun::Completed(_) => "Completed",
        BoundedRun::TimedOut(..) => "TimedOut",
        BoundedRun::SpawnFailed(_) => "SpawnFailed",
        BoundedRun::WaitFailed => "WaitFailed",
        BoundedRun::Refused => "Refused",
    }
}

/// A bound at the spent line refuses without spawning: enough floored reads
/// would spend the drain's reserved slice, so the transport refuses instead.
#[test]
fn a_bound_at_the_spent_line_refuses_without_spawning() {
    let deadline = std::time::Instant::now();
    crate::loopcheck::stopgate_stamp_fire(0, deadline, 16_000);
    let tmp = tempfile::tempdir().unwrap();
    let run = run_bounded(
        std::ffi::OsStr::new("/bin/sh"),
        &["-c", "echo spawned"],
        tmp.path(),
        crate::loopcheck::read_bounds::STOPGATE_PRE_DRAIN_SPENT_BOUND,
    );
    assert!(
        matches!(run, BoundedRun::Refused),
        "got {}",
        bounded_kind(&run)
    );
}

/// A timeout keeps the stderr the child printed before the bound, and the
/// cause field reads only what the row proves: table over (tail -> waited_on)
/// plus one live kill proving the tail survives.
#[test]
fn timeout_rows_keep_their_tail_and_waited_on_reads_row_evidence_only() {
    // The classifier table: evidence in, cause out, nothing guessed.
    let timeout = |tail: &str| {
        GhReadError::timed_out("read", std::time::Duration::from_secs(1), tail.to_string())
    };
    for (tail, want) in [
        ("", "unknown"),
        ("gh: Could not resolve host", "network"),
        ("gh budget: held locally", "budget_lock"),
    ] {
        assert_eq!(timeout(tail).waited_on(), want, "tail {tail:?}");
    }
    let failed = GhReadError::failed("read", "gh: HTTP 403 secondary rate limit".to_string());
    assert_eq!(failed.waited_on(), "github");
    let failed_plain = GhReadError::failed("read", "exit 1".to_string());
    assert_eq!(failed_plain.waited_on(), "unknown");
    // The live kill: the tail the child printed before the bound survives
    // into the timeout payload, the evidence the table above reads.
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_exec(
        tmp.path(),
        "chatty",
        "#!/bin/sh\necho 'gh: Could not resolve host' 1>&2\nsleep 30\n",
    );
    let run = run_bounded(
        stub.as_os_str(),
        &[],
        tmp.path(),
        std::time::Duration::from_millis(300),
    );
    match run {
        BoundedRun::TimedOut(_, tail) => {
            let tail = String::from_utf8_lossy(&tail);
            assert!(tail.contains("Could not resolve host"), "tail {tail:?}");
        }
        other => panic!("expected TimedOut, got {}", bounded_kind(&other)),
    }
}

#[test]
fn no_direct_external_read_bypasses_bounded_runner() {
    // Production region only: test modules may legitimately spawn helper
    // processes. Every child module is production too, so a bypass hiding
    // in a named-by-question child cannot dodge a root-only scan.
    let production = production_source();
    // Positive control first, so an empty scan can never read as green:
    // the centralized runner must exist and carry real call sites.
    assert!(production.contains("fn run_bounded("));
    assert!(
        production.matches("bounded_read(").count() >= 10,
        "the bounded transport must carry its registered read sites"
    );
    assert!(
        production.matches("git_bounded(").count() >= 10,
        "the bounded transport must carry the stop-gate git read sites"
    );
    let bypasses = direct_wait_bypasses(&production);
    assert!(
        bypasses.is_empty(),
        "direct synchronous gh/fno/git waits outside the bounded runner: {bypasses:?}"
    );
}

/// ENOENT from an EXISTING script (its shebang interpreter is missing)
/// must read as spawn trouble, not absence: gh is present on disk, so
/// the session must not degrade to advisory mode over an interpreter
/// problem. The same ENOENT with no file at the path is real absence.
#[test]
fn probe_distinguishes_a_missing_interpreter_from_absence() {
    let tmp = tempfile::tempdir().unwrap();
    let gh = write_exec(tmp.path(), "gh", "#!/definitely/not/an/interp\nexit 0\n");
    assert_eq!(
        probe_gh_bin(gh.as_os_str(), tmp.path()),
        GhProbeOutcome::SpawnTrouble {
            kind: std::io::ErrorKind::NotFound
        }
    );
}

/// THE acceptance for the spawn-trouble class: an existing 644 file
/// fails to spawn with EACCES, a spawn error that is NOT absence, and
/// the probe must say spawn trouble - never "gh is absent".
#[test]
fn probe_reports_spawn_trouble_not_absent_for_a_non_executable_file() {
    let tmp = tempfile::tempdir().unwrap();
    let gh = tmp.path().join("gh-644");
    std::fs::write(&gh, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let outcome = probe_gh_bin(gh.as_os_str(), tmp.path());
    assert_ne!(outcome, GhProbeOutcome::Absent);
    assert!(matches!(outcome, GhProbeOutcome::SpawnTrouble { .. }));
    assert_eq!(outcome.outcome_str(), "spawn_trouble");
}
