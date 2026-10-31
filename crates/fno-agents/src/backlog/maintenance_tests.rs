use super::*;

// 2026-09-09T15:46:29Z, the do-row fixtures' start.
const START_MS: i64 = 1_788_968_789_000;
const CODEX_ID: &str = "01a06886-9405-74a1-8afd-5b67baf89604";
const CLAUDE_SID: &str = "9f06a492-1111-4222-8333-444455556666";

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
fn stale_ideas_obey_the_drain_law() {
    let idea = |id: &str, priority: Option<&str>, days: i64| {
        let mut extra = json!({"status": "idea", "created_at": iso_days_ago(days)});
        if let Some(priority) = priority {
            extra["priority"] = json!(priority);
        }
        node(id, extra)
    };
    let fx = Fixture::new(&[
        idea("ab-p0old", Some("p0"), 99),
        idea("ab-p1old", Some("p1"), 99),
        idea("ab-p2edge", Some("p2"), 60),
        idea("ab-p2past", Some("p2"), 61),
        idea("ab-p3old", Some("p3"), 99),
        idea("ab-unset", None, 99),
    ]);
    let (code, out) = Run::default().go(&fx, &["--json", "--no-validity"]);
    assert_eq!(code, 0, "{}", out.stderr);
    let payload: Value = serde_json::from_str(&out.stdout).unwrap();
    let mut stale: Vec<&str> = payload["stale_ideas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["node_id"].as_str().unwrap())
        .collect();
    stale.sort_unstable();
    assert_eq!(stale, ["ab-p2past", "ab-p3old", "ab-unset"]);
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
