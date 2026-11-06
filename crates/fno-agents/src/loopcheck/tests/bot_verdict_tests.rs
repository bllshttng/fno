use super::*;

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
