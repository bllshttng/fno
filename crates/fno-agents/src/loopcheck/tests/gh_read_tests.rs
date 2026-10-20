use super::*;

/// Shared fixture for the head_is_shipped cases: a PR recording `pr_head`.
pub(super) fn shipped_pr(pr_head: &str, state: PrState) -> PrInfo {
    PrInfo {
        range_tiling: RangeTiling::default(),
        head_oid: pr_head.to_string(),
        state,
        ..reviewers_gate_pr()
    }
}

/// A git stub answering the three probes `head_is_shipped` can make: the
/// working-tree status, the base resolution, and `--is-ancestor`.
/// Mirrors the classify_payload stubs above.
fn git_stub(dir: &Path, ancestor: bool, clean: bool) -> std::path::PathBuf {
    let verdict = if ancestor { "exit 0" } else { "exit 1" };
    // A clean tree is empty stdout with exit 0; a dirty one names a file.
    let status = if clean {
        "exit 0"
    } else {
        "printf ' M src/lib.rs\\n'"
    };
    write_exec(
            dir,
            "git",
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n  status*) {status} ;;\n  rev-parse*origin/main*) exit 0 ;;\n  *is-ancestor*) {verdict} ;;\n  *) exit 1 ;;\nesac\n"
            ),
        )
}

#[test]
fn head_is_shipped_accepts_a_head_already_on_the_base() {
    // The 2026-07-30 and 2026-08-22 repros: a merged PR, a local HEAD that
    // differs because the branch moved past its own merge, and nothing left
    // to ship.
    let dir = tempfile::tempdir().unwrap();
    let git = git_stub(dir.path(), true, true);
    let pr = shipped_pr("23480a0e", PrState::Merged);
    // Retry the spawn, for the same measured reason probe_graphql_quota
    // does: this suite forks hundreds of fake `git` subprocesses in
    // parallel, and a loaded runner intermittently fails one fork/exec.
    // This arm reaches git twice (`git_tree_clean` then `git_head_on_base`)
    // and both fail CLOSED, so a blip in either reads as "not shipped" and
    // reds a PR that changed nothing here. Observed doing exactly that.
    //
    // The retry cannot hide a regression: the stub answers clean and
    // ancestor unconditionally, so the only way to get `false` is a failed
    // spawn, and a real regression fails all five attempts.
    //
    // Its sibling `head_is_shipped_still_refuses_a_commit_stacked_on_a_
    // merged_pr` asserts the NEGATIVE, so a blip there passes it for the
    // wrong reason rather than failing. That is a quieter defect and is
    // left alone here; it needs a positive control on the stub, not a retry.
    let mut shipped = false;
    for _ in 0..5 {
        shipped = head_is_shipped(&pr, "fe407c3b", git.to_str().unwrap(), Path::new("."));
        if shipped {
            break;
        }
    }
    assert!(
        shipped,
        "the stub git kept failing to spawn across 5 retries - a real regression, not a blip"
    );
}

/// THE #447 REGRESSION TEST. A change that makes this case pass as shipped
/// re-opens the defect the guard exists for: unpushed work terminating as
/// DonePRGreen without ever shipping. If this assertion is ever inverted to
/// make a wedge go away, the wedge was the wrong thing to fix.
#[test]
fn head_is_shipped_still_refuses_a_commit_stacked_on_a_merged_pr() {
    let dir = tempfile::tempdir().unwrap();
    let git = git_stub(dir.path(), false, true);
    let pr = shipped_pr("23480a0e", PrState::Merged);
    assert!(!head_is_shipped(
        &pr,
        "deadbeef",
        git.to_str().unwrap(),
        Path::new(".")
    ));
}

/// `--is-ancestor` sees committed history only. Without the clean-tree
/// condition, a rebase onto a base that already holds the merge would read
/// as shipped while uncommitted follow-up edits sat in the tree, and the
/// run would terminate DonePRGreen on work nobody had committed. The old
/// equality predicate blocked that state incidentally; this keeps it
/// blocked deliberately.
#[test]
fn head_is_shipped_refuses_a_dirty_tree_on_the_ancestor_arm() {
    let dir = tempfile::tempdir().unwrap();
    let git = git_stub(dir.path(), true, false);
    let pr = shipped_pr("23480a0e", PrState::Merged);
    assert!(!head_is_shipped(
        &pr,
        "fe407c3b",
        git.to_str().unwrap(),
        Path::new(".")
    ));
}

#[test]
fn graphql_exhausted_reason_names_reset_and_rest_lane() {
    // The message must make a session STOP retrying and say where the
    // answer still lives; "retrying next fire" is the advice it replaces.
    let q = GraphqlQuota {
        remaining: 0,
        reset_epoch: Utc::now().timestamp() + 40 * 60 + 5,
        core_remaining: None,
        core_reset_epoch: None,
    };
    let msg = graphql_exhausted_reason(&q);
    assert!(msg.contains("GraphQL quota exhausted"), "got: {msg}");
    assert!(msg.contains("~40m"), "got: {msg}");
    assert!(msg.contains("fno do pr status"), "got: {msg}");
    assert!(!msg.contains("retrying next fire"), "got: {msg}");
}

#[test]
fn probe_graphql_quota_parses_the_graphql_bucket() {
    let tmp = tempfile::tempdir().unwrap();
    let gh = write_exec(
        tmp.path(),
        "gh",
        "#!/bin/sh\n[ \"$1\" = api ] && [ \"$2\" = rate_limit ] && \
             echo '{\"resources\":{\"graphql\":{\"remaining\":0,\"reset\":1750000000},\
             \"core\":{\"remaining\":4980,\"limit\":5000,\"reset\":1750000000}}}' && exit 0\n\
             exit 1\n",
    );
    // Retry the spawn a few times: under a loaded CI runner (this crate's
    // suite forks hundreds of fake `gh`/`git` subprocesses in parallel),
    // `Command::output()` has measured an intermittent fork/exec failure
    // that has nothing to do with the parser under test - probe_graphql_
    // quota's own `.ok()?` already treats that as "unavailable, degrade
    // gracefully" in production, so retrying here absorbs the same
    // transient blip instead of failing the build on an infra hiccup.
    let mut q = None;
    for _ in 0..5 {
        q = probe_graphql_quota(gh.to_str().unwrap(), tmp.path());
        if q.is_some() {
            break;
        }
    }
    let q = q.expect("gh spawn kept failing across 5 retries - a real regression, not a blip");
    assert_eq!(q.remaining, 0);
    assert_eq!(q.reset_epoch, 1750000000);
    // The core bucket from the SAME probe read feeds the secondary-limit
    // classifier; a payload without it must degrade to None, not to 0.
    assert_eq!(q.core_remaining, Some(4980));
}

fn quota_with(graphql_remaining: i64, core_remaining: Option<i64>) -> GraphqlQuota {
    GraphqlQuota {
        remaining: graphql_remaining,
        reset_epoch: 1_750_000_000,
        core_remaining,
        core_reset_epoch: None,
    }
}

#[test]
fn refusal_is_secondary_classifies_the_verbatim_body_by_the_live_bucket() {
    // The p0 shape: the measured 403 says only "API rate limit exceeded"
    // with both buckets healthy - that IS the secondary limit, whatever
    // the prose says.
    assert!(!VERBATIM_403.to_lowercase().contains("secondary"));
    assert!(refusal_is_secondary(
        VERBATIM_403,
        Some(&quota_with(4446, Some(4980))),
        true
    ));
    assert!(refusal_is_secondary(
        VERBATIM_403,
        Some(&quota_with(4446, Some(4980))),
        false
    ));
}

#[test]
fn refusal_is_secondary_fails_toward_back_off_on_an_unreadable_probe() {
    // No probe (failed, or a caller with none), or a probe that names no
    // core bucket: reading unknown as the primary quota sends the
    // session to wait for a reset that never comes, so unknown still
    // says secondary. This is the fail-safe the Python side ships.
    assert!(refusal_is_secondary(VERBATIM_403, None, true));
    assert!(refusal_is_secondary(
        VERBATIM_403,
        Some(&quota_with(4446, None)),
        true
    ));
}

/// The two stderr wording classifiers are one contract family: gh's own
/// wording decides whether a failed read is world-state (no PR), an
/// outage/retry, or an unrecoverable park (unauthenticated). One test owns
/// the shared vocabulary so the suite stays shrink-only.
#[test]
fn stderr_wording_classifiers() {
    // no-PR positives: real world-state, the fingerprint records it.
    assert!(is_no_pr_stderr(
        b"no pull requests found for branch \"feat\""
    ));
    assert!(is_no_pr_stderr(b"No pull requests found for branch \"x\""));
    // Unauthenticated positives: gh prints the exact fix command.
    assert!(stderr_is_unauthenticated(
        "gh: To get started with GitHub CLI, please run: gh auth login"
    ));
    assert!(stderr_is_unauthenticated(
        "To get started with GitHub CLI, please run:\n\n  gh auth login\n"
    ));
    assert!(stderr_is_unauthenticated(
        "You are not logged into github.com"
    ));
    // Outage, quota, and empty shapes are NEITHER - they keep their own
    // semantics (retry, back off, fingerprint).
    assert!(!is_no_pr_stderr(b"connect: network is unreachable"));
    assert!(!is_no_pr_stderr(b"API rate limit exceeded"));
    assert!(!is_no_pr_stderr(b""));
    assert!(!stderr_is_unauthenticated(
        "connect: network is unreachable"
    ));
    assert!(!stderr_is_unauthenticated("API rate limit exceeded"));
    assert!(!stderr_is_unauthenticated(
        "no pull requests found for branch \"feat\""
    ));
    assert!(!stderr_is_unauthenticated(""));
}

#[test]
fn primary_exhaustion_parks_on_the_ledger_core_reset() {
    // The measured spin shape: a REST read refused with the core bucket at
    // 0 parks on the LEDGER's reset - the fleet's one quota memory - and
    // never on a fabricated window.
    let now = Utc::now().timestamp();
    let q = GraphqlQuota {
        remaining: 1392,
        reset_epoch: now + 300,
        core_remaining: Some(0),
        core_reset_epoch: Some(now + 2400),
    };
    let stderr = "gh: API rate limit exceeded for user ID 4994564 (HTTP 403)";
    assert_eq!(
        primary_exhaustion_park_s("pr_info_rest", stderr, Some(&q), 2400, now),
        Some(2400)
    );
}

#[test]
fn graphql_exhaustion_parks_on_the_probe_reset() {
    // A graphql read refused with its own bucket at 0 parks on the live
    // exempt probe's reset; the ledger holds no graphql bucket.
    let now = Utc::now().timestamp();
    let q = GraphqlQuota {
        remaining: 0,
        reset_epoch: now + 720,
        core_remaining: Some(4980),
        core_reset_epoch: Some(now + 2400),
    };
    let stderr = "gh: API rate limit exceeded for user ID 4994564 (HTTP 403)";
    assert_eq!(
        primary_exhaustion_park_s("pr_view", stderr, Some(&q), 0, now),
        Some(720)
    );
}

#[test]
fn park_refuses_every_non_exhaustion_shape() {
    let now = Utc::now().timestamp();
    let core_drained = GraphqlQuota {
        remaining: 4980,
        reset_epoch: now + 300,
        core_remaining: Some(0),
        core_reset_epoch: Some(now + 2400),
    };
    let stderr = "gh: API rate limit exceeded for user ID 4994564 (HTTP 403)";
    // A blip that is not a rate limit takes the old block: the read may
    // succeed on the very next fire.
    assert_eq!(
        primary_exhaustion_park_s(
            "pr_info_rest",
            "connect: network is unreachable",
            Some(&core_drained),
            2400,
            now
        ),
        None
    );
    // An unreadable probe never fabricates a stall window.
    assert_eq!(
        primary_exhaustion_park_s("pr_info_rest", stderr, None, 2400, now),
        None
    );
    // A refusal the drained core does not explain is the secondary limit:
    // the ledger's backoff wall owns it, the park does not.
    let core_high = GraphqlQuota {
        remaining: 4980,
        reset_epoch: now + 300,
        core_remaining: Some(4980),
        core_reset_epoch: Some(now + 2400),
    };
    assert_eq!(
        primary_exhaustion_park_s("pr_info_rest", stderr, Some(&core_high), 2400, now),
        None
    );
    // Core at 0 with no ledger reset known falls through to the block rather
    // than parking on a made-up window.
    assert_eq!(
        primary_exhaustion_park_s("pr_info_rest", stderr, Some(&core_drained), 0, now),
        None
    );
    // A graphql read refused with its own bucket still up is not explained
    // by the drained core; no park.
    assert_eq!(
        primary_exhaustion_park_s("pr_view", stderr, Some(&core_drained), 2400, now),
        None
    );
}

/// A child that died without stderr still names its death: the exit code in
/// words, or the signal - the shape a load-killed adapter read produced when
/// the gate rendered it as a blank tail.
#[test]
fn silent_nonzero_exits_name_their_cause() {
    use std::os::unix::process::ExitStatusExt;
    let signaled = std::process::ExitStatus::from_raw(9);
    assert_eq!(
        cause_for_silent_exit(&signaled),
        "terminated by signal with no stderr"
    );
    let exited = std::process::ExitStatus::from_raw(1 << 8);
    assert_eq!(cause_for_silent_exit(&exited), "exited 1 with no stderr");
}

// ---------------------------------------------------------------------------
// The stop-gate GraphQL fallback: the broker-field-set retry
// ---------------------------------------------------------------------------

/// A fake `fno-gh-loopcheck` answering the three spawn shapes the fallback
/// machinery makes: the rate_limit probe, the url-bearing broker read (the
/// `graphql_arm` shell lines decide its answer), and everything else - the
/// metadata read - with the REST refusal. One argv row lands in the calls
/// file per spawn.
fn fallback_adapter(
    dir: &Path,
    probe_body: &str,
    graphql_arm: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let calls = dir.join("calls.log");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FGB_CALLS\"\ncase \"$*\" in\n  *\"api rate_limit\"*)\n    printf '%s' '{probe_body}'\n    ;;\n  *url*)\n    {graphql_arm}\n    ;;\n  *)\n    printf '%s' 'gh: API rate limit exceeded for user ID 123 (HTTP 403)' >&2\n    exit 1\n    ;;\nesac\n"
    );
    let bin = write_exec(dir, "fno-gh-loopcheck", &script);
    (bin, calls)
}

const FGB_PROBE_CORE_DRAINED: &str = r#"{"resources":{"graphql":{"remaining":0,"reset":1791234567},"core":{"remaining":0,"reset":1791234500}}}"#;

const FGB_PROBE_CORE_HEALTHY: &str = r#"{"resources":{"graphql":{"remaining":4800,"reset":1791234567},"core":{"remaining":4980,"reset":1791234500}}}"#;

/// The broker field set is the metadata set plus exactly `,url`, and the
/// info set is the metadata set plus the four more the info payload maps.
/// The superset relationships are what route the fallback reads to the
/// broker and promise every key the mappers read; the consts are literals
/// (concat! takes literals only), so drift between the strings is the
/// failure this guards.
#[test]
fn graphql_field_sets_extend_the_metadata_set() {
    use crate::loopcheck::gh_read::{
        PR_INFO_FALLBACK_FIELDS, PR_VIEW_FIELDS, PR_VIEW_FIELDS_GRAPHQL,
    };
    let gate_extra = PR_VIEW_FIELDS_GRAPHQL
        .strip_prefix(PR_VIEW_FIELDS)
        .expect("the gate field set extends the metadata set");
    assert_eq!(gate_extra, ",url");
    let info_extra = PR_INFO_FALLBACK_FIELDS
        .strip_prefix(PR_VIEW_FIELDS)
        .expect("the info field set extends the metadata set");
    assert_eq!(
        info_extra,
        ",url,body,mergedAt,mergeCommit,autoMergeRequest"
    );
}

/// The retry the earned fallback makes: same argv, broker-routing field
/// set, and the repo the caller resolved pinned with --repo. The url the
/// REST adapter does not serve is what routes the read to the
/// quota-brokered GraphQL executor, and the reply parses through the
/// identical consumer path.
#[test]
fn graphql_fallback_retries_the_broker_field_set() {
    let _env = crate::claims::test_env_lock();
    let answer = r#"{"state":"OPEN","number":42,"headRefName":"feature/pr-info-fallback","headRefOid":"abc123def","mergeable":"MERGEABLE","mergeStateStatus":"BEHIND","baseRefName":"main","author":{"login":"jj"},"url":"https://github.test/o/r/pull/42"}"#;
    // A loaded runner intermittently fails one fork/exec; the stub answers
    // deterministically, so only a failed fork can red the happy path (the
    // measured reason the head_is_shipped cases retry).
    for _ in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let (bin, calls) = fallback_adapter(
            dir.path(),
            FGB_PROBE_CORE_DRAINED,
            &format!("printf '%s' '{answer}'"),
        );
        std::env::set_var("FGB_CALLS", &calls);
        std::env::set_var("FNO_LOOPCHECK_GH_BIN", &bin);
        let read = read_pr_view_fields(
            bin.to_str().unwrap(),
            dir.path(),
            Some("42"),
            crate::loopcheck::PR_INFO_FALLBACK_FIELDS,
            Some("o/r"),
            std::time::Duration::from_secs(10),
        );
        std::env::remove_var("FGB_CALLS");
        std::env::remove_var("FNO_LOOPCHECK_GH_BIN");
        let Ok(Some(value)) = read else { continue };
        assert_eq!(
            value.get("url").and_then(|v| v.as_str()),
            Some("https://github.test/o/r/pull/42")
        );
        let recorded = std::fs::read_to_string(&calls).unwrap();
        let rows: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            rows.len(),
            3,
            "probe + refused metadata read + fallback read: {recorded}"
        );
        assert!(
            rows[2].contains("url"),
            "the retry carries the broker-routing field set: {}",
            rows[2]
        );
        assert!(
            rows[2].contains("--repo o/r"),
            "the retry pins the caller's repo: {}",
            rows[2]
        );
        return;
    }
    panic!("the fallback never answered in five attempts");
}

/// A fallback that answers nothing hands back the ORIGINAL REST refusal
/// with the fallback's own cause appended after it: the REST wording stays
/// first (the classifiers and quota parks read it), and the appended cause
/// is what tells the operator a second transport ran.
#[test]
fn graphql_fallback_failure_returns_the_original_rest_refusal() {
    let _env = crate::claims::test_env_lock();
    for _ in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let (bin, _calls) = fallback_adapter(
            dir.path(),
            FGB_PROBE_CORE_DRAINED,
            "printf '%s' 'graphql discretionary read refused: budget exhausted' >&2\n    exit 1",
        );
        std::env::set_var("FNO_LOOPCHECK_GH_BIN", &bin);
        let read = read_pr_view(
            bin.to_str().unwrap(),
            dir.path(),
            Some("42"),
            std::time::Duration::from_secs(10),
        );
        std::env::remove_var("FNO_LOOPCHECK_GH_BIN");
        let Err(err) = read else { continue };
        assert!(
            err.stderr_tail.starts_with("gh: API rate limit exceeded"),
            "the original REST refusal leads: {:?}",
            err.stderr_tail
        );
        assert!(
            err.stderr_tail
                .contains("| graphql fallback also failed: pr_view:"),
            "the fallback cause follows, naming the read: {:?}",
            err.stderr_tail
        );
        return;
    }
    panic!("no attempt produced a read at all");
}

/// A secondary refusal is the request rate; a second transport only
/// sustains it. The probe reading a healthy core bucket classifies the
/// refusal secondary and the fallback never fires: two spawns, no retry.
#[test]
fn secondary_refusal_never_earns_the_fallback() {
    let _env = crate::claims::test_env_lock();
    for _ in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let (bin, calls) = fallback_adapter(
            dir.path(),
            FGB_PROBE_CORE_HEALTHY,
            "printf '%s' 'must never be asked' >&2\n    exit 1",
        );
        std::env::set_var("FGB_CALLS", &calls);
        std::env::set_var("FNO_LOOPCHECK_GH_BIN", &bin);
        let read = read_pr_view(
            bin.to_str().unwrap(),
            dir.path(),
            Some("42"),
            std::time::Duration::from_secs(10),
        );
        std::env::remove_var("FGB_CALLS");
        std::env::remove_var("FNO_LOOPCHECK_GH_BIN");
        let Err(err) = read else { continue };
        assert!(
            err.stderr_tail.contains("rate limit"),
            "the secondary refusal itself is the answer: {:?}",
            err.stderr_tail
        );
        let recorded = std::fs::read_to_string(&calls).unwrap();
        let rows: Vec<&str> = recorded.lines().collect();
        assert_eq!(rows.len(), 2, "metadata read + probe, no retry: {recorded}");
        return;
    }
    panic!("no attempt produced a read at all");
}
