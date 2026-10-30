use super::*;

/// A cancelled check is NO verdict: a run the tier keeper cancelled (or a
/// newer push superseded) reads Pending - the legal wait - never a live red
/// that refuses every idle ending. A skip sibling never masks a real
/// fail.
#[test]
fn ci_conclusion_cancel_only_is_pending() {
    let cancel_only = serde_json::json!([
        {"name": "deploy", "state": "CANCELLED", "bucket": "cancel"}
    ]);
    assert_eq!(
        compute_ci_conclusion(&cancel_only).unwrap(),
        CiConclusion::Pending
    );
    let pass_and_cancel = serde_json::json!([
        {"name": "ci", "state": "SUCCESS", "bucket": "pass"},
        {"name": "deploy", "state": "CANCELLED", "bucket": "cancel"}
    ]);
    assert_eq!(
        compute_ci_conclusion(&pass_and_cancel).unwrap(),
        CiConclusion::Pending
    );
}

/// A `fail` bucket is a verdict, beside any number of cancels.
#[test]
fn ci_conclusion_fail_beside_cancels_is_failure() {
    let checks = serde_json::json!([
        {"name": "deploy", "state": "CANCELLED", "bucket": "cancel"},
        {"name": "smoke", "state": "FAILED", "bucket": "fail"}
    ]);
    assert_eq!(
        compute_ci_conclusion(&checks).unwrap(),
        CiConclusion::Failure(Some("smoke".to_string()))
    );
}

/// An unknown or missing bucket fails closed as Pending, never green.
#[test]
fn ci_conclusion_unknown_bucket_fails_closed() {
    let unknown = serde_json::json!([
        {"name": "ci", "state": "SUCCESS", "bucket": "mystery"}
    ]);
    assert_eq!(
        compute_ci_conclusion(&unknown).unwrap(),
        CiConclusion::Pending
    );

    let missing = serde_json::json!([{"name": "ci", "state": "SUCCESS"}]);
    assert_eq!(
        compute_ci_conclusion(&missing).unwrap(),
        CiConclusion::Pending
    );
}

#[test]
fn coverage_statuses_are_not_generic_ci_checks() {
    let checks = serde_json::json!([
        {"name": "ci", "bucket": "pass"},
        {"name": "fno/review-coverage", "bucket": "pending"},
        {"name": "fno/review-coverage-unavailable", "bucket": "pending"}
    ]);
    let filtered = without_coverage_statuses(&checks);
    assert_eq!(filtered.as_array().unwrap().len(), 1);
    assert_eq!(
        compute_ci_conclusion(&filtered).unwrap(),
        CiConclusion::Success
    );
    assert!(failing_check_names(&filtered).is_empty());
    assert!(!ci_has_pending_checks(&filtered));
}

#[test]
fn ci_has_pending_gates_partial_ci() {
    // One check failed while another still runs -> pending (must hold, not
    // terminate: the pending job could be the session's own new red).
    let partial = serde_json::json!([
        {"name": "smoke",   "bucket": "fail"},
        {"name": "rust-ci", "bucket": "pending"},
    ]);
    assert!(ci_has_pending_checks(&partial));
    // Fully settled red -> no pending -> eligible for the terminal.
    let settled = serde_json::json!([
        {"name": "smoke",   "bucket": "fail"},
        {"name": "rust-ci", "bucket": "pass"},
        {"name": "doc",     "bucket": "skipping"},
    ]);
    assert!(!ci_has_pending_checks(&settled));
    // Unrecognized bucket is treated as pending (fail safe).
    let unknown = serde_json::json!([{"name": "x", "bucket": "queued"}]);
    assert!(ci_has_pending_checks(&unknown));
    // Malformed input never panics.
    assert!(!ci_has_pending_checks(&serde_json::json!({})));
}

#[test]
fn latest_per_name_drops_superseded_cancel_for_newer_pass() {
    // The dedup is what makes the non-terminal cancel safe: a superseded
    // cancel from an earlier push loses to the newer same-name pass, so
    // the terminal is held only by a cancel that IS the latest run.
    let checks = serde_json::json!([
        {"name": "ci", "bucket": "cancel", "startedAt": "2026-08-15T00:00:00Z"},
        {"name": "ci", "bucket": "pass",  "startedAt": "2026-08-15T01:00:00Z"},
        {"name": "doc", "bucket": "skipping", "startedAt": "2026-08-15T01:00:00Z"},
    ]);
    let deduped = latest_per_name(&checks);
    assert!(!ci_has_pending_checks(&deduped));
    assert!(failing_check_names(&deduped).is_empty());
    // Recency keys on TRIGGER time: a slow superseded pass completing
    // later must not displace a newer fast fail.
    let swapped = serde_json::json!([
        {"name": "ci", "bucket": "pass", "startedAt": "2026-08-15T01:00:00Z"},
        {"name": "ci", "bucket": "cancel", "startedAt": "2026-08-15T00:00:00Z"},
    ]);
    let deduped = latest_per_name(&swapped);
    assert!(!ci_has_pending_checks(&deduped));
    assert!(failing_check_names(&deduped).is_empty());
}

#[test]
fn latest_per_name_keeps_same_name_checks_from_different_workflows() {
    // codex P1: several workflows in this repo define a job literally
    // named "self-test". A name-only key folds a cancelled self-test
    // from one workflow into a passing self-test from another, hiding
    // the real failure behind an unrelated workflow's pass. The key
    // must be (name, workflow), so both entries survive the dedup.
    let checks = serde_json::json!([
        {"name": "self-test", "bucket": "cancel", "workflow": "control-plane-doc-colocation",
         "startedAt": "2026-08-15T01:00:00Z"},
        {"name": "self-test", "bucket": "pass", "workflow": "loc-ratchet",
         "startedAt": "2026-08-15T00:00:00Z"},
    ]);
    let deduped = latest_per_name(&checks);
    assert_eq!(deduped.as_array().unwrap().len(), 2);
    assert!(ci_has_pending_checks(&deduped));
    assert_eq!(failing_check_names(&deduped), vec!["self-test".to_string()]);
}
