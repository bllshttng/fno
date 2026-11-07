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

fn vseams<'a>(
    graph: std::path::PathBuf,
    dir: &'a tempfile::TempDir,
    grade: &'a dyn Fn(&str, &Path) -> Option<(String, String)>,
) -> Seams<'a> {
    let events = || Vec::new();
    let slug = |_: Option<&str>| -> Option<String> { None };
    let roster = || -> Result<BTreeMap<String, Vec<String>>, String> { Ok(BTreeMap::new()) };
    let no_exists = |_: &Value| -> Option<validity::PathProbe> { None };
    let no_search = |_: &str| -> Option<usize> { None };
    let no_retro = |_: &Value| -> Vec<(String, String)> { Vec::new() };
    Seams {
        graph,
        settings: Settings::default(),
        workspaces: HashMap::new(),
        worktrees_base: None,
        claims: &|| Ok(BTreeSet::new()),
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
    }
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
    let seams = vseams(graph, &dir, &grade_fail);
    let mut out = Out::captured();
    let flags = parse_flags(&["--vision-sweep".to_string()]).unwrap();
    let code = run_vision_sweep(flags, &seams, &mut out, &fx_rows(&dir, &graph));
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
    let seams = vseams(graph, &dir, &grade_fail);
    let mut out = Out::captured();
    let flags = parse_flags(&["--vision-sweep".to_string(), "--apply".to_string()]).unwrap();
    let code = run_vision_sweep(flags, &seams, &mut out, &fx_rows(&dir, &graph));
    assert_eq!(code, 0);
    let rows = fx_rows(&dir, &graph);
    for id in ["ab-swp01", "ab-swp03"] {
        let row = rows
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("row {id} missing"));
        assert_eq!(row["deferred_kind"], "wont_do");
        assert_eq!(row["deferred_reason"], "vision kill list , batch approved");
    }
}

#[test]
fn vision_sweep_skip_and_unjudged_never_defer() {
    // No vision source: the skip line, no grading, no artifact. The
    // grader answering None reads as a skipped grade, never a fail, so
    // apply defers nothing.
    let (dir, graph) = vfx(&[vrow("ab-swp05", json!({}))]);
    let grade_none = |_: &str, _: &Path| -> Option<(String, String)> { None };
    let seams = vseams(graph, &dir, &grade_none);
    let cwd_probe = std::env::current_dir().unwrap();
    if crate::blueprint_judge::vision_source_text(&[], &cwd_probe).is_some() {
        // The dev machine carries no project vision; skip this leg when
        // that changes so the fixture stays hermetic.
        return;
    }
    let mut out = Out::captured();
    let flags = parse_flags(&["--vision-sweep".to_string()]).unwrap();
    let code = run_vision_sweep(flags, &seams, &mut out, &fx_rows(&dir, &graph));
    assert_eq!(code, 0);
    assert!(
        out.stdout.contains("skipped (no vision source"),
        "{out.stdout}"
    );

    let (dir2, graph2) = vfx(&[vrow("ab-swp06", json!({}))]);
    let seams2 = vseams(graph2, &dir2, &grade_none);
    let mut out2 = Out::captured();
    let flags2 = parse_flags(&["--vision-sweep".to_string(), "--apply".to_string()]).unwrap();
    let code2 = run_vision_sweep(flags2, &seams2, &mut out2, &fx_rows(&dir2, &graph2));
    assert_eq!(code2, 0);
    assert!(out2.stdout.contains("0 on the kill list"), "{out2.stdout}");
    let rows = fx_rows(&dir2, &graph2);
    let swp06 = rows.iter().find(|r| r["id"] == "ab-swp06").unwrap();
    assert!(
        swp06.get("deferred_kind").is_none() || swp06["deferred_kind"].is_null(),
        "unjudged rows never defer"
    );
}
