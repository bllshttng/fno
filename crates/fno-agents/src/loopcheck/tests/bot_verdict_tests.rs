use super::*;

#[test]
fn bot_verdict_fresh_clean_pass_supersedes_stale_review_object() {
    // The normal fix cycle: findings review at H1, fix push, clean-pass
    // comment at H2. Coverage used to hold the stale H1 object and never
    // read the comment while the presence gate passed - the two-gate
    // wedge. Both now read Reviewed through one predicate.
    let reviews = vec![stale_findings_review()];
    let comments = vec![clean_pass_comment("abc12345")];
    let fresh_at = fresh_at_head();
    let (verdict, sha, _) = bot_verdict("chatgpt-codex-connector", &reviews, &comments, &fresh_at);
    assert_eq!(verdict, CoverageVerdict::Reviewed);
    assert_eq!(sha, "abc12345");
    let json = serde_json::json!({"reviews": reviews, "comments": comments});
    let info = compute_review_info(&json, &["chatgpt-codex-connector".to_string()], &fresh_at);
    assert!(info.all_required_passed());
}

#[test]
fn bot_verdict_usage_after_clean_pass_refuses_both_axes() {
    // Clean pass pinned at HEAD, then a quota bounce. The pass does not
    // silently outlive the bot's own later refusal, and the presence gate
    // no longer passes a bot coverage reads as Refused.
    let comments = vec![
        clean_pass_comment("abc12345"),
        usage_comment("2026-08-17T03:00:00Z"),
    ];
    let fresh_at = fresh_at_head();
    let (verdict, _, _) = bot_verdict("chatgpt-codex-connector", &[], &comments, &fresh_at);
    assert_eq!(verdict, CoverageVerdict::Refused);
    let json = serde_json::json!({"reviews": [], "comments": comments});
    let info = compute_review_info(&json, &["chatgpt-codex-connector".to_string()], &fresh_at);
    assert!(!info.all_required_passed());
    assert_eq!(
        info.reviewer_refused,
        vec!["chatgpt-codex-connector".to_string()]
    );
}

#[test]
fn clean_pass_marker_with_minor_findings_is_not_a_pass() {
    // The bot posts this exact clause while REPORTING findings; a bare
    // substring marker cleared both gates on a PR that has them.
    let comments = vec![serde_json::json!({
        "author": {"login": "chatgpt-codex-connector[bot]"},
        "body": "Codex Review: Didn't find any major issues, but 2 minor ones need attention. Reviewed commit: abc12345",
        "createdAt": "2026-08-17T02:00:00Z"
    })];
    let fresh_at = fresh_at_head();
    assert!(clean_pass_review(&comments, "chatgpt-codex-connector", &fresh_at).is_none());
    let json = serde_json::json!({"reviews": [], "comments": comments});
    let info = compute_review_info(&json, &["chatgpt-codex-connector".to_string()], &fresh_at);
    assert_eq!(
        info.missing_bots,
        vec!["chatgpt-codex-connector".to_string()]
    );
}

#[test]
fn human_login_substring_of_bot_cannot_satisfy_the_gate() {
    // A drive-by human "codex" posting a review object used to clear the
    // required bot "chatgpt-codex-connector" through the symmetric
    // correspond test; the gate has always been one-way.
    let reviews = vec![serde_json::json!({
        "author": {"login": "codex"},
        "state": "COMMENTED",
        "commit": {"oid": "abc12345"}
    })];
    let fresh_at = fresh_at_head();
    let (verdict, _, _) = bot_verdict("chatgpt-codex-connector", &reviews, &[], &fresh_at);
    assert_eq!(verdict, CoverageVerdict::Absent);
    let json = serde_json::json!({"reviews": reviews, "comments": []});
    let info = compute_review_info(&json, &["chatgpt-codex-connector".to_string()], &fresh_at);
    assert_eq!(
        info.missing_bots,
        vec!["chatgpt-codex-connector".to_string()]
    );
}

#[test]
fn equal_rank_tie_breaks_on_the_stamp_not_the_array_order() {
    // The same three comments as the test above, delivered NEWEST FIRST.
    // Array order is the payload's promise, not ours: resolving the
    // equal-rank tie with `>=` over iteration order picked the 01:00 pass
    // here, the 02:00 bounce then postdated it, and a required bot fell
    // through Refused -> usage_limited -> all_required_passed false. The
    // parsed `createdAt` orders it the same way whatever order it arrives
    // in, which is the rule `bot_verdict` already applies to `submittedAt`.
    let pass = |at: &str| {
        serde_json::json!({
            "author": {"login": "chatgpt-codex-connector[bot]"},
            "body": "Codex Review: Didn't find any major issues. Bravo. Reviewed commit: abc12345",
            "createdAt": at
        })
    };
    let comments = vec![
        pass("2026-08-17T03:00:00Z"),
        pass("2026-08-17T01:00:00Z"),
        usage_comment("2026-08-17T02:00:00Z"),
    ];
    let fresh_at = fresh_at_head();
    let got = clean_pass_review(&comments, "chatgpt-codex-connector", &fresh_at);
    assert_eq!(
        got.and_then(|(_, _, ts)| ts).as_deref(),
        Some("2026-08-17T03:00:00Z"),
        "the newest equal-rank pass must win regardless of array order"
    );
    let (verdict, _, _) = bot_verdict("chatgpt-codex-connector", &[], &comments, &fresh_at);
    assert_eq!(verdict, CoverageVerdict::Reviewed);
}

#[test]
fn bravo_wearing_findings_is_not_a_clean_pass() {
    // `Bravo` must END its sentence: `Bravo, but...` and `Bravo. 2 minor
    // ones...` are findings notes wearing the flourish, and a bare
    // starts_with("bravo") counted both as passes.
    for body in [
            "Codex Review: Didn't find any major issues. Bravo, but two minor ones need attention. Reviewed commit: abc12345",
            "Codex Review: Didn't find any major issues. Bravo. 2 minor ones need attention. Reviewed commit: abc12345",
        ] {
            let comments = vec![serde_json::json!({
                "author": {"login": "chatgpt-codex-connector[bot]"},
                "body": body,
                "createdAt": "2026-08-17T02:00:00Z"
            })];
            let fresh_at = fresh_at_head();
            assert!(
                clean_pass_review(&comments, "chatgpt-codex-connector", &fresh_at).is_none(),
                "counted as a pass: {body}"
            );
            let (verdict, _, _) = bot_verdict("chatgpt-codex-connector", &[], &comments, &fresh_at);
            assert_eq!(verdict, CoverageVerdict::Absent, "verdict for: {body}");
        }
}

#[test]
fn quota_bounce_predating_the_bots_latest_evidence_reads_stale() {
    // A stale review object (01:00) plus an EARLIER bounce (00:00): the
    // bot recovered and reviewed; parking at Refused has no path back,
    // while Stale asks a re-read. The inverse ordering still refuses.
    let fresh_at = fresh_at_head();
    let (verdict, _, _) = bot_verdict(
        "chatgpt-codex-connector",
        &[stale_findings_review()],
        &[usage_comment("2026-08-17T00:00:00Z")],
        &fresh_at,
    );
    assert_eq!(verdict, CoverageVerdict::Stale);
    let (verdict, _, _) = bot_verdict(
        "chatgpt-codex-connector",
        &[stale_findings_review()],
        &[usage_comment("2026-08-17T02:00:00Z")],
        &fresh_at,
    );
    assert_eq!(verdict, CoverageVerdict::Refused);
}

#[test]
fn short_name_config_cannot_draft_a_fan_into_the_marker_lane() {
    // Config may name the bot by a short login ("codex"); containment then
    // matches any human whose login merely contains it. The comment lanes
    // require the AUTHOR to be a characterized bot profile, so the fan's
    // clean-pass marker and quota text are both inert. The review-object
    // lane is unaffected: a review object carries no marker text to
    // counterfeit, and the real bot still passes it.
    let fan = |body: &str| {
        serde_json::json!({
            "author": {"login": "codex-fan"},
            "body": body,
            "createdAt": "2026-08-17T02:00:00Z"
        })
    };
    let fresh_at = fresh_at_head();
    let (verdict, _, _) = bot_verdict(
        "codex",
        &[],
        &[
            fan("Codex Review: Didn't find any major issues. Bravo. Reviewed commit: abc12345"),
            fan("You have reached your Codex usage limits for code reviews"),
        ],
        &fresh_at,
    );
    assert_eq!(verdict, CoverageVerdict::Absent);
    // The real bot under the same short config still reads Reviewed.
    let (verdict, _, _) = bot_verdict("codex", &[], &[clean_pass_comment("abc12345")], &fresh_at);
    assert_eq!(verdict, CoverageVerdict::Reviewed);
}
