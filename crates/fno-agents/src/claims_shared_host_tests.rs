//! The shared-host node-claim arms: a codex claim's recorded pid is the
//! app-server that hosts every thread on the machine, so on `node:`/`task:`
//! keys it must never answer Live by itself. Specimens x-2121/x-2668 and node
//! x-5a80: the worker row was gone, only the host pid answered, and the node
//! stayed claimed.
use super::*;

fn shared_host_record(harness: Option<&str>, now: i64, expires_at: Option<i64>) -> ClaimRecord {
    ClaimRecord {
        schema_version: SCHEMA_VERSION,
        key: "node:x-shared-host".into(),
        holder: "target-session:x".into(),
        acquired_at: now,
        pid: Some(std::process::id() as i32),
        host: hostname(),
        pid_unavailable: false,
        expires_at,
        reason: None,
        harness: harness.map(str::to_string),
        session_id: Some("codex-thread".into()),
        pid_provenance: Some("ambient".into()),
        machine_id: Some(machine_id()),
        metadata: serde_json::Map::new(),
    }
}

#[test]
fn codex_node_claim_no_ttl_live_host_pid_absent_witness_stales() {
    let now = now_ms();
    let rec = shared_host_record(Some("codex"), now, None);
    let witness: SessionWitness = &|_| SessionLiveness::Absent;
    assert_eq!(
        classify_with_basis_and_exclusivity(&rec, Some(now), &probe_pid, None, Some(witness)),
        (ClaimState::Stale, basis::SESSION_ABSENT)
    );
}

#[test]
fn codex_node_claim_no_ttl_live_host_pid_unresolved_witness_suspects() {
    let now = now_ms();
    let rec = shared_host_record(Some("codex"), now, None);
    let witness: SessionWitness = &|_| SessionLiveness::Unresolved;
    assert_eq!(
        classify_with_basis_and_exclusivity(&rec, Some(now), &probe_pid, None, Some(witness)),
        (ClaimState::Suspect, basis::PID_SHARED)
    );
}

#[test]
fn codex_node_claim_no_ttl_live_host_pid_witness_live_stays_live() {
    let now = now_ms();
    let rec = shared_host_record(Some("codex"), now, None);
    let witness: SessionWitness = &|_| SessionLiveness::Live(basis::REGISTRY_SESSION_LIVE);
    assert_eq!(
        classify_with_basis_and_exclusivity(&rec, Some(now), &probe_pid, None, Some(witness)),
        (ClaimState::Live, basis::REGISTRY_SESSION_LIVE)
    );
}

#[test]
fn codex_node_claim_no_ttl_live_host_pid_without_session_suspects() {
    let now = now_ms();
    let mut rec = shared_host_record(Some("codex"), now, None);
    rec.session_id = None;
    assert_eq!(
        classify_with_basis_and_exclusivity(&rec, Some(now), &probe_pid, None, None),
        (ClaimState::Suspect, basis::PID_SHARED)
    );
}

#[test]
fn codex_node_claim_ttl_window_live_host_pid_without_session_suspects() {
    let now = now_ms();
    let mut rec = shared_host_record(Some("codex"), now, Some(now + 7_200_000));
    rec.session_id = None;
    assert_eq!(
        classify_with_basis_and_exclusivity(&rec, Some(now), &probe_pid, None, None),
        (ClaimState::Suspect, basis::PID_SHARED)
    );
}

#[test]
fn per_session_host_node_claim_no_ttl_live_pid_stays_live() {
    // The must-not-break twin: a harness whose pid IS the session keeps the
    // pid-liveness reading on every arm.
    let now = now_ms();
    let rec = shared_host_record(Some("claude"), now, None);
    assert_eq!(
        classify_with_basis_and_exclusivity(&rec, Some(now), &probe_pid, None, None),
        (ClaimState::Live, basis::LIVE)
    );
}

#[test]
fn codex_non_node_key_no_ttl_live_pid_keeps_pid_reading() {
    // Scope guard: the node's title names NODE claims; other keys keep the
    // bare-pid reading until a specimen widens the contract.
    let now = now_ms();
    let mut rec = shared_host_record(Some("codex"), now, None);
    rec.key = "review:branch:x".into();
    assert_eq!(
        classify_with_basis_and_exclusivity(&rec, Some(now), &probe_pid, None, None),
        (ClaimState::Live, basis::LIVE)
    );
}
