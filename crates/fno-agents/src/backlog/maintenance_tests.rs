use super::*;

// 2026-09-09T15:46:29Z, the do-row fixtures' start.
const START_MS: i64 = 1_788_968_789_000;
const CODEX_ID: &str = "01a06886-9405-74a1-8afd-5b67baf89604";
const CLAUDE_SID: &str = "9f06a492-1111-4222-8333-444455556666";

fn ws() -> HashMap<String, String> {
    [("fno", "/home/u/code/fno"), ("etl", "/home/u/code/etl")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn node(id: &str, extra: Value) -> Value {
    let mut n = json!({"id": id, "title": id, "status": "ready"});
    for (k, v) in extra.as_object().unwrap() {
        n[k] = v.clone();
    }
    n
}

fn iso_days_ago(days: i64) -> String {
    chrono::DateTime::from_timestamp_millis(now_ms() - days * 86_400_000)
        .unwrap()
        .to_rfc3339()
}

fn ids<T>(items: &[T], f: impl Fn(&T) -> &str) -> Vec<String> {
    items.iter().map(|i| f(i).to_string()).collect()
}

#[test]
fn rescope_fixes_every_drift_shape_and_leaves_the_rest() {
    let fix = |project: Value, cwd: &str, base: Option<&str>| {
        detect_rescope_fixes(
            &[node("ab-1", json!({"project": project, "cwd": cwd}))],
            &ws(),
            base,
        )
        .into_iter()
        .map(|f| (f.new_project, f.new_cwd))
        .collect::<Vec<_>>()
    };
    let etl = vec![("etl".to_string(), "/home/u/code/etl".to_string())];
    let fno = vec![("fno".to_string(), "/home/u/code/fno".to_string())];
    assert_eq!(fix(Value::Null, "/home/u/code/fno", None), fno);
    assert_eq!(
        fix(json!("fno"), "/home/u/conductor/workspaces/fno/foo", None),
        fno
    );
    assert_eq!(fix(json!("bogus"), "/home/u/code/etl", None), etl);
    assert_eq!(
        fix(Value::Null, "/x/conductor/workspaces/etl/bar", None),
        etl
    );
    assert_eq!(
        fix(Value::Null, "/home/u/code/etl/.claude/worktrees/bar", None),
        etl
    );
    assert_eq!(
        fix(Value::Null, "/custom/wt/etl/bar", Some("/custom/wt")),
        etl
    );
    assert!(fix(Value::Null, "/custom/wt/etl/bar", None).is_empty());
    assert!(fix(json!("fno"), "/home/u/code/fno", None).is_empty());
    assert!(fix(Value::Null, "/somewhere/unmapped", None).is_empty());
    assert!(detect_rescope_fixes(
        &[node("ab-1", json!({"cwd": "/home/u/code/etl"}))],
        &HashMap::new(),
        None
    )
    .is_empty());
}

#[test]
fn temp_leaks_need_a_test_marker_not_a_temp_root() {
    for leak in [
        "/tmp/pytest-of-bob/pytest-3/x",
        "/private/var/folders/aa/bb/T/fno-test-home-xyz",
    ] {
        assert!(is_temp_cwd(Some(&json!(leak))), "{leak}");
    }
    for real in [
        "/home/u/code/fno",
        "",
        "/tmp/my-scratch-project",
        "/var/folders/aa/T/wt",
    ] {
        assert!(!is_temp_cwd(Some(&json!(real))), "{real}");
    }
    let entries = [
        node("ab-good", json!({"cwd": "/home/u/code/fno"})),
        node("ab-leak", json!({"cwd": "/tmp/pytest-of-x/pytest-1/proj"})),
    ];
    assert_eq!(detect_temp_leaks(&entries), ["ab-leak"]);
}

#[test]
fn url_less_prs_resolve_once_per_cwd_and_report_the_unresolvable() {
    let calls = std::cell::RefCell::new(Vec::new());
    let resolver = |cwd: Option<&str>| {
        calls.borrow_mut().push(cwd.map(str::to_string));
        (cwd == Some("/repo/a")).then(|| "o/a".to_string())
    };
    let entries = [
        node("ab-1", json!({"cwd": "/repo/a", "pr_number": 7})),
        node(
            "ab-2",
            json!({"cwd": "/repo/a", "pr_number": 8, "pr_url": "https://github.com/o/a/pull/8"}),
        ),
        node("ab-3", json!({"cwd": "/repo/a", "pr_number": 9})),
        node("ab-4", json!({"cwd": "/gone", "pr_number": 7})),
    ];
    let fixes = detect_url_less_prs(&entries, &resolver);
    let got: Vec<(String, Option<String>)> =
        fixes.into_iter().map(|f| (f.node_id, f.pr_url)).collect();
    assert_eq!(
        got,
        [
            (
                "ab-1".to_string(),
                Some("https://github.com/o/a/pull/7".to_string())
            ),
            (
                "ab-3".to_string(),
                Some("https://github.com/o/a/pull/9".to_string())
            ),
            ("ab-4".to_string(), None),
        ]
    );
    assert_eq!(calls.borrow().len(), 2, "one resolution per distinct cwd");
    // The default resolver never degrades to the invocation cwd.
    assert_eq!(slug_from_node_cwd(Some("/no/such/dir/at/all")), None);
    assert_eq!(slug_from_node_cwd(None), None);
}

#[test]
fn judgment_detectors_scope_dedup_rollup_and_plan_cost() {
    let entries = [
        node("ab-a", json!({"title": "Fix the thing", "status": "idea"})),
        node(
            "ab-b",
            json!({"title": "fix the  thing!", "status": "idea"}),
        ),
        node("ab-c", json!({"title": "Fix the thing", "status": "ready"})),
    ];
    assert_eq!(
        detect_dup_groups(&entries),
        [vec!["ab-a".to_string(), "ab-b".to_string()]]
    );

    let plans = [
        node("ab-a", json!({"plan_path": "/plans/p.md", "cost_usd": 1.0})),
        node(
            "ab-b",
            json!({"plan_path": "/plans/./p.md", "cost_usd": 0.8}),
        ),
        node("ab-c", json!({"plan_path": "/plans/q.md", "cost_usd": 2.0})),
        node(
            "ab-d",
            json!({"plan_path": "/plans/q.md", "cost_usd": null}),
        ),
    ];
    assert_eq!(
        detect_shared_plan_cost_violations(&plans),
        [(
            "/plans/p.md".to_string(),
            vec!["ab-a".to_string(), "ab-b".to_string()]
        )]
    );

    let epic = node(
        "x-mux",
        json!({"type": "epic", "title": "mux pane layout polish"}),
    );
    let orphan = |id: &str, extra: Value| {
        let mut n = node(
            id,
            json!({"type": "feature", "title": "mux pane layout resize"}),
        );
        for (k, v) in extra.as_object().unwrap() {
            n[k] = v.clone();
        }
        n
    };
    let found = detect_rollup_candidates(&[epic.clone(), orphan("x-1", json!({}))], 20);
    assert_eq!(ids(&found, |p| p.1.as_str()), ["x-mux"]);
    for exempt in [
        json!({"parent": "x-mux"}),
        json!({"status": "done"}),
        json!({"orphan_ok": true}),
        json!({"type": "bug"}),
        json!({"title": "quantum teapot"}),
    ] {
        assert!(
            detect_rollup_candidates(&[epic.clone(), orphan("x-1", exempt.clone())], 20).is_empty(),
            "{exempt}"
        );
    }
    let many: Vec<Value> = std::iter::once(epic)
        .chain((0..25).map(|i| {
            orphan(
                &format!("x-{i:03}"),
                json!({"title": format!("mux pane layout polish variant {i}")}),
            )
        }))
        .collect();
    let capped = detect_rollup_candidates(&many, 20);
    assert_eq!(capped.len(), 20);
    assert!(capped.windows(2).all(|w| w[0].2 >= w[1].2), "best first");
}

#[test]
fn stale_ideas_read_the_curation_touch_strictly_and_spare_movement() {
    let idea = |id: &str, extra: Value| {
        let mut n = node(id, json!({"status": "idea"}));
        for (k, v) in extra.as_object().unwrap() {
            n[k] = v.clone();
        }
        n
    };
    let recent_vote = iso_days_ago(1);
    let entries = [
        idea("ab-old", json!({"created_at": iso_days_ago(40)})),
        idea("ab-edge", json!({"created_at": iso_days_ago(30)})),
        idea(
            "ab-touched",
            json!({"created_at": iso_days_ago(40), "touched_at": iso_days_ago(0)}),
        ),
        idea(
            "ab-moving",
            json!({"created_at": iso_days_ago(40), "sessions": [{"phase": "execute"}]}),
        ),
        idea(
            "ab-voted",
            json!({"created_at": iso_days_ago(40), "encounters": [{"created_at": recent_vote}]}),
        ),
        idea(
            "ab-old-vote",
            json!({"created_at": iso_days_ago(40), "encounters": [{"created_at": iso_days_ago(45)}]}),
        ),
        idea("ab-nots", json!({})),
        node("ab-ready", json!({"created_at": iso_days_ago(40)})),
    ];
    // Read after the stamps: a clock read before them makes a 40-day stamp
    // a millisecond short of 40 days, and the age floors to 39.
    let now = now_ms();
    let stale = detect_stale_ideas(&entries, 30, now);
    assert_eq!(
        ids(&stale, |s| s.node_id.as_str()),
        ["ab-old", "ab-old-vote"]
    );
    assert_eq!(stale[0].age_days, 40);
}

#[test]
fn stale_ready_exempts_undesigned_contained_blocked_and_moved_work() {
    let dir = tempfile::tempdir().unwrap();
    let now = now_ms();
    let old = iso_days_ago(80);
    let design = dir.path().join("d.md");
    std::fs::write(&design, "---\nstatus: design\n---\n").unwrap();
    std::fs::File::options()
        .append(true)
        .open(&design)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    let entries = [
        node("a", json!({"created_at": old})),
        node("b", json!({"created_at": old, "status": "idea"})),
        node("c", json!({"created_at": old, "pr_number": 1})),
        node("d", json!({"created_at": iso_days_ago(21)})),
        node("e", json!({"created_at": old, "contained_in": "x-6320"})),
        node("f", json!({"created_at": old, "blocked_by": ["dep"]})),
        node(
            "g",
            json!({"created_at": old, "plan_path": design.to_string_lossy()}),
        ),
        node("h", json!({})),
    ];
    let got = detect_stale_ready(&entries, 21, now);
    assert_eq!(
        got,
        [Stale {
            node_id: "a".into(),
            age_days: 80
        }]
    );
}

#[test]
fn suspect_reverts_need_a_curation_signal_on_a_drained_node() {
    let drained = |id: &str, extra: Value| {
        let mut n = node(
            id,
            json!({"status": "deferred",
            "deferred_reason": "stale >30d, drained by maintain",
            "deferred_at": "2026-08-14T18:19:51.500000+00:00"}),
        );
        for (k, v) in extra.as_object().unwrap() {
            n[k] = v.clone();
        }
        n
    };
    let entries = [
        drained("ab-reverted", json!({})),
        drained(
            "ab-p1",
            json!({"priority": "p1", "deferred_reason": "stale >45d, drained by maintain"}),
        ),
        drained("ab-genuine", json!({})),
        drained(
            "ab-other",
            json!({"priority": "p1", "deferred_reason": "operator call"}),
        ),
    ];
    // The log's `Z` stamp in the same second as a microsecond deferred_at.
    let events = [
        json!({"type": "node_undeferred", "ts": "2026-08-14T18:19:51Z",
                         "data": {"unit_id": "ab-reverted"}}),
    ];
    let before = entries.to_vec();
    let got = detect_suspect_reverts(&entries, &events);
    assert_eq!(
        got.iter()
            .map(|r| (r.node_id.as_str(), r.signal.as_str()))
            .collect::<Vec<_>>(),
        [
            ("ab-reverted", "undeferred before this defer"),
            ("ab-p1", "priority p1 (non-default ranking)"),
        ]
    );
    assert_eq!(entries.to_vec(), before);
}

#[test]
fn failure_defers_respect_the_threshold_and_carry_the_window_cause() {
    let fail = |id: &str| json!({"type": "node_failed", "data": {"unit_id": id}});
    let adv =
        |id: &str, e: &str| json!({"type": "advance_failed", "data": {"node_id": id, "error": e}});
    let ready = [node("ab-x", json!({}))];
    assert!(detect_failure_defers(&ready, &[fail("ab-x"), fail("ab-x")], 3).is_empty());
    let events = [
        fail("ab-x"),
        adv("ab-x", "spawn exited 2: model not in slot"),
        fail("ab-x"),
        fail("ab-x"),
    ];
    let got = detect_failure_defers(&ready, &events, 3);
    assert_eq!(
        (got[0].streak, got[0].error.as_str()),
        (3, "spawn exited 2: model not in slot")
    );
    assert_eq!(
        got[0].reason(),
        "auto-failure: 3 consecutive failed attempts: spawn exited 2: model not in slot"
    );
    for skip in [json!({"status": "idea"}), json!({"deferred_at": "t"})] {
        assert!(detect_failure_defers(&[node("ab-x", skip)], &events, 3).is_empty());
    }
    // An error older than the last reset is not the current streak's cause.
    let reset = [
        adv("ab-x", "stale"),
        json!({"type": "node_undeferred", "data": {"unit_id": "ab-x"}}),
        fail("ab-x"),
    ];
    assert_eq!(last_advance_failed_error("ab-x", &reset), "");
}

#[test]
fn harness_mismatches_split_twins_from_lone_rows_and_recheck_in_the_lock() {
    let row = |h: &str| json!({"session_id": CODEX_ID, "phase": "execute", "harness": h});
    let twinned = node("x-twin", json!({"sessions": [row("codex"), row("claude")]}));
    let lone = node("x-lone", json!({"sessions": [row("claude")]}));
    let silent = node(
        "x-silent",
        json!({"sessions": [
            {"session_id": "20260823T083106Z-cx87209-9d434e", "phase": "execute", "harness": "claude"},
            {"session_id": "20260823T083106Z-cx87209-9d434e", "phase": "execute", "harness": "unknown"},
        ]}),
    );
    let (twins, fixes) = detect_harness_mismatches(&[twinned.clone(), lone.clone(), silent]);
    assert_eq!(ids(&twins, |t| t.node_id.as_str()), ["x-twin"]);
    assert_eq!(ids(&fixes, |f| f.node_id.as_str()), ["x-lone"]);

    let mut skipped = Vec::new();
    let mut ents = vec![twinned.clone(), lone.clone()];
    let claimed: BTreeSet<String> = ["x-twin".to_string(), "x-lone".to_string()].into();
    assert!(apply_twin_drops(&mut ents, &twins, &claimed, &mut skipped).is_empty());
    assert!(apply_harness_shape_fixes(&mut ents, &fixes, &claimed, &mut skipped).is_empty());
    assert_eq!(skipped, ["x-twin", "x-lone"]);

    let free = BTreeSet::new();
    assert_eq!(
        apply_twin_drops(&mut ents, &twins, &free, &mut skipped).len(),
        1
    );
    assert_eq!(ents[0]["sessions"], json!([row("codex")]));
    assert_eq!(
        apply_harness_shape_fixes(&mut ents, &fixes, &free, &mut skipped).len(),
        1
    );
    assert_eq!(ents[1]["sessions"], json!([row("codex")]));
    // The correct twin left since the scan: the drop is stale, nothing leaves.
    let mut stale = vec![node("x-twin", json!({"sessions": [row("claude")]}))];
    assert!(apply_twin_drops(&mut stale, &twins, &free, &mut skipped).is_empty());
    assert_eq!(stale[0]["sessions"], json!([row("claude")]));
}

fn do_node(id: &str, extra: Value) -> Value {
    let mut n = node(
        id,
        json!({"status": "in_progress", "cwd": "/some/worktree", "sessions": [
        {"phase": "execute", "harness": "claude", "session_id": CLAUDE_SID,
         "started_at": "2026-09-09T15:46:29Z"}]}),
    );
    for (k, v) in extra.as_object().unwrap() {
        n[k] = v.clone();
    }
    n
}

#[test]
fn abandoned_rows_split_gone_from_held() {
    let at = |hours: i64| START_MS + hours * 3_600_000;
    let detect = |n: Value, hours: i64, claimed: &[&str], worked: &[(&str, &str)]| {
        let claimed: BTreeSet<String> = claimed.iter().map(|s| s.to_string()).collect();
        let worked: BTreeMap<String, Vec<String>> = worked
            .iter()
            .map(|(n, w)| (n.to_string(), vec![w.to_string()]))
            .collect();
        detect_abandoned_do_rows(&[n], &claimed, &worked, at(hours), 24 * 3600)
            .into_iter()
            .map(|r| (r.verdict, r.reason))
            .collect::<Vec<_>>()
    };
    let one = |v: &'static str, r: &str| vec![(v, r.to_string())];
    assert_eq!(
        detect(do_node("x-1", json!({})), 40, &[], &[]),
        one("gone", "row idle 40h, no reachable worker on the node")
    );
    assert_eq!(
        detect(do_node("x-1", json!({})), 3, &[], &[]),
        one("held", "row idle 3h, inside the 24h bound")
    );
    assert_eq!(
        detect(do_node("x-1", json!({})), 40, &[], &[("x-1", "worker-a")]),
        one("held", "reachable worker on node worker-a")
    );
    assert_eq!(
        detect(
            do_node("x-1", json!({})),
            40,
            &["x-1"],
            &[("x-1", "worker-a")]
        ),
        one("held", "live claim")
    );
    assert!(detect(
        do_node("x-1", json!({"locked_by": "someone"})),
        40,
        &[],
        &[]
    )
    .is_empty());
    // A note the row's own session wrote resets the clock; another's does not.
    let note_at = chrono::DateTime::from_timestamp_millis(at(39))
        .unwrap()
        .to_rfc3339();
    let noted = |sid: &str| {
        do_node(
            "x-1",
            json!({"progress_notes": [{"ts": note_at, "source_session_id": sid}]}),
        )
    };
    assert_eq!(detect(noted(CLAUDE_SID), 40, &[], &[])[0].0, "held");
    assert_eq!(detect(noted("someone-else"), 40, &[], &[])[0].0, "gone");
}

#[test]
fn an_unread_roster_reaps_nothing() {
    let entries = [do_node("x-deg01", json!({}))];
    let blind =
        || -> Result<BTreeMap<String, Vec<String>>, String> { Err("probe timed out".into()) };
    let now = START_MS + 40 * 3_600_000;
    let err = abandoned_do_rows(&entries, &BTreeSet::new(), 24, &blind, now, true).unwrap_err();
    assert_eq!(err, "roster unread (probe timed out)");
    let held = abandoned_do_rows(&entries, &BTreeSet::new(), 24, &blind, now, false).unwrap();
    assert_eq!(
        (held[0].verdict, held[0].reason.as_str()),
        ("held", "roster unread (probe timed out)")
    );
}

// ---------------------------------------------------------------------
// The whole pass over a seeded store
// ---------------------------------------------------------------------

struct Fixture {
    dir: tempfile::TempDir,
    graph: PathBuf,
}

impl Fixture {
    fn new(rows: &[Value]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.json");
        crate::graph_store::seed_rows(&graph, rows).unwrap();
        Fixture { dir, graph }
    }

    fn rows(&self) -> Vec<Value> {
        crate::graph_store::read_rows(&self.graph).unwrap()
    }

    fn row(&self, id: &str) -> Value {
        self.rows()
            .into_iter()
            .find(|r| entry_id(r) == Some(id))
            .unwrap()
    }

    fn history(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

struct Run<'a> {
    settings: Settings,
    workspaces: HashMap<String, String>,
    claims: &'a dyn Fn() -> Result<BTreeSet<String>, String>,
    events: Vec<Value>,
    slugs: HashMap<String, String>,
    analyze: validity::Analyzer<'a>,
}

fn no_claims() -> Result<BTreeSet<String>, String> {
    Ok(BTreeSet::new())
}

fn keep_all(
    packets: &[validity::EvidencePacket],
    _: Option<f64>,
) -> Result<BTreeMap<String, Value>, String> {
    Ok(packets
        .iter()
        .map(|p| {
            (
                p.node_id.clone(),
                json!({"classification": "keep", "confidence": 0.8, "rationale": "stub"}),
            )
        })
        .collect())
}

impl Default for Run<'_> {
    fn default() -> Self {
        Run {
            settings: Settings::default(),
            workspaces: HashMap::new(),
            claims: &no_claims,
            events: Vec::new(),
            slugs: HashMap::new(),
            analyze: &keep_all,
        }
    }
}

impl Run<'_> {
    fn go(&self, fx: &Fixture, args: &[&str]) -> (i32, Out) {
        let tail: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let flags = parse_flags(&tail).unwrap();
        let events = || self.events.clone();
        let slug = |cwd: Option<&str>| cwd.and_then(|c| self.slugs.get(c).cloned());
        let roster = || -> Result<BTreeMap<String, Vec<String>>, String> { Ok(BTreeMap::new()) };
        let no_exists = |_: &Value| -> Option<validity::PathProbe> { None };
        let no_search = |_: &str| -> Option<usize> { None };
        let no_retro = |_: &Value| -> Vec<(String, String)> { Vec::new() };
        let seams = Seams {
            graph: fx.graph.clone(),
            settings: self.settings.clone(),
            workspaces: self.workspaces.clone(),
            worktrees_base: None,
            claims: self.claims,
            events: &events,
            slug: &slug,
            roster: &roster,
            history: Some(fx.dir.path().join("history.jsonl")),
            deck_dir: Some(fx.dir.path().join("validity-decks")),
            sweep: SweepSeams {
                exists_for: &no_exists,
                search: &no_search,
                retro: &no_retro,
                analyze: self.analyze,
            },
        };
        let mut out = Out::captured();
        let code = run_pass(flags, &seams, &mut out);
        (code, out)
    }
}

fn idea(id: &str, days: i64) -> Value {
    node(
        id,
        json!({"status": "idea", "title": format!("Idea {id}"), "created_at": iso_days_ago(days)}),
    )
}

#[test]
fn apply_prunes_rescopes_backfills_and_skips_claimed_nodes() {
    let fx = Fixture::new(&[
        node(
            "ab-keep01",
            json!({"cwd": "/home/u/code/fno", "project": "fno"}),
        ),
        node("ab-leak01", json!({"cwd": "/tmp/pytest-of-x/pytest-1/p"})),
        node("ab-leak02", json!({"cwd": "/tmp/pytest-of-x/pytest-9/p"})),
        node(
            "ab-scope01",
            json!({"project": "fno", "cwd": "/home/u/code/fno/wt/x", "blocked_by": ["ab-leak01"]}),
        ),
        node("ab-url0001", json!({"cwd": "/repo/a", "pr_number": 7})),
        node(
            "ab-url0003",
            json!({"cwd": "/repo/a", "pr_number": 7, "pr_url": "https://github.com/other/repo/pull/7"}),
        ),
    ]);
    let claims = || -> Result<BTreeSet<String>, String> { Ok(["ab-leak02".to_string()].into()) };
    let run = Run {
        workspaces: [("fno".to_string(), "/home/u/code/fno".to_string())].into(),
        claims: &claims,
        slugs: [("/repo/a".to_string(), "o/a".to_string())].into(),
        ..Run::default()
    };
    let (code, out) = run.go(&fx, &["--apply", "--no-validity"]);
    assert_eq!(code, 0, "{}", out.stderr);
    let ids: BTreeSet<String> = fx
        .rows()
        .iter()
        .filter_map(|r| entry_id(r).map(str::to_string))
        .collect();
    assert!(!ids.contains("ab-leak01") && ids.contains("ab-leak02"));
    let scoped = fx.row("ab-scope01");
    assert_eq!(scoped["cwd"], "/home/u/code/fno");
    assert!(
        scoped["blocked_by"].as_array().is_none_or(Vec::is_empty),
        "dangling ref cleaned"
    );
    assert_eq!(
        fx.row("ab-url0001")["pr_url"],
        "https://github.com/o/a/pull/7"
    );
    assert_eq!(
        fx.row("ab-url0003")["pr_url"],
        "https://github.com/other/repo/pull/7"
    );
    assert!(out
        .stdout
        .contains("pr-url written 1 of 1 | pr-url unresolvable 0"));
    assert!(out.stdout.contains("re-scoped 1 | pruned 1 |"));
    assert!(out.stdout.contains("skipped-claimed 1"));
    let report = fx.history().last().unwrap()["report"].clone();
    assert_eq!(
        (report["applied"].clone(), report["complete"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(report["incomplete_leg"], Value::Null);

    // Idempotent: the second pass finds nothing left to do.
    let (_, again) = run.go(&fx, &["--apply", "--json", "--no-validity"]);
    let payload: Value = serde_json::from_str(&again.stdout).unwrap();
    assert_eq!(payload["rescope"]["candidates"], json!([]));
    assert_eq!(payload["pr_url_backfill"]["applied"], json!([]));
}

#[test]
fn apply_refuses_when_claim_state_is_unreadable() {
    let fx = Fixture::new(&[node(
        "ab-leak03",
        json!({"cwd": "/tmp/pytest-of-x/pytest-9/p"}),
    )]);
    let blind = || -> Result<BTreeSet<String>, String> { Err("claims unavailable".into()) };
    let run = Run {
        claims: &blind,
        ..Run::default()
    };
    let (code, out) = run.go(&fx, &["--apply"]);
    assert_eq!(code, 1);
    assert!(out.stderr.contains("live claim state is unavailable"));
    assert_eq!(fx.rows().len(), 1, "a refused apply mutates nothing");
    // A report degrades to no claims rather than refusing.
    assert_eq!(run.go(&fx, &["--no-validity"]).0, 0);
}

#[test]
fn judgment_legs_only_propose_even_under_apply() {
    let old = iso_days_ago(99);
    let fx = Fixture::new(&[
        node(
            "ab-dup01",
            json!({"status": "idea", "title": "Same idea", "slug": "same-idea", "created_at": old}),
        ),
        node(
            "ab-dup02",
            json!({"status": "idea", "title": "same  idea!", "slug": "same-idea-b", "created_at": old}),
        ),
    ]);
    let (code, out) = Run::default().go(&fx, &["--apply", "--json", "--no-validity"]);
    assert_eq!(code, 0, "{}", out.stderr);
    let payload: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(payload["dedup_groups"], json!([["ab-dup01", "ab-dup02"]]));
    assert_eq!(payload["stale_ideas"].as_array().unwrap().len(), 2);
    assert_eq!(payload["complete"], true);
    assert!(fx.rows().iter().all(|r| !truthy(r.get("deferred_at"))));
    // Every category prints, zero included.
    let (_, human) = Run::default().go(&fx, &["--no-validity"]);
    assert!(human
        .stdout
        .contains("re-scope candidates 0 | prune candidates 0"));
    assert!(human
        .stdout
        .contains("pr-url proposed 0 | pr-url unresolvable 0"));
}

#[test]
fn a_budget_overrun_exits_4_with_a_partial_receipt_and_row() {
    let fx = Fixture::new(&[node("ab-keep03", json!({"cwd": "/home/u/code/fno"}))]);
    let mut run = Run::default();
    run.settings.budget_seconds = 1;
    // A budget already spent dies at the first leg boundary.
    let flags = Flags::default();
    let mut out = Out::captured();
    let mut pass = Pass {
        out: &mut out,
        budget: Budget {
            deadline: Some(Instant::now() - std::time::Duration::from_secs(1)),
        },
        started: Instant::now(),
        legs_done: vec![("rescope".into(), "0".into())],
        flags,
        history: Some(fx.dir.path().join("history.jsonl")),
    };
    assert_eq!(pass.enter("temp-leaks"), Err(4));
    assert!(out.stderr.contains(
        "budget exceeded in leg 'temp-leaks' after 0.0s; 1/14 legs completed; results partial"
    ));
    assert!(out.stdout.contains("  rescope: 0\n"));
    assert!(!out.stdout.contains("temp-leaks:"));
    let row = &fx.history()[0]["report"];
    assert_eq!(
        (row["complete"].clone(), row["incomplete_leg"].clone()),
        (json!(false), json!("temp-leaks"))
    );
    // An unbounded budget never trips.
    assert!(!Budget::new(0).expired() && Budget::new(0).remaining().is_none());
}

#[test]
fn failure_streaks_defer_at_the_threshold_under_apply_only() {
    let ready = |id: &str| node(id, json!({"plan_path": format!("plans/{id}.md")}));
    let fail = |id: &str| json!({"type": "node_failed", "data": {"unit_id": id}});
    let parked =
        |id: &str| json!({"type": "node_closed", "data": {"unit_id": id, "close": "parked"}});
    let fx = Fixture::new(&[ready("ab-fail01"), ready("ab-fail02")]);
    let run = Run {
        events: vec![
            fail("ab-fail01"),
            parked("ab-fail01"),
            fail("ab-fail01"),
            fail("ab-fail02"),
            fail("ab-fail02"),
        ],
        ..Run::default()
    };
    let (_, dry) = run.go(&fx, &["--json", "--no-validity"]);
    let payload: Value = serde_json::from_str(&dry.stdout).unwrap();
    assert_eq!(
        payload["auto_defer"]["candidates"],
        json!([{"node_id": "ab-fail01", "streak": 3}])
    );
    assert!(!truthy(fx.row("ab-fail01").get("deferred_at")));

    let (code, out) = run.go(&fx, &["--apply", "--no-validity"]);
    assert_eq!(code, 0, "{}", out.stderr);
    let deferred = fx.row("ab-fail01");
    assert_eq!(
        deferred["deferred_reason"],
        "auto-failure: 3 consecutive failed attempts"
    );
    assert_eq!(deferred["status"], "deferred");
    assert!(
        !truthy(fx.row("ab-fail02").get("deferred_at")),
        "N-1 failures stay ready"
    );
    assert!(out
        .stdout
        .contains("auto-deferred ab-fail01 (3 consecutive failures)"));

    // The blast cap defers at most ten and names the rest.
    let many: Vec<Value> = (0..12).map(|i| ready(&format!("ab-mass{i:02}"))).collect();
    let fx = Fixture::new(&many);
    let run = Run {
        events: many
            .iter()
            .flat_map(|n| vec![fail(entry_id(n).unwrap()); 3])
            .collect(),
        ..Run::default()
    };
    let (_, out) = run.go(&fx, &["--apply", "--no-validity"]);
    assert_eq!(
        fx.rows()
            .iter()
            .filter(|r| truthy(r.get("deferred_at")))
            .count(),
        10
    );
    assert!(out.stdout.contains("blast cap hit - 2 further"));
}

#[test]
fn the_now_cap_counts_live_epic_promoted_children_not_progress() {
    let fx = Fixture::new(&[
        node("ab-epic01", json!({"type": "epic", "priority": "p1"})),
        node(
            "ab-child1",
            json!({"priority": "p2", "parent": "ab-epic01"}),
        ),
    ]);
    let mut run = Run::default();
    run.settings.now_wip_cap = 1;
    let (_, out) = run.go(&fx, &["--json", "--no-validity"]);
    let payload: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(payload["now_overflow"], json!([2, 1]));

    let fx = Fixture::new(&[
        node(
            "ab-claim01",
            json!({"status": "in_progress", "priority": "p1"}),
        ),
        node(
            "ab-claim02",
            json!({"status": "in_progress", "priority": "p1"}),
        ),
    ]);
    let (_, out) = run.go(&fx, &["--json", "--no-validity"]);
    let payload: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(payload["now_overflow"], Value::Null);
}

#[test]
fn the_abandoned_leg_settles_a_gone_row_and_holds_a_fresh_one() {
    let stamp = |hours: i64| {
        chrono::DateTime::from_timestamp_millis(now_ms() - hours * 3_600_000)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    };
    let row = |sid: &str, hours: i64| json!([{"phase": "execute", "harness": "claude", "session_id": sid, "started_at": stamp(hours)}]);
    let fx = Fixture::new(&[
        node(
            "ab-gone01",
            json!({"status": "in_progress", "cwd": "/repo/x", "sessions": row(CLAUDE_SID, 72)}),
        ),
        node(
            "ab-held01",
            json!({"status": "in_progress", "cwd": "/repo/x",
                                 "sessions": row("aa5b6c93-1111-4222-8333-444455556666", 1)}),
        ),
    ]);
    let (code, out) = Run::default().go(&fx, &["--apply", "--no-validity"]);
    assert_eq!(code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("settled do row ab-gone01"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("row_closed true"));
    assert!(out.stdout.contains("row idle 1h, inside the 24h bound"));
    let gone = fx.row("ab-gone01");
    assert!(
        truthy(gone["sessions"][0].get("ended_at")),
        "the row is filled and kept"
    );
    assert!(!truthy(fx.row("ab-held01")["sessions"][0].get("ended_at")));
}

#[test]
fn the_stale_receipt_is_bounded_to_the_ten_oldest() {
    let fx = Fixture::new(
        &(0..12)
            .map(|i| idea(&format!("ab-s{i:02}"), 60 + i))
            .collect::<Vec<_>>(),
    );
    let (_, out) = Run::default().go(&fx, &["--no-validity"]);
    assert!(out.stdout.contains("stale ideas: 12 (age 60-71d)"));
    assert!(out.stdout.contains("(showing 10 of 12 oldest)"));
    assert!(
        out.stdout.contains("ab-s11")
            && !out.stdout.contains("ab-s00 ")
            && !out.stdout.contains("ab-s01 ")
    );
    assert!(out
        .stdout
        .contains("| xargs fno backlog defer -R 'stale >30d, drained by maintain' --kind expired"));
    let fx = Fixture::new(&[idea("ab-fresh", 0)]);
    let (_, out) = Run::default().go(&fx, &["--no-validity"]);
    assert!(out.stdout.contains("  stale ideas: 0") && !out.stdout.contains("xargs"));
}

#[test]
fn validity_reviews_once_watermarks_and_rechecks_on_demand() {
    let fx = Fixture::new(&[idea("ab-wm1", 90), idea("ab-fresh", 5)]);
    let run = Run::default();
    let (code, out) = run.go(&fx, &["--apply"]);
    assert_eq!(code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains("validity: reviewed 1 ideas -> promote 0 | keep 1"),
        "{}",
        out.stdout
    );
    assert!(
        !truthy(fx.row("ab-wm1").get("deferred_at")),
        "proposal-only under --apply"
    );
    assert!(run
        .go(&fx, &[])
        .1
        .stdout
        .contains("validity: 0 eligible ideas"));
    assert!(run
        .go(&fx, &["--recheck"])
        .1
        .stdout
        .contains("reviewed 1 ideas"));
    let (_, skipped) = run.go(&fx, &["--no-validity"]);
    assert!(!skipped.stdout.contains("validity:"));

    let failing = |_: &[validity::EvidencePacket],
                   _: Option<f64>|
     -> Result<BTreeMap<String, Value>, String> { Err("analyzer down".into()) };
    let fx = Fixture::new(&[idea("ab-d1", 91)]);
    let (_, out) = Run {
        analyze: &failing,
        ..Run::default()
    }
    .go(&fx, &[]);
    assert!(out.stdout.contains("(DEGRADED: analyzer unavailable)"));
}

#[test]
fn flags_follow_the_verb_contract() {
    assert_eq!(parse_flags(&["--bogus".to_string()]).unwrap_err(), 2);
    let f = parse_flags(&["-J".into(), "--apply".into(), "--suspect-reverts".into()]).unwrap();
    assert!(f.json && f.apply && f.suspect_reverts && !f.recheck);
    assert!(is_drain_reason("stale >7d, drained by maintain"));
    assert!(!is_drain_reason("stale >d, drained by maintain"));
}
