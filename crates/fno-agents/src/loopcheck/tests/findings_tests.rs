use super::*;

#[test]
fn blocking_severity_codex_p1_both_forms() {
    // The exact markup codex emits (pinned from PR #447).
    assert_eq!(
        blocking_severity("![P1 Badge](https://img.shields.io/badge/P1-orange?style=flat) Bug"),
        Some("P1")
    );
    // Alt-text only and URL only each match.
    assert_eq!(blocking_severity("![P1 Badge] something"), Some("P1"));
    assert_eq!(
        blocking_severity("see https://img.shields.io/badge/P1-orange"),
        Some("P1")
    );
}

/// AC2-ERR core: a P1 with no reply is unaddressed.
#[test]
fn finding_no_reply_is_unaddressed() {
    let comments = vec![finding_comment(
        100,
        "![P1 Badge](https://img.shields.io/badge/P1-orange) bug",
        "2026-06-05T01:10:00Z",
    )];
    let (ts, unaddressed) = compute_unaddressed_findings(&comments, &[], &req_vec(), &[]);
    assert_eq!(ts, "2026-06-05T01:10:00Z");
    assert_eq!(unaddressed.len(), 1);
    assert_eq!(unaddressed[0].path, "src/x.rs");
    assert_eq!(unaddressed[0].line, 42);
    assert_eq!(unaddressed[0].severity, "P1");
}

/// AC2-HP commit arm: non-bot reply + commit after the finding -> addressed.
#[test]
fn finding_reply_plus_commit_after_is_addressed() {
    let comments = vec![
        finding_comment(
            100,
            "![P1 Badge](https://img.shields.io/badge/P1-orange) bug",
            "2026-06-05T01:10:00Z",
        ),
        reply_comment(
            101,
            100,
            "bllshttng",
            "fixed in abc123",
            "2026-06-05T01:20:00Z",
        ),
    ];
    let commits = vec!["2026-06-05T01:30:00Z".to_string()];
    let (_, unaddressed) = compute_unaddressed_findings(&comments, &commits, &req_vec(), &[]);
    assert!(unaddressed.is_empty(), "commit-after arm must address");
}

/// AC2-FR wontfix arm: non-bot reply carrying wontfix:, NO commit after.
#[test]
fn finding_wontfix_reply_is_addressed_without_commit() {
    let comments = vec![
        finding_comment(
            100,
            "![P1 Badge](https://img.shields.io/badge/P1-orange) bug",
            "2026-06-05T01:10:00Z",
        ),
        reply_comment(
            101,
            100,
            "bllshttng",
            "wontfix: intentional - documented tradeoff",
            "2026-06-05T01:20:00Z",
        ),
    ];
    // Only commit predates the finding -> commit arm unsatisfied.
    let commits = vec!["2026-06-05T01:00:00Z".to_string()];
    let (_, unaddressed) = compute_unaddressed_findings(&comments, &commits, &req_vec(), &[]);
    assert!(unaddressed.is_empty(), "wontfix arm must address alone");
}

/// Anti-gaming: a commit alone (no reply) does NOT address (locked
/// decision 3 - any unrelated commit would silently clear a P1).
#[test]
fn finding_commit_without_reply_is_unaddressed() {
    let comments = vec![finding_comment(
        100,
        "![P1 Badge](https://img.shields.io/badge/P1-orange) bug",
        "2026-06-05T01:10:00Z",
    )];
    let commits = vec!["2026-06-05T01:30:00Z".to_string()];
    let (_, unaddressed) = compute_unaddressed_findings(&comments, &commits, &req_vec(), &[]);
    assert_eq!(unaddressed.len(), 1, "commit alone must not address");
}

/// sigma-review: a blocking finding row with a missing id is SKIPPED
/// (under-block per locked decision 4), never pooled on a default id
/// where one stray reply could clear multiple findings.
#[test]
fn finding_missing_id_skipped_not_pooled() {
    let no_id = serde_json::json!({
        "in_reply_to_id": null,
        "user": {"login": "chatgpt-codex-connector[bot]"},
        "body": "![P1 Badge](https://img.shields.io/badge/P1-orange) idless",
        "path": "src/z.rs", "line": 3,
        "created_at": "2026-06-05T01:05:00Z"
    });
    let real = finding_comment(
        100,
        "![P1 Badge](https://img.shields.io/badge/P1-orange) real",
        "2026-06-05T01:10:00Z",
    );
    // A stray reply keyed to id 0 must not ack anything.
    let stray = reply_comment(
        101,
        0,
        "bllshttng",
        "wontfix: stray",
        "2026-06-05T01:20:00Z",
    );
    let comments = vec![no_id, real, stray];
    let (_, unaddressed) = compute_unaddressed_findings(&comments, &[], &req_vec(), &[]);
    assert_eq!(unaddressed.len(), 1, "only the real finding remains");
    assert_eq!(unaddressed[0].id, 100);
}

/// sigma-review: commit-after comparison parses timestamps instead of
/// string-comparing - an offset-suffixed commit date that lexicographically
/// sorts above a Zulu finding date but is EARLIER in UTC must not clear
/// the finding.
#[test]
fn ts_after_parses_offsets_correctly() {
    // 23:30+13:00 == 10:30Z, which is BEFORE 11:00Z - but the raw string
    // "2026-06-05T23:30:00+13:00" > "2026-06-05T11:00:00Z".
    assert!(!ts_after(
        "2026-06-05T23:30:00+13:00",
        "2026-06-05T11:00:00Z"
    ));
    // POSITIVE direction proves chrono's FromStr for DateTime<Utc>
    // parses offset-suffixed RFC3339 and converts to UTC (gemini's
    // #448 critical claimed it errors; empirically it returns
    // Ok(2026-06-05T13:30:00Z) here). Without this assertion the
    // offset case above could pass vacuously via the Err arm.
    assert!(ts_after(
        "2026-06-05T23:30:00+10:00", // == 13:30Z
        "2026-06-05T11:00:00Z"
    ));
    assert!(ts_after("2026-06-05T11:00:01Z", "2026-06-05T11:00:00Z"));
    assert!(!ts_after("2026-06-05T11:00:00Z", "2026-06-05T11:00:00Z"));
    // Unparseable on either side never clears a finding.
    assert!(!ts_after("garbage", "2026-06-05T11:00:00Z"));
    assert!(!ts_after("2026-06-05T11:00:00Z", "garbage"));
    assert!(!ts_after("2026-06-05T11:00:00Z", ""));
}
