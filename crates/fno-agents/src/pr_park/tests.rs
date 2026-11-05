use super::*;

fn write(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

/// An RFC3339 UTC stamp `hours` before now. Age-gated fixtures must ride the
/// clock: a literal date crosses the 24h park bound a day after it is written
/// and flips the sweep under the test.
fn iso_hours_ago(hours: u64) -> String {
    let t = now_secs().saturating_sub(hours * 3600);
    let (year, month, day, hour, min, sec) = crate::events::civil_from_unix(t);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// The fixture store: one open-PR park, one finished delivery park, one
/// foreign park. Three buckets, one of each.
fn fixture(paths: &Paths) {
    let fresh = iso_hours_ago(2);
    let state_text = r#"{
  "owner/repo#101": {
    "last_seen_state": "OPEN",
    "retries": 3,
    "parked": "retries-exhausted",
    "last_polled_at": "{fresh}"
  },
  "other/repo#55": {
    "last_seen_state": "OPEN",
    "retries": 3,
    "parked": "max-age",
    "last_polled_at": "{fresh}"
  }
}"#
    .replace("{fresh}", &fresh);
    write(&paths.state, &state_text);
    let delivery_text = r#"{
  "owner/repo#42": {
    "last_seen_state": "MERGED",
    "retries": 3,
    "parked": "retries-exhausted",
    "last_polled_at": "{fresh}"
  }
}"#
    .replace("{fresh}", &fresh);
    write(&paths.delivery, &delivery_text);
    // The open row's failure detail, as the merge phase committed it.
    for line in [
        r#"{"ts":"2026-09-16T15:29:00Z","type":"lock_seed","source":"test","data":{"pr":101}}"#,
        r#"{"ts":"2026-09-16T15:29:30Z","type":"merge_grant_execution","source":"daemon","data":{"phase":"failed","actor":"pr-watch","pr":101,"exit_code":1}}"#,
    ] {
        crate::event_store::append_envelope(&paths.events, line, None).unwrap();
    }
    write(
        &paths.err_log,
        r#"{"pr":101,"outcome":"failed","reason":"failed: checks are red; require_checks_pass forbids merging without green","strategy":"merge"}
"#,
    );
}

/// A probe answering the same head on every call.
fn ctx(paths: Paths) -> Ctx {
    ctx_ans(paths, "head1", "OPEN")
}

/// A probe whose answer is fixed per construction; the sweep test moves the
/// head by building a fresh Ctx with the next answer.
fn ctx_ans(paths: Paths, head: &'static str, state: &'static str) -> Ctx {
    ctx_probes(paths, (head, "feature", state), Ok(true), Ok(false))
}

/// A Ctx with every probe injectable: the head answers `(sha, branch, state)`
/// for the open row, the checks probe answers red-or-not, the worktree probe
/// answers clear-or-not. The defaults read "still red" and "still mismatched",
/// the answers a cause park that has not cleared gives.
fn ctx_probes(
    paths: Paths,
    head: (&'static str, &'static str, &'static str),
    checks: Result<bool, &'static str>,
    worktree: Result<bool, &'static str>,
) -> Ctx {
    Ctx {
        paths,
        slug: "owner/repo".to_string(),
        entries: vec![json!({
            "id": "x-open",
            "status": "in_review",
            "pr_number": 101,
            "pr_url": "https://github.com/owner/repo/pull/101"
        })],
        head: Box::new(move |pr| {
            if pr == 101 {
                Ok((head.0.to_string(), head.1.to_string(), head.2.to_string()))
            } else {
                Err("probe failed".to_string())
            }
        }),
        checks_red: Box::new(move |_| checks.map_err(|e| e.to_string())),
        worktree_clear: Box::new(move |_, _| worktree.map_err(|e| e.to_string())),
    }
}

/// One open row parked `reason`, with the `parked_at` stamp and the
/// `last_polled_at` stamp aged independently so the two age arms hold apart.
/// `parked_age_h` of `None` seeds a row parked before the stamp shipped.
fn fixture_cause(paths: &Paths, reason: &str, parked_age_h: Option<u64>, polled_age_h: u64) {
    let mut body = json!({
        "last_seen_state": "OPEN",
        "retries": 1,
        "parked": reason,
        "last_polled_at": iso_hours_ago(polled_age_h),
    });
    if let Some(h) = parked_age_h {
        body["parked_at"] = json!(iso_hours_ago(h));
    }
    write(&paths.state, &json!({"owner/repo#101": body}).to_string());
}

fn tmp_paths(tag: &str) -> Paths {
    let dir = std::env::temp_dir().join(format!("pr-park-test-{tag}-{}", std::process::id()));
    Paths {
        state: dir.join("pr-watcher-state.json"),
        delivery: dir.join("pr-watcher-state-delivery.json"),
        events: dir.join("events.jsonl"),
        err_log: dir.join("pr-watcher.err.log"),
    }
}

#[test]
fn list_buckets_open_finished_and_foreign() {
    let paths = tmp_paths("list");
    fixture(&paths);
    let rows = list_rows(&ctx(paths.clone()));
    assert_eq!(rows.len(), 3);
    let by_key = |k: &str| rows.iter().find(|r| r.key == k).unwrap().clone();
    assert_eq!(by_key("owner/repo#101").bucket, "open");
    assert_eq!(by_key("owner/repo#42").bucket, "finished");
    assert_eq!(by_key("other/repo#55").bucket, "foreign");
    // The open row resolves its node and its stored failure detail.
    let open = by_key("owner/repo#101");
    assert_eq!(open.node, "x-open");
    assert_eq!(open.node_status, "in_review");
    assert!(
        open.reason_detail.contains("failed"),
        "{}",
        open.reason_detail
    );
    // Clean up after the reason-detail tail read.
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());
}

#[test]
fn unpark_all_open_leaves_delivery_and_foreign() {
    let paths = tmp_paths("all-open");
    fixture(&paths);
    let n = unpark(&ctx(paths.clone()), None, true).unwrap();
    assert_eq!(n, 1);
    let data: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.state).unwrap()).unwrap();
    assert!(data["owner/repo#101"]["parked"].is_null());
    assert!(!data["other/repo#55"]["parked"].is_null());
    let delivery: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.delivery).unwrap()).unwrap();
    assert!(!delivery["owner/repo#42"]["parked"].is_null());
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());
}

#[test]
fn sweep_unparks_on_head_change_and_holds_on_same_head() {
    let paths = tmp_paths("sweep-head");
    fixture(&paths);
    // Baseline round: records the head it sees and parks nothing.
    let r = sweep(&ctx(paths.clone())).unwrap();
    assert_eq!(r.unparked, 0);
    let data: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.state).unwrap()).unwrap();
    assert_eq!(data["owner/repo#101"]["parked_head"], json!("head1"));
    assert!(!data["owner/repo#101"]["parked"].is_null());
    // Same head: stays parked.
    let _ = sweep(&ctx(paths.clone())).unwrap();
    let data: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.state).unwrap()).unwrap();
    assert!(!data["owner/repo#101"]["parked"].is_null());
    // A pushed head: un-parked.
    let _ = sweep(&ctx_ans(paths.clone(), "head2", "OPEN")).unwrap();
    let data: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.state).unwrap()).unwrap();
    assert!(data["owner/repo#101"]["parked"].is_null());
    let events = crate::events::committed_journal_text(&paths.events);
    assert!(events.contains("\"by\":\"sweep\""), "{events}");
    assert!(events.contains("\"cause\":\"head-moved\""), "{events}");
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());
}

#[test]
fn mutations_hold_the_lock_file_the_watcher_takes() {
    // The `.lock` files are the cross-language serialization point: the
    // Python WatermarkStore.set() takes the same flock, so a sweep rewrite
    // can never interleave with a tick write.
    let paths = tmp_paths("lock");
    fixture(&paths);
    let _ = unpark(&ctx(paths.clone()), Some("owner/repo#101"), false).unwrap();
    assert!(crate::gh_budget::lock_path(&paths.state).exists());
    assert!(crate::gh_budget::lock_path(&paths.delivery).exists());
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());
}

/// Seed the cause fixture with its two clock ages, sweep with the given
/// probes, and return the report, the store, and the committed journal.
/// The caller removes the tempdir.
fn run_cause_sweep(
    tag: &str,
    reason: &str,
    parked_age_h: Option<u64>,
    polled_age_h: u64,
    head: (&'static str, &'static str, &'static str),
    checks: Result<bool, &'static str>,
    worktree: Result<bool, &'static str>,
) -> (SweepReport, Value, String, Paths) {
    let paths = tmp_paths(tag);
    fixture_cause(&paths, reason, parked_age_h, polled_age_h);
    let ctx = ctx_probes(paths.clone(), head, checks, worktree);
    let r = sweep(&ctx).unwrap();
    let data: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.state).unwrap()).unwrap();
    let events = crate::events::committed_journal_text(&paths.events);
    (r, data, events, paths)
}

#[test]
fn sweep_lifts_a_checks_red_park_only_when_checks_read_green() {
    // Green lifts, with the cause on the event; red and a probe error hold.
    let (r, data, events, paths) = run_cause_sweep(
        "cause-checks-green",
        "checks-red",
        None,
        2,
        ("head1", "feature", "OPEN"),
        Ok(false),
        Ok(false),
    );
    assert_eq!(r.unparked, 1);
    assert!(data["owner/repo#101"]["parked"].is_null());
    assert_eq!(data["owner/repo#101"]["retries"], json!(0));
    assert!(events.contains("\"cause\":\"checks-cleared\""), "{events}");
    assert!(events.contains("\"by\":\"sweep\""), "{events}");
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());

    for checks in [Ok(true), Err("gh down")] {
        let (r, data, events, paths) = run_cause_sweep(
            "cause-checks-hold",
            "checks-red",
            None,
            2,
            ("head1", "feature", "OPEN"),
            checks,
            Ok(false),
        );
        assert_eq!(r.unparked, 0);
        assert_eq!(data["owner/repo#101"]["parked"], json!("checks-red"));
        assert!(!events.contains("pr_watch_unparked"), "{events}");
        let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());
    }
}

#[test]
fn a_worktree_park_lifts_when_clear_and_holds_on_probe_error() {
    for reason in ["worktree_head_mismatch", "worktree_dirty"] {
        let (r, _, events, paths) = run_cause_sweep(
            "cause-worktree",
            reason,
            None,
            2,
            ("head1", "feature", "OPEN"),
            Ok(true),
            Ok(true),
        );
        assert_eq!(r.unparked, 1, "{reason}");
        assert!(
            events.contains("\"cause\":\"worktree-cleared\""),
            "{events}"
        );
        let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());
    }
    // A probe error lifts nothing, the rule the head probe keeps.
    let (r, data, _, paths) = run_cause_sweep(
        "cause-worktree-err",
        "worktree_head_mismatch",
        None,
        2,
        ("head1", "feature", "OPEN"),
        Ok(true),
        Err("git exploded"),
    );
    assert_eq!(r.unparked, 0);
    assert_eq!(
        data["owner/repo#101"]["parked"],
        json!("worktree_head_mismatch")
    );
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());
}

#[test]
fn the_age_rule_reads_parked_at_then_the_poll_stamp_and_unpark_clears_it() {
    // A merge-queue row never refreshes last_polled_at, so on main its park
    // age read hours of queue silence and every retries-exhausted park
    // lifted on the first sweep. The park's own stamp governs now; a row
    // parked before the stamp shipped keeps the poll stamp; an unpark
    // clears the stamp beside the park.
    let (r, _, events, paths) = run_cause_sweep(
        "age-parked-at-hold",
        "retries-exhausted",
        Some(1),
        30,
        ("head1", "feature", "OPEN"),
        Ok(true),
        Ok(false),
    );
    assert_eq!(
        r.unparked, 0,
        "a young parked_at holds despite stale last_polled_at"
    );
    assert!(!events.contains("pr_watch_unparked"), "{events}");
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());

    let (r, _, events, paths) = run_cause_sweep(
        "age-parked-at-lift",
        "retries-exhausted",
        Some(25),
        30,
        ("head1", "feature", "OPEN"),
        Ok(true),
        Ok(false),
    );
    assert_eq!(r.unparked, 1);
    assert!(events.contains("\"cause\":\"max-age\""), "{events}");
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());

    let (r, _, _, paths) = run_cause_sweep(
        "age-legacy",
        "retries-exhausted",
        None,
        30,
        ("head1", "feature", "OPEN"),
        Ok(true),
        Ok(false),
    );
    assert_eq!(r.unparked, 1, "the legacy stamp still carries the age arm");
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());

    let paths = tmp_paths("manual-unpark-stamp");
    fixture_cause(&paths, "checks-red", Some(3), 3);
    let n = unpark(&ctx(paths.clone()), Some("owner/repo#101"), false).unwrap();
    assert_eq!(n, 1);
    let data: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.state).unwrap()).unwrap();
    assert!(data["owner/repo#101"]["parked"].is_null());
    assert!(data["owner/repo#101"]["parked_at"].is_null());
    let _ = std::fs::remove_dir_all(paths.state.parent().unwrap());
}
