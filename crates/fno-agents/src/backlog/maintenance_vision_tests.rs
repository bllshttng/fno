use super::maintenance_vision::run_vision_sweep;
use super::*;

fn vrow(id: &str, extra: Value) -> Value {
    let mut n = json!({
        "id": id,
        "slug": format!("slug-{id}"),
        "title": id,
        "status": "deferred",
        "priority": "p2",
        "type": "feature",
        "details": "a deferred row the sweep reads",
    });
    for (k, v) in extra.as_object().unwrap() {
        n[k] = v.clone();
    }
    n
}

fn vfx(rows: &[Value]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let graph = dir.path().join("graph.json");
    crate::graph_store::seed_rows(&graph, rows).unwrap();
    (dir, graph)
}

fn fx_rows(_dir: &tempfile::TempDir, graph: &std::path::Path) -> Vec<Value> {
    crate::graph_store::read_rows(graph).unwrap()
}

/// Build the seams inside one scope (the closures borrow locals) and run
/// one sweep invocation with the given flags.
fn sweep_run(
    graph: std::path::PathBuf,
    dir: &tempfile::TempDir,
    grade: &dyn Fn(&str, &Path) -> Option<(String, String)>,
    args: &[&str],
    entries: &[Value],
) -> (i32, Out) {
    let events = || -> Vec<Value> { Vec::new() };
    let slug = |_: Option<&str>| -> Option<String> { None };
    let roster = || -> Result<BTreeMap<String, Vec<String>>, String> { Ok(BTreeMap::new()) };
    let no_exists = |_: &Value| -> Option<validity::PathProbe> { None };
    let no_search = |_: &str| -> Option<usize> { None };
    let no_retro = |_: &Value| -> Vec<(String, String)> { Vec::new() };
    let claims = || -> Result<BTreeSet<String>, String> { Ok(BTreeSet::new()) };
    let seams = Seams {
        graph,
        settings: Settings::default(),
        workspaces: HashMap::new(),
        worktrees_base: None,
        claims: &claims,
        events: &events,
        slug: &slug,
        roster: &roster,
        history: Some(dir.path().join("history.jsonl")),
        deck_dir: Some(dir.path().join("validity-decks")),
        history_dir: Some(dir.path().join("history")),
        vision_grade: grade,
        sweep: SweepSeams {
            exists_for: &no_exists,
            search: &no_search,
            retro: &no_retro,
            analyze: &|packets: &[validity::EvidencePacket], _: Option<f64>| {
                Ok(packets
                    .iter()
                    .map(|p| (p.node_id.clone(), json!({"classification": "keep"})))
                    .collect())
            },
        },
    };
    let tail: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let flags = parse_flags(&tail).unwrap();
    let mut out = Out::captured();
    let code = run_vision_sweep(flags, &seams, &mut out, entries);
    (code, out)
}

#[test]
fn vision_sweep_reports_ranks_and_defers_only_under_apply() {
    let grade_fail = |_: &str, _: &Path| Some(("fail".to_string(), "off-mission".to_string()));
    let (dir, graph) = vfx(&[
        vrow(
            "ab-swp01",
            json!({"details": "unrelated spreadsheet plumbing"}),
        ),
        vrow(
            "ab-swp02",
            json!({"deferred_kind": "wont_do", "details": "already dead"}),
        ),
        vrow("ab-swp03", json!({"details": "another orphan backlog row"})),
        vrow(
            "ab-swp04",
            json!({"status": "ready", "details": "live work"}),
        ),
    ]);
    let (code, out) = sweep_run(
        graph.clone(),
        &dir,
        &grade_fail,
        &["--vision-sweep"],
        &fx_rows(&dir, &graph),
    );
    assert_eq!(code, 0);
    let stdout = &out.stdout;
    assert!(stdout.contains("2 deferred rows scored"), "{stdout}");
    assert!(stdout.contains("2 on the kill list"), "{stdout}");
    assert!(
        stdout.contains("ab-swp01") && stdout.contains("ab-swp03"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("ab-swp02"),
        "closing kind excluded: {stdout}"
    );
    assert!(
        !stdout.contains("ab-swp04"),
        "ready row untouched: {stdout}"
    );
    let artifact_count = std::fs::read_dir(dir.path().join("history"))
        .unwrap()
        .count();
    assert_eq!(artifact_count, 1, "one kill-list artifact lands");
    let rows = fx_rows(&dir, &graph);
    let swp01 = rows.iter().find(|r| r["id"] == "ab-swp01").unwrap();
    assert!(
        swp01.get("deferred_kind").is_none() || swp01["deferred_kind"].is_null(),
        "report mode defers nothing"
    );
}

#[test]
fn vision_sweep_apply_defers_exactly_the_judged_list() {
    let grade_fail = |_: &str, _: &Path| Some(("fail".to_string(), "off-mission".to_string()));
    let (dir, graph) = vfx(&[vrow("ab-swp01", json!({})), vrow("ab-swp03", json!({}))]);
    // The report run lands the approval surface.
    let (code, _out) = sweep_run(
        graph.clone(),
        &dir,
        &grade_fail,
        &["--vision-sweep"],
        &fx_rows(&dir, &graph),
    );
    assert_eq!(code, 0);
    // Apply reads the newest artifact and never re-grades.
    let (code2, out2) = sweep_run(
        graph.clone(),
        &dir,
        &grade_fail,
        &["--vision-sweep", "--apply"],
        &fx_rows(&dir, &graph),
    );
    assert_eq!(code2, 0);
    let applied = &out2.stdout;
    assert!(applied.contains("apply: applying"), "{applied}");
    let rows = fx_rows(&dir, &graph);
    for id in ["ab-swp01", "ab-swp03"] {
        let row = rows
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("row {id} missing"));
        assert_eq!(row["deferred_kind"], "wont_do");
        let reason = row["deferred_reason"].as_str().unwrap_or("");
        assert!(
            reason.starts_with("vision kill list ") && reason.ends_with(", batch approved"),
            "{reason}"
        );
    }
}

#[test]
fn vision_sweep_skip_and_unjudged_never_defer() {
    // No vision source: the skip line, no grading, no artifact. Apply with
    // no artifact on disk defers nothing.
    let (dir, graph) = vfx(&[vrow("ab-swp05", json!({}))]);
    let grade_none = |_: &str, _: &Path| -> Option<(String, String)> { None };
    let cwd_probe = std::env::current_dir().unwrap();
    if crate::blueprint_judge::vision_source_text(&[], &cwd_probe).is_some() {
        // The dev machine carries no project vision; skip this leg when
        // that changes so the fixture stays hermetic.
        return;
    }
    let (code, out) = sweep_run(
        graph.clone(),
        &dir,
        &grade_none,
        &["--vision-sweep"],
        &fx_rows(&dir, &graph),
    );
    assert_eq!(code, 0);
    let skipped = &out.stdout;
    assert!(skipped.contains("skipped (no vision source"), "{skipped}");

    let (dir2, graph2) = vfx(&[vrow("ab-swp06", json!({}))]);
    let (code2, out2) = sweep_run(
        graph2.clone(),
        &dir2,
        &grade_none,
        &["--vision-sweep", "--apply"],
        &fx_rows(&dir2, &graph2),
    );
    assert_eq!(code2, 0);
    let refused = &out2.stdout;
    assert!(refused.contains("no kill list artifact"), "{refused}");
    let rows = fx_rows(&dir2, &graph2);
    let swp06 = rows.iter().find(|r| r["id"] == "ab-swp06").unwrap();
    assert!(
        swp06.get("deferred_kind").is_none() || swp06["deferred_kind"].is_null(),
        "unjudged rows never defer"
    );
}
