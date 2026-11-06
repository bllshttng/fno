use super::*;

// 2026-09-09T15:46:29Z, the do-row fixtures' start.
const START_MS: i64 = 1_788_968_789_000;
const CLAUDE_SID: &str = "9f06a492-1111-4222-8333-444455556666";

fn node(id: &str, extra: Value) -> Value {
    let mut n = json!({"id": id, "title": id, "status": "ready"});
    for (k, v) in extra.as_object().unwrap() {
        n[k] = v.clone();
    }
    n
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
