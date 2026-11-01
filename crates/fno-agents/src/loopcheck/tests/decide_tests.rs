use super::*;

#[test]
fn fire_budget_clamp_stays_under_remaining_and_above_the_floor() {
    let s = std::time::Duration::from_secs(30);
    // Budget-rich: the configured ceiling stands.
    assert_eq!(
        clamp_to_fire_budget(s, s + std::time::Duration::from_secs(1)),
        s
    );
    // Budget-poor: what remains stands.
    assert_eq!(
        clamp_to_fire_budget(s, std::time::Duration::from_millis(500)),
        std::time::Duration::from_millis(500)
    );
    // Budget spent: every read still gets a positive, killable bound.
    assert_eq!(
        clamp_to_fire_budget(s, std::time::Duration::ZERO),
        STOPGATE_BOUND_FLOOR
    );
}

#[test]
fn freshness_shrunk_diff_carries_as_subset() {
    // The specimen shape: one doc sentence reworded, one test-file hunk
    // gone because main absorbed it. Every raw line still shipping was
    // read, so the shrunk diff carries instead of costing a re-review.
    let f = facts_lines(
        &[
            ":100644 100644 aaa bbb M\tcli/src/a.py",
            ":100644 100644 ccc ddd M\tcli/tests/test_a.py",
        ],
        &[":100644 100644 aaa bbb M\tcli/src/a.py"],
        Some(&["docs/x.md"]),
    );
    let verdict = review_freshness("r1", "h1", &f);
    assert_eq!(verdict, Freshness::CarriedSubset);
    assert!(verdict.counts());
}

#[test]
fn freshness_subset_does_not_swallow_equal_or_added_lines() {
    // Equal sets are not a strict subset (they hash equal and take the
    // tree-paths branch instead), and a HEAD line the reviewer never saw
    // is new unreviewed code: Stale.
    assert_eq!(
        review_freshness(
            "r",
            "h",
            &facts_lines(&["l1", "l2"], &["l1", "l2"], Some(&[]))
        ),
        Freshness::CarriedBaseSync
    );
    assert_eq!(
        review_freshness("r", "h", &facts_lines(&["l1"], &["l1", "l2"], None)),
        Freshness::Stale
    );
}

#[test]
fn freshness_absent_identity_stays_stale() {
    // An identity that failed to compute (None, which is also what an
    // empty code diff yields) can never carry - absence is not evidence.
    assert_eq!(
        review_freshness("r", "h", &facts(None, Some("i"), None)),
        Freshness::Stale
    );
    assert_eq!(
        review_freshness("r", "h", &facts(Some("i"), None, None)),
        Freshness::Stale
    );
}

#[test]
fn a_zero_file_pass_does_not_satisfy_the_reviewers_gate() {
    // The reviewers gate (config.review.reviewers, the self-review floor)
    // reads a separate scan; a review of nothing must not satisfy it
    // there either, only on the coverage axis.
    let zero = serde_json::json!({
        "ts": "2026-01-01T00:00:00Z", "source": "test",
        "type": "review_attestation",
        "data": {"reviewer": "code-review", "head_sha": "h", "verdict": "pass",
                 "attester_session_id": "sess-author",
                 "reviewed_line_count": 0, "reviewed_file_count": 0}
    })
    .to_string();
    let dir = std::env::temp_dir().join(format!(
        "fno-zero-file-scan-{}-{}",
        std::process::id(),
        line!()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let events = dir.join("events.jsonl");
    std::fs::write(&events, zero).unwrap();
    let reviewers = vec!["code-review".to_string()];
    let (unattested, _malformed) =
        unattested_reviewers_scan(&events, &reviewers, &|_| Freshness::Fresh, "", "h", false);
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        !unattested.is_empty(),
        "a zero-file pass must leave the reviewer unattested"
    );
    assert_eq!(unattested[0].name, "code-review");
}

#[test]
fn required_staleness_still_blocks_local_recovery() {
    // Unchanged, pinned: a stale REQUIRED verdict is one re-read from
    // counting, so recovery must not skip past it.
    let events = attestation_line_on_branch("code-review", "h", "pass", "feature/x");
    let rep = classify_coverage(
        &[],
        &[],
        &events,
        &[],
        true,
        Some("sess-author"),
        &|_| Freshness::Fresh,
        "feature/x",
        "h",
    );
    let refused = vec!["some-required-bot".to_string()];
    let stale = vec![("some-required-bot".to_string(), "oldsha".to_string())];
    assert!(!local_recovery_from_refusal(&refused, &[], &stale, &rep));
    // And no refusal means no recovery at all: absence is a wait.
    assert!(!local_recovery_from_refusal(&[], &[], &[], &rep));
}

#[test]
fn required_reviewer_gate_honors_the_same_carry() {
    // The N-reachable-paths check. `config.review.reviewers` is satisfied
    // by a DIFFERENT scan than the coverage count, so a carry granted to
    // one and refused by the other leaves the gate exactly as tight as
    // before and the softening purely decorative.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("events.jsonl");
    std::fs::write(
        &p,
        attestation_line_on_branch("code-review", "oldhead", "pass", "feature/x") + "\n",
    )
    .unwrap();
    let reviewers = vec!["code-review".to_string()];

    let carried = unattested_reviewers_scan(
        &p,
        &reviewers,
        &|_| Freshness::CarriedBaseSync,
        "feature/x",
        "currenthead",
        false,
    )
    .0;
    assert!(
        carried.is_empty(),
        "a carried attestation must satisfy the gate"
    );

    let stale = unattested_reviewers_scan(
        &p,
        &reviewers,
        &|_| Freshness::Stale,
        "feature/x",
        "currenthead",
        false,
    )
    .0;
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].superseded_head.as_deref(), Some("oldhead"));
}

#[test]
fn unknown_coverage_names_the_read_remedy_not_a_ci_failure() {
    let mut pr = watch_pr();
    pr.ci_conclusion = CiConclusion::Success;
    pr.ci_has_pending = false;
    pr.coverage = CoverageReport {
        fail_at_head: false,
        github_approval_satisfies: false,
        coverage: Coverage::Unknown,
        verdicts: vec![],
    };
    let reason = build_block_reason(&pr, "abc", true, true);
    assert!(
        reason.contains("coverage read unavailable"),
        "got: {reason}"
    );
    assert!(reason.contains("retry the review command"), "got: {reason}");
    assert!(!reason.contains("CI red"), "got: {reason}");
    assert!(!reason.contains("failed"), "got: {reason}");
    assert!(!reason.contains("Read the failing log"), "got: {reason}");
    assert!(!reason.contains("fno do pr logs"), "got: {reason}");
}

#[test]
fn unwatched_async_nudge_ci_pending_teaches_arm_and_tag() {
    // AC3-HP: the CI-pending block message must instruct arming a
    // harness-tracked watcher with a timeout and emitting <watching>,
    // replacing the old "wait silently" prose.
    let pr = PrInfo {
        range_tiling: RangeTiling::default(),
        ci_conclusion: CiConclusion::Pending,
        ci_has_pending: true,
        ..watch_pr()
    };
    let reason = build_block_reason(&pr, "abc", true, true);
    assert!(reason.contains("<watching"), "got: {reason}");
    assert!(reason.contains("timeout"), "got: {reason}");
    // The taught watcher is the sanctioned REST wait command, never the
    // GraphQL `gh pr checks --watch` and never an inline loop (which a
    // worktree session's Bash isolation refuses as too complex).
    assert!(reason.contains("fno do pr wait"), "got: {reason}");
    assert!(!reason.contains("gh pr checks"), "got: {reason}");
    assert!(!reason.contains("while ["), "got: {reason}");
    assert!(!reason.contains("wait silently"), "got: {reason}");
}

/// the terminal fires only when the quota bounce is the SOLE unmet
/// conjunct of `reviewed`. Each case drops one other conjunct and must
/// block; reverting `awaiting_review_only` to the bare
/// `!usage_limited.is_empty()` check fails them.
#[test]
fn awaiting_review_only_requires_every_other_conjunct() {
    let bounced = || {
        let mut pr = watch_pr();
        pr.coverage = CoverageReport {
            fail_at_head: false,
            github_approval_satisfies: false,
            coverage: Coverage::Covered(0),
            verdicts: vec![ReviewerVerdict {
                producer: CoverageProducer::GithubApp,
                name: "chatgpt-codex-connector".to_string(),
                verdict: CoverageVerdict::Refused,
                human_approval: false,
                author_approval: false,
                attestation_origin: AttestationOrigin::Unknown,
                reviewed_sha: String::new(),
                freshness: None,
                scope: None,
                refusal_reason: None,
                reviewer_context: None,
                attest_machine: None,
                required: true,
                passed: false,
            }],
        };
        pr
    };
    assert!(awaiting_review_only(&bounced()), "the terminal's own case");
    assert!(!awaiting_review_only(&watch_pr()), "no bounce to report");

    // A bot that has not reviewed YET is owed its nudge window: one bot's
    // quota state must not end the session on the others' behalf.
    let mut still_pending = bounced();
    still_pending.missing_bots = vec!["gemini-code-assist".to_string()];
    assert!(
        !awaiting_review_only(&still_pending),
        "bot still owed a wait"
    );

    // A stale bot owes a re-read: the same window a missing bot
    // gets, never a clean exit on another bot's quota bounce.
    let mut stale_pending = bounced();
    stale_pending.stale_bots = vec![("gemini-code-assist".to_string(), "00001111".to_string())];
    assert!(
        !awaiting_review_only(&stale_pending),
        "stale bot still owed a re-read"
    );

    // A standing blocking finding is work the agent must DO; parking hands
    // a human a PR carrying an unaddressed P1.
    let mut with_finding = bounced();
    with_finding.unaddressed_findings = vec![Finding {
        id: 1,
        author: "gemini-code-assist".to_string(),
        path: "src/lib.rs".to_string(),
        line: 12,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        severity: "P1",
        had_reply: true,
    }];
    assert!(!awaiting_review_only(&with_finding), "unaddressed P1");

    let mut unattested = bounced();
    unattested.unattested_reviewers = vec![UnattestedReviewer {
        name: "sigma".to_string(),
        superseded_head: None,
        failed_at_head: false,
    }];
    assert!(!awaiting_review_only(&unattested), "local review never ran");
}

#[test]
fn watch_expiry_gate_allows_daemon_routed_codex_idle() {
    // Supported interactive harnesses share the daemon expiry arm.
    assert!(harness_can_idle(Some("claude"), false));
    assert!(harness_can_idle(Some("codex"), false));
    assert!(harness_can_idle(Some("opencode"), false));
    assert!(harness_can_idle(Some("pi"), false));
    assert!(harness_can_idle(Some("agy"), false));
    assert!(harness_can_idle(Some("gemini"), false));
    // A loop-run child (FNO_DRIVER_LIB) exits on allow -> never idles.
    assert!(!harness_can_idle(Some("claude"), true));
    assert!(!harness_can_idle(Some("codex"), true));
    assert!(!harness_can_idle(Some("opencode"), true));
    // Unknown/unroutable harnesses keep the conservative block behavior.
    assert!(!harness_can_idle(Some("unknown"), false));
    // Unknown harness (bare shell / daemon): conservative block.
    assert!(!harness_can_idle(None, false));
}

#[test]
fn watching_refusal_names_the_disqualifying_substrate() {
    assert_eq!(
        watch_lease::watching_harness_refusal(Some("claude"), true),
        "watching ignored: loop-run child cannot idle"
    );
    // A no-self-wake harness is told to stop, never to arm a watcher it
    // cannot run: the old Claude-only ritual text is the loop the codex
    // thread worker burned 13 turns in.
    let text = watch_lease::watching_harness_refusal(Some("grok"), false);
    assert!(text.contains("harness grok cannot idle"), "{text}");
    assert!(text.contains("Do not arm a"), "{text}");
    assert!(text.contains("stop instead"), "{text}");
    assert!(!text.contains("Arm a harness-tracked watcher"), "{text}");
}

#[test]
fn watch_idle_rejects_ci_pending_with_a_failure() {
    // The plain pending-ci positive arm of the same classifier line the
    // failure shape refuses below (folded from watch_idle_classifies_pending_ci).
    assert_eq!(async_wait_class(&watch_pr(), true, true), Some("ci"));
    // gemini finding: a check has ALREADY concluded red while others run.
    // The agent should debug now, not idle out the remaining pending checks.
    let pr = PrInfo {
        range_tiling: RangeTiling::default(),
        ci_conclusion: CiConclusion::Failure(Some("unit".into())),
        ci_has_pending: true,
        ..watch_pr()
    };
    assert_eq!(async_wait_class(&pr, true, true), None);
}

#[test]
fn nudge_needs_nudge_blocks_and_names_the_command() {
    // AC1: not idlable; reason gives the exact gh command; no arm-and-tag hint.
    let pr = bot_review_pr(
        "chatgpt-codex-connector",
        vec![bn(
            "chatgpt-codex-connector",
            NudgeClass::NeedsNudge,
            0,
            0,
            0,
        )],
    );
    assert_eq!(async_wait_class(&pr, true, true), None);
    let reason = build_block_reason(&pr, "abc", true, true);
    assert!(
        reason.contains("gh pr comment 618 --body \"@codex review\""),
        "{reason}"
    );
    assert!(
        !reason.contains("harness-tracked watcher"),
        "no arm hint: {reason}"
    );
}

#[test]
fn nudge_unresponsive_blocks_and_names_optional_apps() {
    // AC3: not idlable; names the give-up + optional_apps; no arm-and-tag hint.
    let pr = bot_review_pr(
        "chatgpt-codex-connector",
        vec![bn(
            "chatgpt-codex-connector",
            NudgeClass::Unresponsive,
            3,
            20,
            47,
        )],
    );
    assert_eq!(async_wait_class(&pr, true, true), None);
    let reason = build_block_reason(&pr, "abc", true, true);
    assert!(
        reason.contains("did not review after 3 nudges over 47m"),
        "{reason}"
    );
    assert!(reason.contains("config.review.optional_apps"), "{reason}");
    assert!(reason.contains("do not arm a watcher"), "{reason}");
    assert!(
        !reason.contains("harness-tracked watcher"),
        "no arm hint: {reason}"
    );
}

#[test]
fn block_reason_local_work_outranks_a_bot_wait() {
    // Codex review of this PR: with a bot AND a local reviewer both
    // outstanding, naming only the bot hides the half the session can act
    // on now. Worse, if the bot never posts, the local work never happens
    // and the run dies on budget with the gate still unmet - the #618 shape
    // this node exists to delete.
    let pr = PrInfo {
        range_tiling: RangeTiling::default(),
        missing_bots: vec!["chatgpt-codex-connector".into()],
        bot_nudges: vec![],
        ..reviewers_gate_pr()
    };
    let reason = build_block_reason(&pr, "abc", true, true);
    assert!(reason.contains("reviewers gate unmet"), "got: {reason}");
    assert!(!reason.contains("<watching"), "got: {reason}");
    // Once the local half is attested, the bot wait is the sole blocker and
    // the arm-and-tag message returns.
    let after = PrInfo {
        range_tiling: RangeTiling::default(),
        unattested_reviewers: vec![],
        ..pr
    };
    assert!(build_block_reason(&after, "abc", true, true).contains("<watching"));
}

#[test]
fn reviewers_gate_stays_fail_closed() {
    // AC7: promoting the predicate's return value to a list must not move
    // the gate. Missing file, missing event, stale head, and a `fail`
    // verdict all still leave the reviewer unsatisfied.
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("absent.jsonl");
    let sigma = vec!["sigma".to_string()];
    assert!(!unattested_reviewers(&missing, &sigma, "h", "").is_empty());

    let stale = tmp.path().join("stale.jsonl");
    std::fs::write(&stale, format!("{}\n", r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"review_attestation","data":{"reviewer":"sigma","head_sha":"OLD","verdict":"pass","branch":"feature/x"}}"#)).unwrap();
    let out = unattested_reviewers(&stale, &sigma, "NEW", "feature/x");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].superseded_head.as_deref(), Some("OLD"));
    assert!(!out[0].failed_at_head);

    let failed = tmp.path().join("fail.jsonl");
    std::fs::write(&failed, format!("{}\n", r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"review_attestation","data":{"reviewer":"sigma","head_sha":"h","verdict":"fail"}}"#)).unwrap();
    let out = unattested_reviewers(&failed, &sigma, "h", "");
    assert_eq!(out.len(), 1);
    // A head-pinned fail is not a superseded pass; do not offer a stale head.
    assert_eq!(out[0].superseded_head, None);
    // ...but it IS an attestation at this head, and the message says so
    // rather than claiming none exists. Pinned at the PARSER: the message
    // test hand-builds the struct and never exercises this derivation, so
    // `failed_at_head: false` survived the whole suite before this line.
    assert!(
        out[0].failed_at_head,
        "a fail at HEAD must be reported as such"
    );
}

#[test]
fn unpinned_attestation_never_counts_as_evidence() {
    // codex P1 on this PR: defaulting a missing head_sha to "" made an
    // unpinned event MATCH a caller whose own head_sha is "", turning
    // no-evidence into a pass.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("e.jsonl");
    std::fs::write(
            &p,
            format!(
                "{}\n",
                r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"review_attestation","data":{"reviewer":"sigma","verdict":"pass"}}"#
            ),
        )
        .unwrap();
    let out = unattested_reviewers(&p, &["sigma".to_string()], "", "");
    assert_eq!(out.len(), 1, "unpinned evidence must not satisfy the gate");
    assert_eq!(out[0].superseded_head, None);
}

#[test]
fn a_corrupt_attestation_line_is_counted_and_named() {
    // A torn write leaves an unparseable review_attestation in the file.
    // The gate must still fail closed, but reporting "no head-pinned
    // review_attestation" over a corrupt one is the same class of lie this
    // node deletes. The sibling review_finding scanner already counts its
    // malformed lines; this one did not.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("e1.jsonl");
    std::fs::write(
            &p,
            concat!(
                r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"review_attestation","data":{"reviewer":"sigma","hea"#,
                "\n",
                r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"loop_check","data":{}}"#,
            ),
        )
        .unwrap();
    let (out, malformed) = unattested_reviewers_scan(
        &p,
        &["sigma".to_string()],
        &sha_equality_freshness("h"),
        "",
        "h",
        false,
    );
    assert_eq!(out.len(), 1, "a corrupt line never satisfies the gate");
    assert_eq!(malformed, 1, "and it is counted, not silently dropped");

    let pr = PrInfo {
        range_tiling: RangeTiling::default(),
        malformed_attestations: malformed,
        ..reviewers_gate_pr()
    };
    let reason = build_block_reason(&pr, "abc", true, true);
    assert!(
        reason.contains("unparseable attestation line"),
        "got: {reason}"
    );

    // A clean file adds nothing to the message.
    let p = tmp.path().join("e2.jsonl");
    std::fs::write(
        &p,
        r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"loop_check","data":{}}"#,
    )
    .unwrap();
    assert_eq!(
        unattested_reviewers_scan(
            &p,
            &["sigma".to_string()],
            &sha_equality_freshness("h"),
            "",
            "h",
            false
        )
        .1,
        0
    );
    assert!(!build_block_reason(&reviewers_gate_pr(), "abc", true, true)
        .contains("unparseable attestation line"));
}

#[test]
fn short_sha_never_panics_on_multibyte() {
    // codex P2: `&s[..8]` panics when byte 8 lands inside a character, and
    // superseded_head comes from a user-writable events.jsonl.
    assert_eq!(short_sha("0123456789ab"), "01234567");
    assert_eq!(short_sha("abc"), "abc");
    assert_eq!(short_sha(""), "");
    assert_eq!(short_sha("1234567\u{e9}xyz"), "1234567\u{e9}");
    let pr = PrInfo {
        range_tiling: RangeTiling::default(),
        unattested_reviewers: vec![UnattestedReviewer {
            name: "sigma".to_string(),
            superseded_head: Some("1234567\u{e9}abc".to_string()),
            failed_at_head: false,
        }],
        ..reviewers_gate_pr()
    };
    build_block_reason(&pr, "1234567\u{e9}abc", true, true);
}

#[test]
fn watcher_hint_never_contradicts_the_idle_classifier() {
    // codex P1: the missing-bot branch emitted the ritual unconditionally,
    // including for states async_wait_class refuses to idle (an unaddressed
    // finding, or an open operator finding). The hint is now derived from
    // that same classifier, so the two agree by construction.
    let bot_only = PrInfo {
        range_tiling: RangeTiling::default(),
        missing_bots: vec!["chatgpt-codex-connector".into()],
        bot_nudges: vec![],
        unattested_reviewers: vec![],
        ..reviewers_gate_pr()
    };
    for (label, pr, open_empty) in [
        // Reaches the FINDINGS branch, not the bot branch: unaddressed
        // findings render first. Kept because the invariant under test is
        // "no hint for a non-idlable state", which holds branch-wide - but
        // it is the case below that reaches `missing_bots`, so that one is
        // what would catch a reintroduced unconditional `arm_watch_hint`
        // there. A sigma round caught this test silently losing its teeth
        // when the branch order moved out from under it.
        (
            "bot + unaddressed finding (renders as the finding)",
            PrInfo {
                range_tiling: RangeTiling::default(),
                missing_bots: vec!["chatgpt-codex-connector".into()],
                bot_nudges: vec![],
                unattested_reviewers: vec![],
                unaddressed_findings: vec![Finding {
                    id: 1,
                    author: "codex".into(),
                    path: "a.rs".into(),
                    line: 1,
                    created_at: "2026-07-27T00:00:00Z".into(),
                    severity: "P1",
                    had_reply: true,
                }],
                ..reviewers_gate_pr()
            },
            true,
        ),
        (
            // The one that DOES reach `missing_bots` while non-idlable.
            "bot + open operator finding",
            PrInfo {
                range_tiling: RangeTiling::default(),
                missing_bots: vec!["chatgpt-codex-connector".into()],
                bot_nudges: vec![],
                unattested_reviewers: vec![],
                ..reviewers_gate_pr()
            },
            false,
        ),
    ] {
        let reason = build_block_reason(&pr, "abc", open_empty, true);
        assert_eq!(async_wait_class(&pr, open_empty, true), None, "{label}");
        assert!(!reason.contains("<watching"), "{label}: {reason}");
    }
    // The genuinely idlable state keeps the ritual.
    assert_eq!(async_wait_class(&bot_only, true, true), Some("review"));
    assert!(build_block_reason(&bot_only, "abc", true, true).contains("<watching"));
}

#[test]
fn unaddressed_finding_with_no_reply_names_the_top_level_blind_spot() {
    // A finding answered with a top-level PR comment reads as unaddressed
    // because the gate walks in_reply_to_id chains only. The block reason
    // must name the mechanism and the exact gh command, not the ambiguous
    // "reply in-thread" a worker who posted a top-level comment reads as
    // "I did reply" (PR #447, #787 both stalled green PRs this way).
    let pr = PrInfo {
        range_tiling: RangeTiling::default(),
        unaddressed_findings: vec![Finding {
            id: 1,
            author: "codex".into(),
            path: "a.rs".into(),
            line: 7,
            created_at: "2026-07-27T00:00:00Z".into(),
            severity: "P1",
            had_reply: false,
        }],
        ..reviewers_gate_pr()
    };
    let reason = build_block_reason(&pr, "abc", true, true);
    assert!(reason.contains("no in-thread reply"), "got: {reason}");
    assert!(reason.contains("in_reply_to_id"), "got: {reason}");
    assert!(reason.contains("top-level PR comment"), "got: {reason}");
    assert!(reason.contains("in_reply_to=<id>"), "got: {reason}");
}

#[test]
fn block_reason_stops_demanding_a_push_for_a_shipped_head() {
    // The wedge, end to end. Positive marker AND absence: a panicking
    // renderer would also fail to print the push message.
    let pr = shipped_pr("23480a0e", PrState::Merged);
    let reason = build_block_reason(&pr, "fe407c3b", true, true);
    assert!(
        !reason.contains("push the latest commits"),
        "shipped head must not be told to push: {reason}"
    );
    assert!(!reason.is_empty(), "a reason is still rendered: {reason}");
}

#[test]
fn block_reason_still_demands_a_push_for_unshipped_work() {
    // The half that proves the fix did not just delete the guard.
    let pr = shipped_pr("23480a0e", PrState::Open);
    let reason = build_block_reason(&pr, "deadbeef", true, false);
    assert!(
        reason.contains("push the latest commits before completing"),
        "unpushed work must still block: {reason}"
    );
}

#[test]
fn unwatched_async_nudge_review_uses_review_aware_watcher() {
    // codex P2: the review-wait watcher must poll REVIEW state, not
    // checks. It must also poll on REST - the GraphQL
    // reviews read is part of what exhausts the shared quota. The recipes
    // are the sanctioned `fno do pr wait` command, one plain command: an
    // inline `while`/`$(...)` loop is refused by Claude Code's worktree
    // Bash isolation, so a worktree session cannot arm the watcher at all.
    let hint = arm_watch_hint(404, "review");
    assert!(
        hint.contains("fno do pr wait 404 --until review"),
        "got: {hint}"
    );
    assert!(!hint.contains("gh pr view"), "got: {hint}");
    assert!(!hint.contains("while ["), "got: {hint}");
    assert!(!hint.contains("$("), "got: {hint}");
    // The CI-wait watcher polls the REST status chokepoint for the
    // POSITIVE settled marker, never `gh pr checks --watch` (GraphQL).
    let ci_hint = arm_watch_hint(404, "ci");
    assert!(
        ci_hint.contains("fno do pr wait 404 --until settled"),
        "got: {ci_hint}"
    );
    assert!(!ci_hint.contains("gh pr checks"), "got: {ci_hint}");
    assert!(!ci_hint.contains("while ["), "got: {ci_hint}");
    assert!(!ci_hint.contains("$("), "got: {ci_hint}");
}

#[test]
fn watch_idle_window_defaults_clamps_and_slacks() {
    // Default (no tag timeout): 15m + 12m slack.
    assert_eq!(
        watch_window_ms(None),
        15 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // Honored within range.
    assert_eq!(
        watch_window_ms(Some("10m")),
        10 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // Above the 15m ceiling clamps down: the ci-push-cadence law wakes a
    // parked session every 15 minutes, so a 30m tag reads as 15m.
    assert_eq!(
        watch_window_ms(Some("30m")),
        15 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // Below the 5m floor clamps up.
    assert_eq!(
        watch_window_ms(Some("1m")),
        5 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // The same ceiling holds for hours-long tags.
    assert_eq!(
        watch_window_ms(Some("5h")),
        15 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // Garbage falls back to the default.
    assert_eq!(
        watch_window_ms(Some("soon")),
        15 * 60_000 + watch_lease::WATCH_SLACK_MS
    );
    // The event deadline uses the normalized watch duration, not claim slack.
    assert_eq!(watch_lease::watch_expiry_ms(Some("30m"), 1_000), 901_000);
    assert_eq!(watch_lease::watch_expiry_ms(Some("1m"), 1_000), 301_000);
}

#[test]
fn watch_idle_event_is_non_terminal_allow() {
    // AC1-HP invariant: the idle branch emits allow + null termination, so
    // the stop-hook shim (which runs finalize only on a NON-null
    // termination_reason) never invokes finalize / stamps the ledger /
    // graduates a plan on an idle fire. This is the exact output shape the
    // idle branch returns.
    let json = allow_output(
        "allow",
        None,
        "watching: idling until watcher fires (PR #404, ci pending)",
        3,
        Some("sha|OPEN|PENDING|none".to_string()),
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["decision"], "allow");
    assert!(
        v["termination_reason"].is_null(),
        "idle-allow MUST be non-terminal or finalize would run"
    );
    assert!(v["message"].as_str().unwrap().contains("watching"));
}

#[test]
fn termination_reason_variant_names_byte_identical() {
    // Fix 6: all TerminationReason variants must serialize to the exact strings
    // the spec names - no rename attributes applied.
    let cases = [
        (TerminationReason::DonePRGreen, "DonePRGreen"),
        (TerminationReason::DoneAdvisory, "DoneAdvisory"),
        (TerminationReason::DoneAwaitingReview, "DoneAwaitingReview"),
        (TerminationReason::NoWork, "NoWork"),
        (TerminationReason::Budget, "Budget"),
        (TerminationReason::NoProgress, "NoProgress"),
        (TerminationReason::Interrupted, "Interrupted"),
        (TerminationReason::Aborted, "Aborted"),
    ];
    for (variant, expected) in cases {
        let json = serde_json::to_string(&variant).unwrap();
        // serde serializes enum unit variants as "\"VariantName\""
        assert_eq!(
            json,
            format!("\"{expected}\""),
            "variant {expected} serialized incorrectly"
        );
    }
}

#[test]
fn reviewers_all_attested_fail_and_missing_are_unsatisfied() {
    let tmp = tempfile::tempdir().unwrap();
    // fail verdict -> unsatisfied
    let fail = write_events(
        tmp.path(),
        &[
            r#"{"ts":"t","type":"review_attestation","source":"target","data":{"reviewer":"sigma","head_sha":"h","verdict":"fail"}}"#,
        ],
    );
    assert!(!reviewers_all_attested(&fail, &["sigma".to_string()], "h"));
    // missing file -> fail closed
    let gone = tmp.path().join("gone.jsonl");
    assert!(!reviewers_all_attested(&gone, &["sigma".to_string()], "h"));
}

#[test]
fn reviewers_all_attested_conjunction_and_slash_normalized() {
    // Every reviewer must be attested (strict conjunction); a '/'-prefixed
    // config entry matches an event that emits the bare name and vice-versa.
    let tmp = tempfile::tempdir().unwrap();
    let p = write_events(
        tmp.path(),
        &[
            r#"{"ts":"t","type":"review_attestation","source":"target","data":{"reviewer":"sigma","head_sha":"h","verdict":"pass"}}"#,
            r#"{"ts":"t","type":"review_attestation","source":"target","data":{"reviewer":"code-review","head_sha":"h","verdict":"pass"}}"#,
        ],
    );
    // Both present -> satisfied ('/code-review' config vs 'code-review' event).
    assert!(reviewers_all_attested(
        &p,
        &["sigma".to_string(), "/code-review".to_string()],
        "h"
    ));
    // One missing -> unsatisfied.
    assert!(!reviewers_all_attested(
        &p,
        &["sigma".to_string(), "declare".to_string()],
        "h"
    ));
}

#[test]
fn local_peer_attestation_is_head_pinned() {
    let td = tempfile::tempdir().unwrap();
    let events = td.path().join("events.jsonl");
    std::fs::write(&events, format!("{}\n", r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"review_attestation","data":{"reviewer":"peer","head_sha":"OLD","verdict":"pass"}}"#)).unwrap();
    let peer = vec![LOCAL_PEER_REVIEWER.to_string()];
    assert!(!reviewers_all_attested(&events, &peer, "NEW"));
    std::fs::write(&events, format!("{}\n", r#"{"ts":"2026-01-01T00:00:00Z","source":"test","type":"review_attestation","data":{"reviewer":"peer","head_sha":"NEW","verdict":"pass"}}"#)).unwrap();
    assert!(reviewers_all_attested(&events, &peer, "NEW"));
}

#[test]
fn optional_only_refusal_is_not_awaiting_review() {
    // The stop gate must not end the session DoneAwaitingReview over a
    // refusal nobody was owed: on a repo with no required bots that
    // terminal was trivially reachable, and the message told the worker
    // to wait for a reviewer that will never come. The gate blocks, so
    // the unattested local reviewer reads as the real work it is.
    let owed = |required| ReviewerVerdict {
        producer: CoverageProducer::GithubApp,
        name: "chatgpt-codex-connector".to_string(),
        verdict: CoverageVerdict::Refused,
        human_approval: false,
        author_approval: false,
        attestation_origin: AttestationOrigin::Unknown,
        reviewed_sha: String::new(),
        freshness: None,
        scope: None,
        refusal_reason: None,
        reviewer_context: None,
        attest_machine: None,
        required,
        passed: false,
    };
    let mut pr = watch_pr();
    pr.coverage = CoverageReport {
        fail_at_head: false,
        github_approval_satisfies: false,
        coverage: Coverage::Covered(0),
        verdicts: vec![owed(false)],
    };
    assert!(!awaiting_review_only(&pr));
    // The same refusal OWED is still the terminal's case - the pre-change
    // semantics, kept: a required bot declined and nothing else is unmet.
    pr.coverage.verdicts[0].required = true;
    assert!(awaiting_review_only(&pr));
}
