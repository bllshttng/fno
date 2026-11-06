//! The short-lived holder arm: `gate:` keys and `holder-process` leases name
//! the short-lived process that held them, so the recorded pid IS the verdict
//! and a live writing session never heals a dead process's claim. Gate keys
//! read it at any age; `holder-process` leases read it at expiry.
//! Split from claims.rs to keep the over-budget file shrinking.

use super::*;

fn gate_record(pid: i32, acquired_at: i64, expires_at: Option<i64>) -> ClaimRecord {
    // The measured shape (x-dead direction three): holder
    // `spawn-gate:<pid>:<session>` on the global claims root, the pid of
    // the short-lived gate holder, the session id of the long-lived
    // SPAWNING session whose transcript witness used to heal it.
    ClaimRecord {
        key: "gate:spawn".into(),
        holder: format!("spawn-gate:{pid}:t-eacb-operator-pin"),
        session_id: Some("t-eacb-operator-pin".into()),
        ..ClaimRecord {
            schema_version: 1,
            key: String::new(),
            holder: String::new(),
            acquired_at,
            pid: Some(pid),
            host: hostname(),
            pid_unavailable: false,
            expires_at,
            reason: None,
            harness: None,
            session_id: None,
            pid_provenance: None,
            machine_id: None,
            metadata: Default::default(),
        }
    }
}

#[test]
fn an_expired_gate_claim_reads_the_pids_verdict() {
    // Dead holder pid, expired TTL: Stale by pid evidence, not by the
    // clock. Positive marker: the basis names the pid probe's cause.
    let now = now_ms();
    let (state, cause) =
        classify_with_basis(&gate_record(-1, now, Some(now - 1)), Some(now), &probe_pid);
    assert_eq!(state, ClaimState::Stale);
    assert_eq!(cause, basis::PID_ABSENT);
}

#[test]
fn an_unexpired_gate_claim_with_a_refused_probe_stays_suspect() {
    // A refusal is not proof of death.
    let now = now_ms();
    let (state, _cause) = classify_with_basis(
        &gate_record(-1, now, Some(now + 60_000)),
        Some(now),
        &|_| PidProbe::Refused,
    );
    assert_eq!(state, ClaimState::Suspect);
}

fn prov_opts(td: &tempfile::TempDir) -> AcquireOpts {
    AcquireOpts {
        root: Some(td.path().to_path_buf()),
        events_dir: Some(td.path().to_path_buf()),
        ..Default::default()
    }
}

#[test]
fn make_claim_stamps_pid_provenance_by_pid_origin() {
    let td = tempfile::TempDir::new().unwrap();
    // A defaulted pid is the claimant itself: the strongest provenance -
    // but only under a harness that forks per session. The suite's own
    // ambient harness decides which, so the expectation is derived rather
    // than hardcoded; hardcoding it makes this test pass under claude and
    // fail under codex, which is a flake keyed to who ran it.
    let own = match acquire("session:prov", "pty:me", prov_opts(&td)) {
        AcquireOutcome::Acquired(r) => r,
        other => panic!("{other:?}"),
    };
    let expected = if pid_dies_with_session(own.harness.as_deref()) {
        "session-prover"
    } else {
        "ambient"
    };
    assert_eq!(own.pid_provenance.as_deref(), Some(expected));
    // An explicitly passed pid is caller-supplied and unverifiable here:
    // ambient, so the hybrid arm will not extend an expired lease for it.
    let mut o = prov_opts(&td);
    o.pid = Some(4242);
    let foreign = match acquire("session:prov2", "pty:me", o) {
        AcquireOutcome::Acquired(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(foreign.pid_provenance.as_deref(), Some("ambient"));
}
