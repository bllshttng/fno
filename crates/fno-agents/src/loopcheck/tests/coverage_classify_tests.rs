use super::*;

#[test]
fn refused_invocation_row_reads_reviewer_refused_not_unreviewed() {
    // The measured 2026-08-30 empty-diff shape: a review ran at a
    // recipient sitting on the base branch, read nothing, and the producer
    // refused to attest. Before the refused terminal row, that attempt was
    // byte-identical to "never attempted" at every coverage surface, so a
    // lead re-fired the review and reproduced the failure exactly.
    let events = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "review_invocation",
            "data": {"invocation_id": "ri-1", "stage": "refused",
                     "verb": "/review", "reason": "empty_diff",
                     "head_sha": "abc123", "branch": "feature/x"}
        })
        .to_string(),
        serde_json::json!({
            "type": "review_invocation",
            "data": {"invocation_id": "ri-1", "stage": "sent",
                     "verb": "/review"}
        })
        .to_string(),
    );
    let rep = classify_coverage(
        &[],
        &[],
        &events,
        &[],
        true,
        None,
        &|_| Freshness::Stale,
        "feature/x",
        "abc123",
    );
    assert_eq!(rep.coverage, Coverage::Covered(0), "a refusal never covers");
    assert_eq!(rep.review_state(), Some(ReviewState::ReviewerRefused));
    assert_eq!(rep.refused_reviewers(), vec!["review"]);
    assert_eq!(
        rep.verdicts[0].refusal_reason.as_deref(),
        Some("empty_diff"),
        "the refusal class rides the verdict so a reader names the cause"
    );
    // A real review outranks the refusal: once something Reviewed exists,
    // the state must not stay parked at ReviewerRefused.
    let with_pass = format!(
        "{}{}\n",
        events,
        attestation_line("code-review", "abc123", "pass")
    );
    let rep = classify_coverage(
        &[],
        &[],
        &with_pass,
        &[],
        true,
        None,
        &|_| Freshness::Fresh,
        "feature/x",
        "abc123",
    );
    assert_eq!(rep.review_state(), Some(ReviewState::Reviewed));
}

#[test]
fn github_app_verdict_at_an_older_commit_is_stale_and_uncovers_the_pr() {
    // THE specimen. Before this, the github_app axis read `state !=
    // ""` and never asked which commit the review was submitted against,
    // so this exact payload produced `coverage: covered, reviewed_count:
    // 1` for a commit codex never saw. The state is non-empty here on
    // purpose: that is the whole of what the old rule looked at.
    let rep = classify_coverage(
        &pr826_reviews(),
        &[],
        "",
        &["chatgpt-codex-connector".to_string()],
        true,
        None,
        &|_| Freshness::Stale,
        "",
        "",
    );
    let v = &rep.verdicts[0];
    assert_eq!(v.verdict, CoverageVerdict::Stale);
    assert_eq!(v.freshness, Some(Freshness::Stale));
    assert_eq!(v.reviewed_sha, "8e557ccdecec07abc7e409ad8d888318016612c1");
    assert_eq!(rep.coverage, Coverage::Covered(0));
    // And the word a human reads now agrees with the number beside it.
    let data = coverage_event_data(826, &rep, "89bc0b91", "", None);
    assert_eq!(data["coverage"], serde_json::json!("uncovered"));
    assert_eq!(data["reviewed_count"], serde_json::json!(0));
}

#[test]
fn github_app_review_without_a_commit_oid_fails_closed() {
    // An older review object, or a payload shape change, leaves no commit
    // to pin. That is an absence, and an absence is never freshness.
    let reviews = vec![serde_json::json!({
        "author": {"login": "chatgpt-codex-connector"},
        "state": "APPROVED"
    })];
    let rep = classify_coverage(
        &reviews,
        &[],
        "",
        &["chatgpt-codex-connector".to_string()],
        true,
        None,
        // The real predicate, not a stub: an empty sha must reach Stale on
        // its own rather than because a fake said so.
        &|sha| review_freshness(sha, "89bc0b91", &FreshnessFacts::default()),
        "",
        "",
    );
    assert_eq!(rep.verdicts[0].verdict, CoverageVerdict::Stale);
    assert_eq!(rep.coverage, Coverage::Covered(0));
}

#[test]
fn a_zero_line_attestation_is_not_review_evidence() {
    // A pass over a zero-line diff is a review of nothing: a session
    // resolving its target from a checkout sitting on the base branch
    // reads an empty diff, reports clean, and before this guard that
    // pass counted exactly like a real one. The producer refuses to emit
    // at 0, so a line carrying the field as 0 is either pre-guard or
    // hand-crafted; skipped either way. The control pins that the field
    // ABSENT (the pre-landed backlog) still counts - absence must never
    // be read as zero.
    let zero = serde_json::json!({
        "ts": "2026-01-01T00:00:00Z", "source": "test",
        "type": "review_attestation",
        "data": {"reviewer": "code-review", "head_sha": "h", "verdict": "pass",
                 "attester_session_id": "sess-author", "reviewed_line_count": 0}
    })
    .to_string();
    let rep = classify_coverage(
        &[],
        &[],
        &zero,
        &[],
        false,
        None,
        &|_| Freshness::Fresh,
        "",
        "h",
    );
    assert!(
        rep.verdicts.is_empty(),
        "a zero-line pass must yield no verdict"
    );
    assert_eq!(rep.coverage, Coverage::Unknown);

    let control = attestation_line("code-review", "h", "pass");
    let rep = classify_coverage(
        &[],
        &[],
        &control,
        &[],
        false,
        None,
        &|_| Freshness::Fresh,
        "",
        "h",
    );
    assert_eq!(rep.coverage, Coverage::Covered(1));
}

#[test]
fn a_retraction_revokes_the_named_pair_not_the_retractor() {
    // The retraction command addresses the EVENT, not the identity: an
    // operator session emits a fail carrying retracts_attester naming the
    // pair that passed. The pair-keyed scan must revoke THAT pair while
    // the retracting session's own entry stays untouched - undoing an
    // impersonation must not require performing it a second time.
    let pass = serde_json::json!({
        "ts": "2026-01-01T00:00:00Z", "source": "test",
        "type": "review_attestation",
        "data": {"reviewer": "code-review", "head_sha": "h", "verdict": "pass",
                 "attester_session_id": "sess-A", "branch": "feature/x"}
    })
    .to_string();
    let control = classify_coverage(
        &[],
        &[],
        &pass,
        &[],
        false,
        None,
        &|_| Freshness::Fresh,
        "feature/x",
        "h",
    );
    assert_eq!(control.coverage, Coverage::Covered(1));

    let retraction = serde_json::json!({
        "ts": "2026-01-01T00:00:00Z", "source": "test",
        "type": "review_attestation",
        "data": {"reviewer": "code-review", "head_sha": "h", "verdict": "fail",
                 "attester_session_id": "sess-operator",
                 "retracts_attester": "sess-A", "branch": "feature/x"}
    })
    .to_string();
    let revoked = classify_coverage(
        &[],
        &[],
        &format!("{}\n{}", pass, retraction),
        &[],
        false,
        None,
        &|_| Freshness::Fresh,
        "feature/x",
        "h",
    );
    assert!(
        revoked.verdicts.is_empty(),
        "the named pair must yield no verdict"
    );
    assert_eq!(revoked.coverage, Coverage::Unknown);
}
