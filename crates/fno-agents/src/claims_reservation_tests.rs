//! Key-class arm tests for the claim classifier, moved out of claims.rs when
//! the file crossed the 5,000-line shrink-only budget (x-41f7): the
//! classifier itself stays in claims.rs, these pins live beside it.

use super::*;

/// One `dispatch:`-shaped record: the holder names the dispatching pid and
/// the session stamp is the dispatcher's, the measured shape of every
/// boot-window reservation (ambient provenance, 10 of 10 dead pids).
fn reservation(pid: i32, expires_at: Option<i64>, host: &str) -> ClaimRecord {
    ClaimRecord {
        schema_version: 1,
        key: "dispatch:x-t".into(),
        holder: format!("spawn-cli:{pid}"),
        acquired_at: now_ms(),
        pid: Some(pid),
        host: host.into(),
        pid_unavailable: false,
        expires_at,
        reason: None,
        harness: None,
        session_id: Some("s-dispatcher".into()),
        pid_provenance: Some("ambient".into()),
        machine_id: None,
        metadata: serde_json::Map::new(),
    }
}

#[test]
fn expired_reservation_resolves_through_the_pid_it_names() {
    // AC4/AC5: the witness says the DISPATCHER's session is live - the
    // old reading kept the reservation bucket-live off exactly that. The
    // pid the holder names is the verdict instead: dead -> Stale and
    // provably dead, live -> Live and protected.
    let me = std::process::id() as i32;
    let host = hostname();
    let now = now_ms();
    let witness: SessionWitness = &|_| SessionLiveness::Live(basis::REGISTRY_SESSION_LIVE);

    let dead = reservation(-1, Some(now - 1), &host);
    let (state, cause) =
        classify_with_basis_and_exclusivity(&dead, Some(now), &probe_pid, None, Some(witness));
    assert_eq!((state, cause), (ClaimState::Stale, basis::PID_ABSENT));
    let (provably_dead, _) = classify_for_sweep(&dead, Some(now), &probe_pid, None, Some(witness));
    assert!(provably_dead);

    let live = reservation(me, Some(now - 1), &host);
    let (state, _) =
        classify_with_basis_and_exclusivity(&live, Some(now), &probe_pid, None, Some(witness));
    assert_eq!(state, ClaimState::Live);
    let (provably_dead, bucket) =
        classify_for_sweep(&live, Some(now), &probe_pid, None, Some(witness));
    assert!(!provably_dead);
    assert_eq!(bucket, "live");

    // Scoping: the arm is keyed on the dispatch: namespace. The same
    // expired shape under another key keeps answering from the session
    // witness, byte-for-byte.
    let mut other_key = reservation(-1, Some(now - 1), &host);
    other_key.key = "node:x-t".into();
    let (state, cause) =
        classify_with_basis_and_exclusivity(&other_key, Some(now), &probe_pid, None, Some(witness));
    assert_eq!(
        (state, cause),
        (ClaimState::Live, basis::REGISTRY_SESSION_LIVE)
    );
}

/// One `review:branch:`-shaped record: the hold a review dispatch takes,
/// carrying a session-prover pid and the reviewer session's own stamp - the
/// shape that let a TTL-lapsed hold read Live off its live session (x-b5f6).
fn review_hold(pid: i32, expires_at: Option<i64>, host: &str) -> ClaimRecord {
    ClaimRecord {
        schema_version: 1,
        key: "review:branch:feature/x".into(),
        holder: "review-session:s-reviewer".into(),
        acquired_at: now_ms(),
        pid: Some(pid),
        host: host.into(),
        pid_unavailable: false,
        expires_at,
        reason: None,
        harness: Some("claude".into()),
        session_id: Some("s-reviewer".into()),
        pid_provenance: Some("session-prover".into()),
        machine_id: None,
        metadata: serde_json::Map::new(),
    }
}

#[test]
fn an_unexpired_review_hold_with_a_dead_pid_still_protects() {
    // Inside the TTL the hold keeps its Suspect arm and review_activity keeps
    // blocking on it; only the expired arm changed.
    let host = hostname();
    let now = now_ms();
    let unexpired = review_hold(-1, Some(now + 60_000), &host);
    let (state, cause) = classify_with_basis(&unexpired, Some(now), &probe_pid);
    assert_eq!((state, cause), (ClaimState::Suspect, basis::PID_ABSENT));
}

/// One `blueprint-session:`-shaped record: a native subagent planner's node
/// claim, stamped by `session open` with the PARENT's pid (ambient
/// provenance) and the parent's session id - the shape that read Live for
/// the parent's whole life after a mid-flow TaskStop.
fn blueprint_hold(pid: i32, acquired_at: i64, expires_at: Option<i64>) -> ClaimRecord {
    ClaimRecord {
        schema_version: 1,
        key: "node:x-t".into(),
        holder: "blueprint-session:s-parent".into(),
        acquired_at,
        pid: Some(pid),
        host: hostname().into(),
        pid_unavailable: false,
        expires_at,
        reason: None,
        harness: Some("claude".into()),
        session_id: Some("s-parent".into()),
        pid_provenance: Some("ambient".into()),
        machine_id: None,
        metadata: serde_json::Map::new(),
    }
}

#[test]
fn a_blueprint_claim_past_its_lease_is_stale_while_the_parent_lives() {
    // The lease is clock-only, and the parent's pid and transcript are
    // exactly the evidence that outlives a stopped planner: the arm sits
    // above both the hybrid and the witness, so the strongest live
    // evidence still cannot heal a lapsed claim. The stub probe (created
    // at epoch 0) is a deterministically live parent.
    let me = std::process::id() as i32;
    let now = now_ms();
    let probe = |_pid: i32| PidProbe::Created(0);
    let witness: SessionWitness = &|_| SessionLiveness::Live(basis::TRANSCRIPT_LIVE);

    let lapsed = blueprint_hold(me, now - BLUEPRINT_LEASE_MS - 1, None);
    let (state, cause) =
        classify_with_basis_and_exclusivity(&lapsed, Some(now), &probe, None, Some(witness));
    assert_eq!((state, cause), (ClaimState::Stale, basis::TTL_EXPIRED));
    let (provably_dead, _) = classify_for_sweep(&lapsed, Some(now), &probe, None, Some(witness));
    assert!(provably_dead);
}

#[test]
fn the_lease_is_scoped_to_the_blueprint_holder() {
    // The same stale-shaped record under any other holder keeps today's
    // verdict - the pid alone answers Live - so no other claim class
    // flips polarity.
    let me = std::process::id() as i32;
    let now = now_ms();
    let probe = |_pid: i32| PidProbe::Created(0);
    let witness: SessionWitness = &|_| SessionLiveness::Live(basis::TRANSCRIPT_LIVE);

    let mut other = blueprint_hold(me, now - BLUEPRINT_LEASE_MS - 1, None);
    other.holder = "target-session:s-parent".into();
    let (state, _) =
        classify_with_basis_and_exclusivity(&other, Some(now), &probe, None, Some(witness));
    assert_eq!(state, ClaimState::Live);
}
