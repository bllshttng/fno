use super::*;
use chrono::TimeZone;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 24, 0, 0, 0).unwrap()
}

fn ctx() -> RenderCtx {
    RenderCtx {
        session_id: None,
        promoted: false,
        cwd_name: "a".to_string(),
        now: now(),
    }
}

fn sources(dir: &Path) -> Sources {
    let root = dir.join("a");
    std::fs::create_dir_all(root.join(".fno")).unwrap();
    Sources {
        root,
        questions: dir.join("questions.jsonl"),
        lane: dir.join("lane.md"),
        graph: dir.join("graph.db"),
        backend: "graph".to_string(),
        sidecar_dir: dir.join("sidecar"),
        home: None,
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn the_json_contract_keeps_every_key_in_order_and_names_each_root() {
    let o = Outstanding {
        carveout_total: 3,
        carveout_by_kind: vec![("oos-bug".into(), 2), ("deferred".into(), 1)],
        carveout_oldest_ts: Some("2026-07-14T04:27:55Z".into()),
        carveout_root: Some("/r".into()),
        questions: vec![Question {
            id: "q1".into(),
            ts: "t".into(),
            question: "?".into(),
            options: vec!["a".into()],
            live: Some(true),
            ..Default::default()
        }],
        questions_store: "/q".into(),
        captures: vec![
            Capture {
                fu_id: "fu-a".into(),
                title: "A".into(),
                project: "p".into(),
                added_at: None,
            },
            Capture {
                fu_id: "fu-b".into(),
                title: "B".into(),
                project: "p".into(),
                added_at: Some("t".into()),
            },
        ],
        capture_file_total: 2,
        capture_row_total: 3,
        lane: vec![LaneRow {
            text: "top".into(),
            line: 4,
        }],
        lane_parked: 1,
        lane_store: "/l".into(),
        verdicts: vec![Verdict {
            node: "x-1".into(),
            report: "/rep".into(),
            verdict: "FAIL".into(),
            claim: "c".into(),
            status: None,
            mtime: None,
        }],
        verdicts_error: None,
    };
    let expected = concat!(
        r#"{"roots":{"carveouts":{"scope":"project","root":"/r"},"questions":{"scope":"machine","store":"/q"},"#,
        r#""captures":{"scope":"machine","files_read":2},"lane":{"scope":"machine","store":"/l"}},"#,
        r#""carveouts":{"total":3,"by_kind":{"oos-bug":2,"deferred":1},"oldest_ts":"2026-07-14T04:27:55Z"},"#,
        r#""questions":[{"id":"q1","ts":"t","question":"?","session_id":null,"cwd":null,"node":null,"asker":null,"ask":null,"options":["a"],"blocks":[],"subject":null,"live":true,"host":null,"rank":1}],"#,
        r#""captures":{"total":2,"resolved_files":2,"parsed_open_rows":3,"repeated_ids":1,"by_project":{"p":2},"#,
        r#""items":[{"id":"fu-a","title":"A","project":"p","added_at":null},{"id":"fu-b","title":"B","project":"p","added_at":"t"}]},"#,
        r#""lane":{"total":1,"parked":1,"items":[{"text":"top","line":4}]},"#,
        r#""verdicts":{"total":1,"error":null,"items":[{"node":"x-1","report":"/rep","verdict":"FAIL","claim":"c","status":null,"mtime":null}]}}"#,
    );
    assert_eq!(o.as_json().to_string(), expected);
}

#[test]
fn a_missing_store_reads_empty_and_an_unreadable_one_is_a_failed_read() {
    let tmp = tempfile::tempdir().unwrap();
    let src = sources(tmp.path());
    let o = collect(&src, Ok(Vec::new())).unwrap();
    assert!(o.empty(), "no ledger, no index, no lane reads as nothing");
    assert_eq!(render(&o, &ctx()), "");

    std::fs::create_dir_all(&src.questions).unwrap();
    let err = collect(&src, Ok(Vec::new())).unwrap_err();
    assert!(err.contains("questions.jsonl"), "{err}");

    std::fs::remove_dir(&src.questions).unwrap();
    std::fs::write(
        src.root.join(".fno/carveouts.jsonl"),
        b"\xff\xfe not utf-8\n",
    )
    .unwrap();
    let err = collect(&src, Ok(Vec::new())).unwrap_err();
    assert!(err.contains("cannot read carve-out ledger"), "{err}");
}

#[test]
fn questions_fold_closes_away_and_rank_live_first_then_newest() {
    let ask = |id: &str, ts: &str, asker: Option<&str>, blocks: &[&str]| {
        json!({"type": QUESTION_EVENT, "ts": ts, "data": {
            "question_id": id, "question": "?", "asker": asker, "blocks": blocks}})
        .to_string()
    };
    let raw = [
        ask("q1", "2026-07-20T00:00:00Z", Some("alice"), &[]),
        ask("q2", "2026-07-21T00:00:00Z", None, &[]),
        ask("q3", "2026-07-19T00:00:00Z", Some("bob"), &[]),
        ask("q4", "2026-07-22T00:00:00Z", Some("carol"), &[]),
        json!({"type": QUESTION_CLOSED_EVENT, "data": {"question_id": "q4"}}).to_string(),
        ask("q5", "2026-07-21T00:00:00Z", Some("dave"), &["x-1"]),
        "{operator_question not json".to_string(),
    ]
    .join("\n");
    let mut qs = fold_questions(&raw);
    let ids: Vec<&str> = qs.iter().map(|q| q.id.as_str()).collect();
    assert_eq!(
        ids,
        ["q5", "q2", "q1", "q3"],
        "newest first, more blocks first on a tie"
    );
    let answers = HashMap::from([("alice".to_string(), true), ("bob".to_string(), false)]);
    apply_liveness(&mut qs, &answers);
    let ranked: Vec<(&str, Option<bool>)> = qs.iter().map(|q| (q.id.as_str(), q.live)).collect();
    assert_eq!(
        ranked,
        [
            ("q1", Some(true)),
            ("q5", None),
            ("q2", Some(false)),
            ("q3", Some(false)),
        ]
    );
}

#[test]
fn the_capture_leg_folds_every_project_dedupes_ids_and_ages_from_capture_events() {
    let tmp = tempfile::tempdir().unwrap();
    let mut src = sources(tmp.path());
    let (a, b, c) = (src.root.clone(), tmp.path().join("b"), tmp.path().join("c"));
    write(
        &a.join(".fno/backlog/inbox.md"),
        "- [ ] fu-aa1 - first thing (p2)\n- [x] fu-aa2 - promoted\n- [ ] fu-shared - shared\n",
    );
    write(
        &b.join(".fno/backlog/inbox.md"),
        "- [ ] fu-bb1 \u{2014} second thing\n- [ ] fu-shared - shared\n",
    );
    // An unreadable inbox is one project less, never a failure.
    std::fs::create_dir_all(c.join(".fno").join("backlog").join("parking-lot.md")).unwrap();
    let event = |fu: &str, ts: &str| {
        json!({"type": "capture_add", "ts": ts, "data": {"fu_id": fu}}).to_string() + "\n"
    };
    write(
        &project_log(&a, "events.jsonl"),
        &event("fu-aa1", "2026-07-10T00:00:00Z"),
    );
    write(
        &project_log(&b, "events.jsonl"),
        &event("fu-bb1", "2026-07-04T00:00:00Z"),
    );
    src.backend = "linear".to_string();
    write(
        &src.sidecar_dir.join("b.json"),
        &json!({"cwd": b.display().to_string()}).to_string(),
    );
    write(
        &src.sidecar_dir.join("c.json"),
        &json!({"source_cwd": c.display().to_string()}).to_string(),
    );

    let o = collect(&src, Ok(Vec::new())).unwrap();
    let ids: Vec<&str> = o.captures.iter().map(|c| c.fu_id.as_str()).collect();
    assert_eq!(
        ids,
        ["fu-bb1", "fu-aa1", "fu-shared"],
        "oldest known age first"
    );
    assert_eq!((o.capture_file_total, o.capture_row_total), (2, 4));
    assert_eq!(o.captures[1].title, "first thing");
    let out = render(&o, &ctx());
    for want in [
        "3 captures awaiting triage (fu- items).",
        "  fu-bb1  second thing  (b)\n",
        "  fu-aa1  first thing\n",
        "  Showing 3 of 3, oldest 20 days. Count rule: 3 unique open fu-* IDs across 2 resolved capture files (4 parsed open rows minus 1 repeated ID);",
        "Triage with: fno backlog triage",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
}
