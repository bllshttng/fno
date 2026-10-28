use super::*;

const NOW: i64 = 1_800_000_000_000;

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .expect("valid ms")
        .to_rfc3339()
}

fn row(kind: &str, ms: i64, data: Value) -> Value {
    json!({"ts": stamp(ms), "type": kind, "data": data})
}

fn entry_row(pr: u64, head: &str, ms: i64, priority: u8, repair: bool) -> Value {
    row(
        ENTRY_EVENT,
        ms,
        json!({"slug": "o/r", "pr": pr, "head": head, "head_ref": format!("b{pr}"),
            "priority": priority, "repair": repair}),
    )
}

fn entry(pr: u64, priority: u8, ts_ms: i64) -> Entry {
    Entry {
        pr,
        head: format!("h{pr}"),
        head_ref: format!("b{pr}"),
        repair: false,
        priority,
        ts_ms,
    }
}

fn idle_state() -> State {
    fold(&[], DEFAULT_MAX_BATCH)
}

fn batch(prs: &[u64], base: &str) -> Batch {
    Batch {
        id: "1".into(),
        prs: prs.to_vec(),
        sha: "batch-sha".into(),
        base: base.into(),
        repair: false,
        reran: false,
        since_ms: NOW,
    }
}

fn with_active(b: Batch) -> State {
    State {
        active: Some(b),
        ..idle_state()
    }
}

fn step(entries: &[Entry], state: &State, main: &str, main_red: bool, gate: &Gate) -> Step {
    let flakes = HashSet::from(["plugin-tree".to_string()]);
    decide(&View {
        entries,
        state,
        main,
        main_red,
        gate,
        flakes: &flakes,
        now_ms: NOW + 60_000,
    })
}

#[test]
fn a_green_queue_builds_in_priority_order_then_lands_the_batch_sha() {
    let entries = [entry(3, 2, 30), entry(1, 1, 50), entry(2, 2, 10)];
    let built = step(&entries, &idle_state(), "m0", false, &Gate::Pending);
    assert_eq!(built, Step::Build { prs: vec![1, 2, 3] });
    let state = with_active(batch(&[1, 2, 3], "m0"));
    assert_eq!(
        step(&entries, &state, "m0", false, &Gate::Pending),
        Step::Wait
    );
    assert_eq!(
        step(&entries, &state, "m0", false, &Gate::Green),
        Step::Land {
            sha: "batch-sha".into()
        }
    );
}

#[test]
fn a_batch_never_holds_more_than_the_cap() {
    let entries: Vec<Entry> = (1..=8).map(|pr| entry(pr, 2, pr as i64)).collect();
    let Step::Build { prs } = step(&entries, &idle_state(), "m0", false, &Gate::Pending) else {
        panic!("expected a build");
    };
    assert_eq!(prs, vec![1, 2, 3, 4, 5, 6]);
}

/// The node's done line: two PRs, each green alone, whose combination fails
/// the gate. The queue lands exactly one and ejects the other.
#[test]
fn two_green_prs_whose_combination_fails_land_only_one() {
    let entry_rows = [
        entry_row(1, "h1", NOW, 2, false),
        entry_row(2, "h2", NOW + 1, 2, false),
    ];
    let mut open = HashMap::from([(1, "h1".to_string()), (2, "h2".to_string())]);
    let (mut steps, mut on_main): (Vec<Value>, Vec<u64>) = (Vec::new(), Vec::new());
    let flakes = HashSet::new();
    for tick in 0..20 {
        let ms = NOW + 10 + tick;
        let state = fold(&steps, DEFAULT_MAX_BATCH);
        let entries = live_entries(&entry_rows, &state, &open);
        let main = format!("m{}", on_main.len());
        // Red only when main plus the batch carries both PRs.
        let gate = match &state.active {
            Some(b) => {
                let all: HashSet<u64> = on_main.iter().chain(&b.prs).copied().collect();
                if all.contains(&1) && all.contains(&2) {
                    Gate::Red {
                        failing: vec!["guards".into()],
                    }
                } else {
                    Gate::Green
                }
            }
            None => Gate::Pending,
        };
        let view = View {
            entries: &entries,
            state: &state,
            main: &main,
            main_red: false,
            gate: &gate,
            flakes: &flakes,
            now_ms: ms,
        };
        let id = state
            .active
            .as_ref()
            .map(|b| b.id.clone())
            .unwrap_or_default();
        let data = match decide(&view) {
            Step::Idle => break,
            Step::Build { prs } => json!({"step": "build", "batch": ms.to_string(), "prs": prs,
                "sha": format!("s{ms}"), "base": main}),
            Step::Land { .. } => {
                let prs = state
                    .active
                    .as_ref()
                    .map(|b| b.prs.clone())
                    .unwrap_or_default();
                for pr in &prs {
                    open.remove(pr);
                }
                on_main.extend(&prs);
                json!({"step": "land", "batch": id, "prs": prs})
            }
            Step::Split { left, right } => {
                json!({"step": "split", "batch": id, "left": left, "right": right})
            }
            Step::Eject { pr, .. } => json!({"step": "eject", "batch": id, "pr": pr,
                "head": format!("h{pr}"), "ends_batch": true}),
            other => panic!("unexpected step {other:?}"),
        };
        steps.push(row(STEP_EVENT, ms, data));
    }
    assert_eq!(on_main, vec![1]);
    let words: Vec<String> = steps.iter().map(|r| text(r, "/data/step")).collect();
    assert_eq!(words, ["build", "split", "build", "land", "build", "eject"]);
}

#[test]
fn a_red_batch_of_known_flakes_reruns_once_then_splits() {
    let entries = [entry(1, 2, 1), entry(2, 2, 2)];
    let red = Gate::Red {
        failing: vec!["plugin-tree".into()],
    };
    let state = with_active(batch(&[1, 2], "m0"));
    assert_eq!(step(&entries, &state, "m0", false, &red), Step::Rerun);
    let mut reran = batch(&[1, 2], "m0");
    reran.reran = true;
    assert_eq!(
        step(&entries, &with_active(reran), "m0", false, &red),
        Step::Split {
            left: vec![1],
            right: vec![2]
        }
    );
}

#[test]
fn a_red_main_builds_only_the_repair_entry_alone() {
    let mut repair = entry(9, 3, 99);
    repair.repair = true;
    let entries = [entry(1, 0, 1), repair, entry(2, 1, 2)];
    assert_eq!(
        step(&entries, &idle_state(), "m0", true, &Gate::Pending),
        Step::Build { prs: vec![9] }
    );
    assert_eq!(
        step(&entries[..1], &idle_state(), "m0", true, &Gate::Pending),
        Step::Idle
    );
    // A green main still lands the repair first and alone.
    assert_eq!(
        step(&entries, &idle_state(), "m0", false, &Gate::Pending),
        Step::Build { prs: vec![9] }
    );
}

#[test]
fn main_moving_under_a_built_batch_rebuilds_and_ejects_nobody() {
    let entries = [entry(1, 2, 1), entry(2, 2, 2)];
    let state = with_active(batch(&[1, 2], "m0"));
    assert_eq!(
        step(&entries, &state, "m1", false, &Gate::Green),
        Step::Rebuild { main_moved: true }
    );
}

#[test]
fn three_rebuilds_in_a_row_hold_until_main_stays_still() {
    let rebuild = |ms| {
        row(
            STEP_EVENT,
            ms,
            json!({"step": "rebuild", "batch": "1", "main_moved": true}),
        )
    };
    let build = |ms, base: &str| {
        row(
            STEP_EVENT,
            ms,
            json!({"step": "build", "batch": "1", "prs": [1], "sha": "s", "base": base}),
        )
    };
    let entries = [entry(1, 2, 1)];
    let rows = vec![
        build(1, "m0"),
        rebuild(2),
        build(3, "m1"),
        rebuild(4),
        build(5, "m2"),
        rebuild(6),
        build(7, "m3"),
    ];
    let state = fold(&rows, DEFAULT_MAX_BATCH);
    assert_eq!(state.rebuilds, 3);
    let held = step(&entries, &state, "m4", false, &Gate::Pending);
    assert_eq!(
        held,
        Step::Hold {
            reason: "main_moving".into()
        }
    );
    let mut rows = rows;
    rows.push(row(
        STEP_EVENT,
        8,
        json!({"step": "hold", "batch": "1", "main": "m4"}),
    ));
    let state = fold(&rows, DEFAULT_MAX_BATCH);
    assert_eq!(state.rebuilds, 0);
    assert!(matches!(
        step(&entries, &state, "m5", false, &Gate::Pending),
        Step::Hold { .. }
    ));
    assert_eq!(
        step(&entries, &state, "m4", false, &Gate::Pending),
        Step::Build { prs: vec![1] }
    );
}

#[test]
fn an_entry_whose_head_moved_drops_out_and_the_batch_builds_without_it() {
    let rows = [
        entry_row(1, "h1", 1, 2, false),
        entry_row(2, "h2", 2, 2, false),
    ];
    let open = HashMap::from([(1, "h1".to_string()), (2, "pushed-again".to_string())]);
    let entries = live_entries(&rows, &idle_state(), &open);
    assert_eq!(entries.iter().map(|e| e.pr).collect::<Vec<_>>(), vec![1]);
    assert_eq!(
        step(&entries, &idle_state(), "m0", false, &Gate::Pending),
        Step::Build { prs: vec![1] }
    );
    // The same move under a built batch restarts it, not counted as main moving.
    let state = with_active(batch(&[1, 2], "m0"));
    assert_eq!(
        step(&entries, &state, "m0", false, &Gate::Green),
        Step::Rebuild { main_moved: false }
    );
}

#[test]
fn an_ejected_head_never_requeues_but_a_new_push_does() {
    let rows = [
        entry_row(1, "h1", 1, 2, false),
        entry_row(1, "h1b", 5, 2, false),
    ];
    let steps = [row(
        STEP_EVENT,
        3,
        json!({"step": "eject", "batch": "1", "pr": 1, "head": "h1"}),
    )];
    let state = fold(&steps, DEFAULT_MAX_BATCH);
    let old = HashMap::from([(1, "h1".to_string())]);
    assert!(live_entries(&rows, &state, &old).is_empty());
    let new = HashMap::from([(1, "h1b".to_string())]);
    assert_eq!(live_entries(&rows, &state, &new)[0].head, "h1b");
}

#[test]
fn an_unlisted_gate_run_waits_through_the_dispatch_grace_then_reruns() {
    let entries = [entry(1, 2, 1)];
    let mut fresh = batch(&[1], "m0");
    fresh.since_ms = NOW;
    assert_eq!(
        step(
            &entries,
            &with_active(fresh.clone()),
            "m0",
            false,
            &Gate::Missing
        ),
        Step::Wait
    );
    fresh.since_ms = NOW - DISPATCH_GRACE_MS;
    assert_eq!(
        step(&entries, &with_active(fresh), "m0", false, &Gate::Missing),
        Step::Rerun
    );
}

#[test]
fn reusable_workflow_job_names_match_the_flake_ledger() {
    assert_eq!(check_name("gate / plugin-tree"), "plugin-tree");
    assert_eq!(check_name("guards"), "guards");
}
