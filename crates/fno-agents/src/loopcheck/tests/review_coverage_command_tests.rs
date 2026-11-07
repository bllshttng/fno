use super::*;

/// Run the command until the quota probe answers decisively (a bool
/// graphql_exhausted): a fork/exec blip on a loaded runner lands as
/// probe_graphql_quota -> None (null), which is correct behavior - the
/// assertions need a decisive read, not the first one.
fn run_exit4_until_decisive(args: &[String]) -> Value {
    for _ in 0..5 {
        let (code, out) = run_review_coverage_capture(args);
        assert_eq!(code, 4);
        let parsed: Value = serde_json::from_str(&out).expect("stdout is one JSON object");
        if parsed
            .get("graphql_exhausted")
            .and_then(|x| x.as_bool())
            .is_some()
        {
            return parsed;
        }
    }
    panic!(
        "exit-4 stdout never carried a decisive graphql_exhausted across 5 \
             runs: either the gh stub kept failing to spawn or the stdout \
             contract changed - both are real failures"
    );
}

#[test]
fn review_coverage_pr_failure_stdout_carries_quota_diagnostic() {
    let _root = crate::paths::DeclaredRoot::declare("review_coverage_pr_failure_s");
    // exit 4 with a known PR persists a schema-gated unknown row,
    // and its stdout must say WHY the read degraded. A bare unknown is
    // indistinguishable from "nobody reviewed this" and sent operators to
    // re-review PRs whose only problem was an exhausted quota window.
    let tmp = tempfile::tempdir().unwrap();
    let gh = write_failing_pr_view_gh(tmp.path(), 0, 14 * 60, 0);
    let events = tmp.path().join("ev.jsonl");
    let args = review_coverage_args(tmp.path(), "930c2e9dad5d2dc5ba2deae320070bd86ecfcfc2", &gh);
    let v = run_exit4_until_decisive(&args);
    assert_eq!(v["coverage"], "unknown");
    assert_eq!(v["graphql_exhausted"], true, "got: {v}");
    let reason = v["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("GraphQL quota exhausted"),
        "reason must name the cause, got: {reason}"
    );
    // The PERSISTED row keeps the bare schema: stdout-only diagnostics, no
    // event-contract fork. Pin the destination, not the tag - the row must
    // lack the diagnostic keys, and carry the unknown verdict as emitted.
    let log = crate::events::committed_journal_text(&events);
    let row: Value =
        serde_json::from_str(log.lines().next().expect("one row")).expect("row is JSON");
    assert_eq!(row["type"], "review_coverage");
    assert_eq!(row["data"]["coverage"], "unknown");
    assert_eq!(row["data"]["verdicts"], serde_json::json!([]));
    assert!(row["data"].get("graphql_exhausted").is_none());
    assert!(row["data"].get("graphql_remaining").is_none());
    assert!(row["data"].get("reason").is_none());
}

#[test]
fn review_coverage_pr_failure_verbatim_403_classifies_secondary_via_the_exempt_probe() {
    let _root = crate::paths::DeclaredRoot::declare("review_coverage_pr_failure_v");
    // The p0 shape on this command: the failed read's stderr is the
    // MEASURED 2026-08-24 body (no "secondary" anywhere) while the exempt
    // rate_limit endpoint still answers healthy. The classifier is that
    // one probe - it fires exactly once (the marker), never counts
    // against a bucket, and the verdict keeps the graphql diagnostics
    // null so the reason names the burst limit, not a healthy bucket.
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("probe-fired");
    let gh = write_exec(
            tmp.path(),
            "gh",
            &format!(
                "#!/bin/sh\n\
                 [ \"$1\" = pr ] && [ \"$2\" = view ] && \
                 echo 'API rate limit exceeded for user ID 4994564. If you reach out to GitHub Support for help, please include the request ID FAEB:283161:6EF36:99B72:6A8B97DD ... (HTTP 403)' >&2 && exit 1\n\
                 [ \"$1\" = api ] && [ \"$2\" = rate_limit ] && \
                 echo '{{\"resources\":{{\"graphql\":{{\"remaining\":4446,\"reset\":1750000000}},\
                 \"core\":{{\"remaining\":4980,\"limit\":5000,\"reset\":1750000000}}}}}}' && \
                 echo probe > {marker} && exit 0\n\
                 exit 1\n",
                marker = marker.display()
            ),
        );
    let events = tmp.path().join("ev.jsonl");
    // Retry until the pr-view spawn lands (its fork is the one flake this
    // suite retries for): a spawn failure degrades tail to an exec error,
    // the secondary match misses, and the probe fires.
    let mut v: Option<Value> = None;
    for _ in 0..5 {
        let (code, out) = run_review_coverage_capture(&[
            "review-coverage".to_string(),
            "--cwd".to_string(),
            tmp.path().display().to_string(),
            "--pr".to_string(),
            "865".to_string(),
            "--head".to_string(),
            "deadbeefdeadbeefdeadbeefdeadbeef00000001".to_string(),
            "--events".to_string(),
            events.display().to_string(),
            "--global-events".to_string(),
            tmp.path().join("gev.jsonl").display().to_string(),
            "--settings".to_string(),
            tmp.path().join("absent.toml").display().to_string(),
            "--gh-bin".to_string(),
            gh.display().to_string(),
        ]);
        assert_eq!(code, 4);
        if let Ok(parsed) = serde_json::from_str::<Value>(&out) {
            if parsed.get("reason").is_some() {
                v = Some(parsed);
                break;
            }
        }
    }
    let v = v.expect("5 runs never produced a reasoned stdout");
    let reason = v["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("secondary rate limit"),
        "reason must name the secondary limit, got: {reason}"
    );
    assert!(
        v["graphql_exhausted"].is_null(),
        "secondary keeps the quota diagnostics null: exhausted reads null, got: {v}"
    );
    // The exempt probe FIRED exactly as the classifier; the marker is the
    // positive control that classification came from a live reading.
    assert!(
        marker.exists(),
        "the exempt probe is the classifier - it must fire"
    );
    assert_eq!(v["coverage"], "unknown");
}
