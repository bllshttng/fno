//! The retirement sweep, reap-receipt gate and plan_reconcile test
//! families. Shared helpers (`tmp_home`, `ask_row`, `rentry`, `civil`, ...)
//! stay in the parent tests module and resolve through the glob.
use super::*;
use crate::daemon::claude_stop::stop_claude_pid_confirmed;

use crate::gc_sweep::{self, GcSummary, GraphRead};
use crate::quiet_retire::no_live_worker;

// ── x-c672: the retirement sweep, keyed by the reverse join ─────────────

/// A transcript file untouched for `age_secs`, so the activity read answers
/// a positive quiet past the grace.
pub(super) fn quiet_transcript(
    dir: &std::path::Path,
    name: &str,
    age_secs: i64,
) -> std::path::PathBuf {
    use std::io::Write;
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "{{}}").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mtime = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_secs((now - age_secs).max(0) as u64);
    f.set_modified(mtime).unwrap();
    path
}

/// The test age seam (x-54cf): the sweep reads ages through the injected
/// batch seam, and the fixtures answer from the SAME staged transcript files
/// the old stat read - the seam is what changed, not the fixture ages. `now`
/// is injected so a dry pass and its acting pass judge one clock and their
/// ages agree byte for byte; wall-clock call sites pass
/// `crate::daemon::now_epoch_secs()`.
pub(super) fn staged_ages_at(
    now: i64,
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
) -> impl Fn(&[&state::RegistryEntry]) -> std::collections::HashMap<String, Option<i64>> + '_ {
    move |entries| {
        entries
            .iter()
            .map(|e| {
                let age = transcripts(e).and_then(|hits| {
                    hits.iter()
                        .filter_map(|p| {
                            let secs = std::fs::metadata(p).ok()?.modified().ok()?;
                            Some(secs.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
                        })
                        .max()
                });
                (
                    crate::gc::row_handle(e),
                    age.map(|newest| now.saturating_sub(newest)),
                )
            })
            .collect()
    }
}

/// The single-fixture age seam: one staged age answers every row the sweep
/// probes, the shape most call sites stage (`quiet_transcript(..., 2*3600)`).
pub(super) fn uniform_ages(
    age_s: i64,
) -> impl Fn(&[&state::RegistryEntry]) -> std::collections::HashMap<String, Option<i64>> {
    move |entries| {
        entries
            .iter()
            .map(|e| (crate::gc::row_handle(e), Some(age_s)))
            .collect()
    }
}

/// Build the injected graph seam from `(session, node, status)` triples and
/// `(session, node)` open-do pairs.
pub(super) fn graph_read(
    named: &[(&str, &str, &str)],
    open_do: &[(&str, &str)],
) -> Option<GraphRead> {
    let mut index: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();
    let mut statuses: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut pr_state: std::collections::HashMap<String, (Option<String>, usize, usize)> =
        std::collections::HashMap::new();
    for (sid, node, status) in named {
        index
            .entry(sid.to_ascii_lowercase())
            .or_default()
            .push((node.to_string(), status.to_string()));
        statuses.insert(node.to_string(), status.to_string());
        pr_state.insert(node.to_string(), (None, 0, 0));
    }
    let mut open: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for (slurp, node) in open_do {
        open.entry(slurp.to_ascii_lowercase())
            .or_default()
            .push(node.to_string());
    }
    Some(GraphRead {
        work_index: index.clone(),
        index,
        open_do: open,
        phases: std::collections::HashMap::new(),
        closed_planning: std::collections::HashMap::new(),
        plan_written: std::collections::HashMap::new(),
        statuses,
        pr_state,
        pr_number: std::collections::HashMap::default(),
        plan_paths: std::collections::HashMap::default(),
        delivery_merged_refs: std::collections::HashMap::default(),
        do_nodes: std::collections::HashMap::default(),
        pr_reads: std::collections::HashMap::default(),
    })
}

/// A retirement-shaped sweep for the dispatch-accounting tests: every row
/// named on a done node, its staged transcript quiet (grace 0), stop
/// confirmed, no tree.

pub(super) fn no_agents() -> crate::claude_roster::ClaudeAgentsSnapshot {
    crate::claude_roster::ClaudeAgentsSnapshot::unknown("test: no snapshot staged")
}

fn retire_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    named: &[(&str, &str, &str)],
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
) -> GcSummary {
    let graph = graph_read(named, &[]);
    gc_sweep::run(
        home,
        emitter,
        0,
        false,
        7,
        &move |_| graph.clone(),
        transcripts,
        &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
    )
}

/// The sweep against staged seams: graph as staged, transcripts as staged,
/// stop always confirmed, tree probes staged per name, prune recorded.
fn staged_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    grace_secs: i64,
    graph: Option<GraphRead>,
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
    trees: &dyn Fn(&str) -> (Option<bool>, Option<bool>),
) -> GcSummary {
    gc_sweep::run(
        home,
        emitter,
        grace_secs,
        false,
        7,
        &move |_| graph.clone(),
        transcripts,
        &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|e| trees(&e.name),
        &|e| Some(crate::daemon::PruneOutcome::Removed(e.cwd.clone())),
    )
}

/// AC4-HP, the epic's VERIFICATION three-row marker. Row A is named on a
/// done node and owns a clean linked worktree: it retires with its basis,
/// its tree is pruned, and the branch-shaped protections (transcript on
/// disk, no graph mutation) hold. Row B is named on one done and one
/// open node: kept, naming the open node. Row C is named nowhere and
/// exited: the registry's terminal stamp is its finished marker, so it
/// retires past the idle hour like a reported-done row.
#[test]
fn ac4_hp_three_row_marker_retires_prunes_and_names_every_keep() {
    let home = tmp_home("gc-ac4");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let a_path = quiet_transcript(transcripts.path(), "a.jsonl", 2 * 3600);
    // B is kept as open-work stale: its age must sit inside the origin
    // ruling's one idle hour, not merely past the old grace.
    let b_path = quiet_transcript(transcripts.path(), "b.jsonl", 1800);
    let c_path = quiet_transcript(transcripts.path(), "c.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut a = ask_row("row-a", None);
        a.short_id = "rowa".into();
        a.harness = Some("claude".into());
        a.harness_session_id = Some("sess-a".into());
        a.origin = Some("spawn".into());
        // A hosted worker, not a one-shot ask: the row owns its worktree.
        a.host_mode = Some(state::HOST_MODE_INTERACTIVE.into());
        // A linked worktree: `.git` is a file.
        let wt = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wt.path()).unwrap();
        std::fs::write(wt.path().join(".git"), "gitdir: /x/worktrees/a\n").unwrap();
        a.cwd = wt.keep().to_string_lossy().into_owned();
        r.entries.push(a);
        let mut b = ask_row("row-b", None);
        b.short_id = "rowb".into();
        b.harness_session_id = Some("sess-b".into());
        b.origin = Some("spawn".into());
        r.entries.push(b);
        let mut c = ask_row("row-c", None);
        c.short_id = "rowc".into();
        c.harness_session_id = Some("sess-c".into());
        c.origin = Some("spawn".into());
        r.entries.push(c);
    })
    .unwrap();

    let summary = staged_sweep(
        &home,
        &emitter,
        900,
        graph_read(
            &[
                ("sess-a", "N1", "done"),
                ("sess-b", "N2", "done"),
                ("sess-b", "N3", "in_review"),
            ],
            &[],
        ),
        &|e| match e.harness_session_id.as_deref() {
            Some("sess-a") => Some(vec![a_path.clone()]),
            Some("sess-b") => Some(vec![b_path.clone()]),
            Some("sess-c") => Some(vec![c_path.clone()]),
            _ => None,
        },
        &|name| {
            // A's tree: clean and merged; B and C own nothing removable.
            let _ = name;
            (Some(true), Some(true))
        },
    );

    assert_eq!(
        summary.retired,
        vec![
            (
                "rowa".to_string(),
                "every named node done: N1 (via sessions; merge_status: N1:unrecorded)".to_string()
            ),
            ("rowc".to_string(), "done".to_string()),
        ],
        "{:?}",
        summary.retired
    );
    assert_eq!(summary.pruned.len(), 1, "{:?}", summary.pruned);
    assert_eq!(
        summary.kept_open_work_stale,
        vec![(
            "rowb".to_string(),
            "N3".to_string(),
            "in_review".to_string(),
            "sessions".to_string()
        )]
    );
    assert!(
        summary.kept_no_provenance.is_empty(),
        "{:?}",
        summary.kept_no_provenance
    );
    // The retired vocabulary: none of the exit-stamp words appears.
    let rendered = format!("{summary:?}");
    for word in [
        "exited_at",
        "not-terminal",
        "contradicted",
        "within-grace",
        "uncorroborated",
        "backstop",
    ] {
        assert!(!rendered.contains(word), "{word} leaked: {rendered}");
    }
    // The receipt survived (the resumable handle), and the other rows stayed.
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.entries.iter().all(|e| e.name != "row-a"));
    assert!(reg.entries.iter().any(|e| e.name == "row-b"));
    assert!(reg.entries.iter().all(|e| e.name != "row-c"));
    let receipts = home.root().join("reap-receipts");
    assert!(
        receipts.join("claude-sess-a.json").exists(),
        "the resume handle must be durable before the row leaves"
    );
    assert!(
        !summary
            .kept_no_receipt
            .iter()
            .any(|(_, reason)| reason.contains("registry write refused")),
        "{:?}",
        summary.kept_no_receipt
    );

    // AC4-EDGE: touch A's transcript and rerun as a fresh home - the same
    // row shape with a fresh transcript is ACTIVE, nothing retires.
    let home2 = tmp_home("gc-ac4-active");
    let emitter2 = EventEmitter::new(home2.events_jsonl(), "daemon");
    let fresh = quiet_transcript(transcripts.path(), "a-fresh.jsonl", 10);
    state::update_registry(&home2.registry_json(), |r| {
        let mut a = ask_row("row-a", None);
        a.short_id = "rowa".into();
        a.harness_session_id = Some("sess-a".into());
        a.origin = Some("spawn".into());
        r.entries.push(a);
    })
    .unwrap();
    let summary = staged_sweep(
        &home2,
        &emitter2,
        900,
        graph_read(&[("sess-a", "N1", "done")], &[]),
        &|_| Some(vec![fresh.clone()]),
        &|_| (Some(true), Some(true)),
    );
    let [(row, age)] = summary.kept_active.as_slice() else {
        panic!("expected one kept-active row: {:?}", summary.kept_active);
    };
    assert_eq!(row, "rowa");
    // The transcript was stamped now-10s; a loaded runner measures the write
    // a second or two later, so pin the RANGE (fresh, inside the 900s grace),
    // never the wall-clock instant.
    assert!(
        *age >= 10 && *age < 900,
        "age {age} should read fresh, got {summary:?}"
    );
    assert!(summary.retired.is_empty());
    assert!(summary.pruned.is_empty());
    let reg = state::load_registry(&home2.registry_json()).unwrap();
    assert!(reg.entries.iter().any(|e| e.name == "row-a"));
}

/// A clean-and-merged worktree the callback could not remove reads as a
/// prune ATTEMPT, not a prune SUCCESS: `Kept` never populates `pruned`, and
/// `None` (no linked worktree) reads as its own reason. Neither ever
/// duplicates into the other bucket.
#[test]
fn a_prune_that_did_not_confirm_removal_is_never_reported_pruned() {
    let home = tmp_home("gc-prune-outcome");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "p.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut p = ask_row("row-p", None);
        p.short_id = "rowp".into();
        p.harness_session_id = Some("sess-p".into());
        p.origin = Some("spawn".into());
        p.host_mode = Some(state::HOST_MODE_INTERACTIVE.into());
        let wt = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wt.path()).unwrap();
        std::fs::write(wt.path().join(".git"), "gitdir: /x/worktrees/p\n").unwrap();
        p.cwd = wt.keep().to_string_lossy().into_owned();
        r.entries.push(p);
    })
    .unwrap();
    let graph = graph_read(&[("sess-p", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| {
            Some(crate::daemon::PruneOutcome::Kept(
                "git worktree remove failed: simulated".into(),
            ))
        },
    );
    assert_eq!(
        summary.retired,
        vec![(
            "rowp".to_string(),
            "every named node done: N1 (via sessions; merge_status: N1:unrecorded)".to_string()
        )]
    );
    assert!(summary.pruned.is_empty(), "{:?}", summary.pruned);
    assert_eq!(
        summary.prune_failed,
        vec![(
            "rowp".to_string(),
            "git worktree remove failed: simulated".to_string()
        )]
    );
}

/// A shared worktree cwd: one row retires, the other is still live on it -
/// the tree survives and the retiring row names the row still holding it.
#[test]
fn a_shared_worktree_survives_while_the_other_row_is_live() {
    let home = tmp_home("gc-shared-tree-one-live");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "x.jsonl", 2 * 3600);
    let wt = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(wt.path()).unwrap();
    std::fs::write(wt.path().join(".git"), "gitdir: /x/worktrees/shared\n").unwrap();
    let shared_cwd = wt.keep().to_string_lossy().into_owned();
    state::update_registry(&home.registry_json(), |r| {
        let mut x = ask_row("row-x", None);
        x.short_id = "rowx".into();
        x.harness_session_id = Some("sess-x".into());
        x.origin = Some("spawn".into());
        x.host_mode = Some(state::HOST_MODE_INTERACTIVE.into());
        x.cwd = shared_cwd.clone();
        r.entries.push(x);
        // row-y stays: named on an open node, still live on the same tree.
        let mut y = ask_row("row-y", None);
        y.short_id = "rowy".into();
        y.harness_session_id = Some("sess-y".into());
        y.origin = Some("spawn".into());
        y.host_mode = Some(state::HOST_MODE_INTERACTIVE.into());
        y.cwd = shared_cwd.clone();
        r.entries.push(y);
    })
    .unwrap();
    let graph = graph_read(
        &[("sess-x", "N1", "done"), ("sess-y", "N2", "in_review")],
        &[],
    );
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        // The ruling's hour splits the pair: the done row is past it, the
        // live row sits inside it, so only the done row retires.
        &|entries| {
            entries
                .iter()
                .map(|e| {
                    let age = if e.short_id == "rowy" { 1800 } else { 2 * 3600 };
                    (crate::gc::row_handle(e), Some(age))
                })
                .collect()
        },
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| {
            Some(crate::daemon::PruneOutcome::Removed(
                "must not be called".into(),
            ))
        },
    );
    assert_eq!(
        summary.retired,
        vec![(
            "rowx".to_string(),
            "every named node done: N1 (via sessions; merge_status: N1:unrecorded)".to_string()
        )]
    );
    assert!(summary.pruned.is_empty(), "{:?}", summary.pruned);
    assert_eq!(
        summary.kept_shared_tree,
        vec![("rowx".to_string(), "rowy".to_string())]
    );
}

/// A parent row named as `spawned_by_session` by a live CHILD child (a
/// joiner-named row) is never retired, and no active-surface removal ever
/// reaches it: `surface_removal` panics if the sweep calls it.
#[test]
fn a_parent_with_a_live_descendant_is_kept_and_never_touched() {
    let home = tmp_home("gc-lineage-live-child");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "parent.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut parent = ask_row("row-parent", None);
        parent.short_id = "rowparent".into();
        parent.harness_session_id = Some("sess-parent".into());
        parent.origin = Some("spawn".into());
        r.entries.push(parent);
        let mut child = ask_row("jn-t-row-child", None);
        child.short_id = "rowchild".into();
        child.harness_session_id = Some("sess-child".into());
        child.spawned_by_session = Some("sess-parent".into());
        child.status = crate::AgentStatus::Busy;
        r.entries.push(child);
    })
    .unwrap();
    let graph = graph_read(&[("sess-parent", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| panic!("active-surface removal must never run on a held parent"),
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert!(summary.retired.is_empty(), "{:?}", summary.retired);
    assert_eq!(
        summary.kept_live_descendants,
        vec![("rowparent".to_string(), "rowchild".to_string())]
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.entries.iter().any(|e| e.name == "row-parent"));
}

/// AC4-ERR, both arms: an unreadable graph keeps every row (never a
/// retirement on a failed read), and a stop that does not confirm keeps
/// the row under `stop_refused`.
#[test]
fn ac4_err_graph_unreadable_and_stop_refusal_keep_every_row() {
    let home = tmp_home("gc-ac4-err");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    state::update_registry(&home.registry_json(), |r| {
        for name in ["row-a", "row-b"] {
            let mut e = ask_row(name, None);
            e.harness_session_id = Some(format!("sess-{name}"));
            e.origin = Some("spawn".into());
            // The verdict door reads the stored status first; these scenarios vary
            // the vendor word, so the row itself must be fno-undecided.
            e.status = crate::AgentStatus::Idle;
            r.entries.push(e);
        }
    })
    .unwrap();

    // Graph unreadable: every row keeps under that one reason.
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &|_| None,
        &|_| None,
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert!(summary.retired.is_empty());
    assert_eq!(
        summary.kept_graph_unreadable,
        vec!["row-a".to_string(), "row-b".to_string()]
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert_eq!(reg.entries.len(), 2);

    // Stop refused: the row earned its retirement but the process could
    // not be confirmed stopped, so it stays for the next tick.
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "q.jsonl", 2 * 3600);
    let graph = graph_read(&[("sess-row-a", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &|e| e.name != "row-a",
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    // row-b names no node and sits idle past the hour, so it retires under
    // the origin ruling; row-a's refused stop keeps it.
    assert_eq!(
        summary
            .retired
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["row-b"]
    );
    assert_eq!(
        summary
            .stop_refused
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        vec!["row-a".to_string()]
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.entries.iter().any(|e| e.name == "row-a"));
}

/// Locked Decision 1, one table, two rows: every named node done but one
/// carrying an OPEN do row for this session keeps the row and names the
/// node, and the same row with its do entry closed (absent from
/// open_do, the seam model of a closed row) retires by name with
/// kept_open_do_row empty. The exact (short_id, node) identity must
/// land on each side; a count assertion alone would pass on an unrelated
/// row. The sweep never settles graph rows on a retirement.
#[test]
fn an_open_do_row_on_a_done_node_holds_the_retirement_and_a_closed_one_retires_by_name() {
    for (open_do, expect_retired) in [(&[("sess-d", "N1")][..], false), (&[][..], true)] {
        let home = tmp_home("gc-open-do");
        let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
        let transcripts = tempfile::tempdir().unwrap();
        // A held row sits inside the idle hour; past it a spawn row retires.
        let age = if expect_retired { 2 * 3600 } else { 1800 };
        let quiet = quiet_transcript(transcripts.path(), "d.jsonl", age);
        state::update_registry(&home.registry_json(), |r| {
            let mut e = ask_row("row-d", None);
            e.short_id = "rowd".into();
            e.harness_session_id = Some("sess-d".into());
            e.origin = Some("spawn".into());
            r.entries.push(e);
        })
        .unwrap();
        let graph = graph_read(&[("sess-d", "N1", "done")], open_do);
        let summary = gc_sweep::run(
            &home,
            &emitter,
            900,
            false,
            7,
            &move |_| graph.clone(),
            &move |_| Some(vec![quiet.clone()]),
            &uniform_ages(age),
            &|_| true,
            &|_| crate::daemon::CascadeOutcome::NotApplicable,
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &no_agents,
            &|_| (Some(true), Some(true)),
            &|_| None,
        );
        if expect_retired {
            assert_eq!(
                summary.retired,
                vec![(
                    "rowd".to_string(),
                    "every named node done: N1 (via sessions; merge_status: N1:unrecorded)"
                        .to_string()
                )]
            );
            assert!(summary.kept_open_do_row.is_empty());
            let reg = state::load_registry(&home.registry_json()).unwrap();
            assert!(!reg.entries.iter().any(|e| e.name == "row-d"));
        } else {
            assert!(summary.retired.is_empty());
            assert_eq!(
                summary.kept_open_do_row,
                vec![("rowd".to_string(), "N1".to_string())]
            );
            let reg = state::load_registry(&home.registry_json()).unwrap();
            assert!(reg.entries.iter().any(|e| e.name == "row-d"));
        }
    }
}

/// The origin and team protections, and the tree buckets on a retired row.
#[test]
fn operator_and_teamed_rows_never_retire_and_tree_buckets_only_keep_trees() {
    let home = tmp_home("gc-protect");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let q1 = quiet_transcript(transcripts.path(), "o.jsonl", 2 * 3600);
    let q2 = quiet_transcript(transcripts.path(), "k.jsonl", 2 * 3600);
    let q3 = quiet_transcript(transcripts.path(), "t.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut o = ask_row("row-o", None);
        o.short_id = "rowo".into();
        o.harness_session_id = Some("sess-o".into());
        o.origin = Some("operator".into());
        r.entries.push(o);
        let mut k = ask_row("row-k", None);
        k.short_id = "rowk".into();
        k.harness_session_id = Some("sess-k".into());
        k.origin = Some("spawn".into());
        k.role_level = Some(1);
        r.entries.push(k);
        let mut t = ask_row("row-t", None);
        t.short_id = "rowt".into();
        t.harness_session_id = Some("sess-t".into());
        t.origin = Some("spawn".into());
        t.host_mode = Some(state::HOST_MODE_INTERACTIVE.into());
        let wt = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wt.path()).unwrap();
        std::fs::write(wt.path().join(".git"), "gitdir: /x/worktrees/t\n").unwrap();
        t.cwd = wt.keep().to_string_lossy().into_owned();
        r.entries.push(t);
    })
    .unwrap();
    let graph = graph_read(
        &[
            ("sess-o", "N1", "done"),
            ("sess-k", "N1", "done"),
            ("sess-t", "N1", "done"),
        ],
        &[],
    );
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |e| match e.harness_session_id.as_deref() {
            Some("sess-o") => Some(vec![q1.clone()]),
            Some("sess-k") => Some(vec![q2.clone()]),
            _ => Some(vec![q3.clone()]),
        },
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        // row-t's tree: dirty. The row retires; the tree stays and is named.
        &|e| {
            if e.name == "row-t" {
                (Some(false), None)
            } else {
                (Some(true), Some(true))
            }
        },
        &|_| None,
    );
    assert_eq!(summary.kept_operator, vec!["rowo".to_string()]);
    assert_eq!(summary.kept_teamed, vec!["rowk".to_string()]);
    assert_eq!(
        summary
            .retired
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        vec!["rowt".to_string()],
        "a dirty tree never pins the row"
    );
    assert_eq!(summary.kept_dirty.len(), 1, "{:?}", summary.kept_dirty);
    assert!(summary.pruned.is_empty());
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.entries.iter().all(|e| e.name != "row-t"));
    assert!(reg.entries.iter().any(|e| e.name == "row-o"));
    assert!(reg.entries.iter().any(|e| e.name == "row-k"));
}

// ── x-b150: the reap receipt gate ────────────────────────────────────────

fn write_receipt_at(home: &AgentsHome, name: &str, reaped_at: &str) -> std::path::PathBuf {
    let dir = home.root().join("reap-receipts");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let receipt = serde_json::json!({
        "row_name": "t-expiry",
        "short_id": "texpiry",
        "harness": "claude",
        "harness_session_id": "expiry-sess",
        "cwd": "/tmp",
        "created_at": "2020-01-01T00:00:00Z",
        "reaped_at": reaped_at,
        "resume": "claude --resume expiry-sess",
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    path
}

/// x-70e1 AC6: the active-surface removal is a REAL effect of the sweep. A
/// typed outcome that does not confirm (Failed) HOLDS the row for retry, and
/// a confirming outcome is recorded on the staged receipt as an
/// `active-surface` effect - a partial retirement is never readable as a
/// full one.
#[test]
fn a_failed_surface_removal_holds_the_row_and_a_confirmation_is_recorded() {
    use crate::daemon::CascadeOutcome;

    let home = tmp_home("gc-surface-hold");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let dir = home.root().join("store");
    std::fs::create_dir_all(&dir).unwrap();
    let quiet = quiet_transcript(&dir, "q.jsonl", 2 * 3600);
    let graph = graph_read(&[("s-hold", "N1", "done")], &[]);
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "holdw".into();
        e.short_id = "holdw".into();
        e.origin = Some("spawn".into());
        e.harness = Some("codex".into());
        e.harness_session_id = Some("s-hold".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();

    // First pass: the surface removal FAILS. The row holds; nothing drops.
    let refusing =
        |_e: &state::RegistryEntry| CascadeOutcome::Failed("claude rm timed out".to_string());
    let q1 = quiet.clone();
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![q1.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &refusing,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert!(summary.retired.is_empty(), "{:?}", summary.retired);
    assert!(
        summary
            .stop_refused
            .iter()
            .any(|(id, reason)| id == "holdw" && reason.contains("active-surface")),
        "{:?}",
        summary.stop_refused
    );
    assert!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .iter()
            .any(|e| e.name == "holdw"),
        "a refused surface removal holds the row"
    );

    // Second pass: the removal confirms. The row retires and the receipt on
    // disk carries the active-surface effect record. The staged values are
    // re-cloned for the second closure set.
    let confirming = |e: &state::RegistryEntry| {
        let _ = e;
        CascadeOutcome::Removed
    };
    let graph = graph_read(&[("s-hold", "N1", "done")], &[]);
    let quiet2 = quiet.clone();
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet2.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &confirming,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert_eq!(summary.retired.len(), 1, "{:?}", summary.retired);
    let receipts: Vec<String> = std::fs::read_dir(home.root().join("reap-receipts"))
        .unwrap()
        .flatten()
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();
    assert!(!receipts.is_empty(), "a receipt was staged");
    let body = std::fs::read_to_string(&receipts[0]).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let effects = value["effects"].as_array().expect("effects recorded");
    let by_op: std::collections::BTreeMap<String, String> = effects
        .iter()
        .map(|e| {
            (
                e["op"].as_str().unwrap_or_default().to_string(),
                e["outcome"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        by_op.get("native-stop").map(String::as_str),
        Some("confirmed-removed"),
        "the confirmed stop is named before the surface removal: {by_op:?}"
    );
    assert_eq!(
        by_op.get("active-surface").map(String::as_str),
        Some("confirmed-removed"),
        "the native active-surface outcome must be named: {by_op:?}"
    );
    assert!(
        by_op.contains_key("resume-evidence"),
        "the resumability evidence op must be present: {by_op:?}"
    );
}

/// x-5aef AC3-EDGE: a row whose receipt cannot be built refuses BEFORE any
/// effect fires - no stop seam call, no surface seam call, no receipt on
/// disk, row kept under kept_no_receipt.
#[test]
fn a_row_without_a_buildable_receipt_refuses_before_any_effect() {
    use crate::daemon::CascadeOutcome;

    let home = tmp_home("gc-no-receipt-refuses-first");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let dir = home.root().join("store");
    std::fs::create_dir_all(&dir).unwrap();
    let quiet = quiet_transcript(&dir, "q.jsonl", 2 * 3600);
    let graph = graph_read(&[("s-norc", "N1", "done")], &[]);
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "norcw".into();
        e.short_id = "norcw".into();
        e.origin = Some("spawn".into());
        // A harness with no capability row builds no receipt: the Unknown
        // case in build_reap_receipt's gate.
        e.harness = Some("harness-no-contract".into());
        e.harness_session_id = Some("s-norc".into());
        e.created_at = "2026-01-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();
    let stop_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let surface_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let stop_for_seam = std::rc::Rc::clone(&stop_calls);
    let surface_for_seam = std::rc::Rc::clone(&surface_calls);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &move |_| {
            stop_for_seam.set(stop_for_seam.get() + 1);
            true
        },
        &move |_| {
            surface_for_seam.set(surface_for_seam.get() + 1);
            CascadeOutcome::Removed
        },
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert!(summary.retired.is_empty(), "{:?}", summary.retired);
    assert_eq!(
        stop_calls.get(),
        0,
        "no effect may fire when the receipt cannot be built"
    );
    assert_eq!(
        surface_calls.get(),
        0,
        "no effect may fire when the receipt cannot be built"
    );
    assert!(
        summary.kept_no_receipt.iter().any(|(id, _)| id == "norcw"),
        "{:?}",
        summary.kept_no_receipt
    );
    assert!(
        !home.root().join("reap-receipts").exists()
            || std::fs::read_dir(home.root().join("reap-receipts"))
                .map(|entries| entries.count() == 0)
                .unwrap_or(true),
        "no receipt may land for a row the gate refused before staging"
    );
}

/// x-5aef AC6-EDGE at the stage seam: a session whose native transcript
/// cannot be located records `resume-evidence` as `failed`. The row still
/// retires (the session is already stopped), and the receipt is what the
/// verifier will refuse rather than certify.
#[test]
fn a_row_without_a_located_transcript_records_failed_resume_evidence() {
    use crate::daemon::CascadeOutcome;

    let home = tmp_home("gc-resume-evidence-failed");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let dir = home.root().join("store");
    std::fs::create_dir_all(&dir).unwrap();
    let quiet = quiet_transcript(&dir, "q.jsonl", 2 * 3600);
    let graph = graph_read(
        &[("s-nores-0000-0000-0000-000000000000", "N1", "done")],
        &[],
    );
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "noresw".into();
        e.short_id = "noresw".into();
        e.origin = Some("spawn".into());
        e.harness = Some("codex".into());
        e.harness_session_id = Some("s-nores-0000-0000-0000-000000000000".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();
    let confirming = |_e: &state::RegistryEntry| CascadeOutcome::Removed;
    let q1 = quiet.clone();
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![q1.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &confirming,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert_eq!(summary.retired.len(), 1, "{:?}", summary.retired);
    let receipts: Vec<std::path::PathBuf> = std::fs::read_dir(home.root().join("reap-receipts"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(receipts.len(), 1, "{receipts:?}");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&receipts[0]).unwrap()).unwrap();
    let effects = value["effects"].as_array().expect("effects recorded");
    let resume = effects
        .iter()
        .find(|e| e["op"] == "resume-evidence")
        .expect("resume-evidence recorded");
    assert_eq!(
        resume["outcome"], "failed",
        "no located transcript: the op must read failed, not confirmed: {effects:?}"
    );
}

/// x-5aef AC4-HP/AC4-EDGE over the PRODUCTION graph read: two bp- workers,
/// both named on a node at `ready`, both quiet past grace. Only the one
/// whose own blueprint row carries `ended_at` retires; the other keeps with
/// the unclosed node named. This is the quiet-replanner trap: a previous
/// blueprint moved the node to ready, and the completion must not be
/// inherited by a worker that never closed its assignment.
#[test]
fn a_planner_retires_only_on_its_own_closed_assignment() {
    use crate::daemon::CascadeOutcome;

    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let planning_row = |sid: &str, ended: Option<&str>| {
        let mut row = json!({
            "phase": "blueprint",
            "harness": "codex",
            "session_id": sid,
            "started_at": "2026-09-01T00:00:00Z",
        });
        if let Some(at) = ended {
            row["ended_at"] = json!(at);
        }
        row
    };
    stage_graph(
        dir.path(),
        json!([{
            "id": "x-4hp",
            "status": "ready",
            "sessions": [
                planning_row("planner-unclosed", None),
                planning_row("planner-closed", Some("2026-09-02T00:00:00Z")),
            ],
        }]),
    );
    for (name, sid) in [
        ("bp-x-4hp-a", "planner-unclosed"),
        ("bp-x-4hp-b", "planner-closed"),
    ] {
        crate::state::update_registry(&home.registry_json(), |r| {
            let mut e = state::RegistryEntry::default();
            e.name = name.into();
            e.short_id = name.into();
            e.origin = Some("spawn".into());
            e.harness = Some("codex".into());
            e.harness_session_id = Some(sid.into());
            e.created_at = "2026-09-01T00:00:00Z".into();
            r.entries.push(e);
        })
        .unwrap();
    }
    let store = home.root().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let quiet = quiet_transcript(&store, "q.jsonl", 2 * 3600);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &crate::gc_sweep::read_graph_entries,
        &move |_| Some(vec![quiet.clone()]),
        // The unclosed planner sits inside the idle hour, so its keep holds.
        &|entries: &[&state::RegistryEntry]| {
            entries
                .iter()
                .map(|e| {
                    let age = if e.name == "bp-x-4hp-a" {
                        1800
                    } else {
                        2 * 3600
                    };
                    (crate::gc::row_handle(e), Some(age))
                })
                .collect()
        },
        &|_| true,
        &|_| CascadeOutcome::Removed,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert_eq!(summary.retired.len(), 1, "{:?}", summary.retired);
    assert_eq!(summary.retired[0].0, "bp-x-4hp-b", "{:?}", summary.retired);
    assert!(
        summary
            .kept_planning_unclosed
            .iter()
            .any(|(id, node)| id == "bp-x-4hp-a" && node == "x-4hp"),
        "{:?}",
        summary.kept_planning_unclosed
    );
    assert!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .iter()
            .any(|e| e.name == "bp-x-4hp-a"),
        "the unclosed planner keeps its row"
    );
}

/// d-81c6da7e AC3-HP: a planner whose node has a `plan_path` that names a
/// MISSING file holds neither finished marker. The row keeps, the hold
/// names the node and its status, and the reason is what escalates and
/// what a release answers.
#[test]
fn a_planner_whose_plan_file_is_missing_keeps_and_holds() {
    use crate::daemon::CascadeOutcome;

    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let plan_file = dir.path().join("plans").join("gone.md");
    stage_graph(
        dir.path(),
        json!([{
            "id": "x-m3hp",
            "status": "idea",
            "plan_path": plan_file.display().to_string(),
            "sessions": [{
                "phase": "blueprint",
                "harness": "codex",
                "session_id": "s-m3",
                "started_at": "2026-09-01T00:00:00Z",
            }],
        }]),
    );
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "bp-x-m3hp".into();
        e.short_id = "bp-x-m3hp".into();
        e.origin = Some("spawn".into());
        e.harness = Some("codex".into());
        e.harness_session_id = Some("s-m3".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();
    let store = home.root().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let quiet = quiet_transcript(&store, "q.jsonl", 2 * 3600);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &crate::gc_sweep::read_graph_entries,
        &move |_| Some(vec![quiet.clone()]),
        // Inside the idle hour, so the unfinished-plan keep holds.
        &uniform_ages(1800),
        &|_| true,
        &|_| CascadeOutcome::Removed,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert_eq!(summary.retired.len(), 0, "{:?}", summary.retired);
    assert!(
        summary
            .kept_planning_unclosed
            .iter()
            .any(|(id, node)| id == "bp-x-m3hp" && node == "x-m3hp"),
        "{:?}",
        summary.kept_planning_unclosed
    );
    let hold = summary
        .holds
        .iter()
        .find(|h| h.id == "bp-x-m3hp")
        .expect("a hold for the unfinished planner");
    assert_eq!(
        hold.reason,
        "planning assignment not finished by this session"
    );
    assert_eq!(
        hold.detail,
        "x-m3hp idea: no close and no plan written by this session"
    );
}

#[test]
fn a_receipt_whose_reaped_at_will_not_parse_is_kept_and_named() {
    let home = tmp_home("gc-receipt-baddate");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let broken = write_receipt_at(&home, "claude-broken-sess.json", "");
    let missing = write_receipt_at(&home, "claude-missing-sess.json", "not-a-date");

    let summary = gc_sweep(&home, &emitter, 900, 7);

    // A failed read is not evidence of age: both survive, and the sweep
    // names what it kept so a silent pile-up is never mistaken for health.
    assert!(broken.exists());
    assert!(missing.exists());
    assert!(summary.expired_receipts.is_empty());
    let kept: Vec<&str> = summary
        .kept_receipts
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(kept.contains(&"claude-broken-sess.json"), "{kept:?}");
    assert!(kept.contains(&"claude-missing-sess.json"), "{kept:?}");
}

/// Durability ordering, enforced not narrated: a receipt that cannot be
/// written holds its row in the registry. The receipts dir is pre-created
/// as a FILE, so the persist write fails and the sweep keeps the row.
#[test]
fn a_row_reaps_only_after_its_receipt_is_durable() {
    let home = tmp_home("gc-receipt-durability");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    std::fs::write(home.root().join("reap-receipts"), b"not a directory").unwrap();
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "undeliv.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut e = ask_row("undeliverable", None);
        e.short_id = "undeliv".into();
        e.origin = Some("spawn".into());
        r.entries.push(e);
    })
    .unwrap();

    let graph = graph_read(&[("undeliverable-sess", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &move |_| Some(vec![quiet.clone()]),
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );

    assert!(summary.retired.is_empty());
    assert!(
        summary
            .kept_no_receipt
            .iter()
            .any(|(id, reason)| id == "undeliv" && reason.contains("receipt did not persist")),
        "the persist failure must be surfaced: {:?}",
        summary.kept_no_receipt
    );
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.entries.iter().any(|e| e.name == "undeliverable"));
}

/// A retirement whose registry write refuses names every row it staged in
/// `kept_no_receipt`, so the real sweep reports what the dry run reports.
#[test]
fn a_refused_retirement_write_names_every_staged_row() {
    let home = tmp_home("gc-write-refused");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "refused.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut e = ask_row("refused", None);
        e.short_id = "refusd".into();
        e.origin = Some("spawn".into());
        r.entries.push(e);
    })
    .unwrap();
    // A newer writer floor: reads still serve, every write refuses.
    let mut doc: serde_json::Value =
        serde_json::from_str(&crate::registry_store::read_raw(&home.registry_json())).unwrap();
    doc["min_writer_version"] = serde_json::json!(state::REGISTRY_SCHEMA_VERSION + 1);
    crate::registry_store::seed_raw(home.registry_json(), doc.to_string());

    let summary = staged_sweep(
        &home,
        &emitter,
        900,
        graph_read(&[("refused-sess", "N1", "done")], &[]),
        &move |_| Some(vec![quiet.clone()]),
        &|_| (Some(true), Some(true)),
    );

    assert!(summary.retired.is_empty(), "{summary:?}");
    assert!(
        summary
            .kept_no_receipt
            .iter()
            .any(|(id, reason)| id == "refusd" && reason.starts_with("registry write refused: ")),
        "{:?}",
        summary.kept_no_receipt
    );
}

#[test]
fn gc_sweep_turns_unterminated_node_reap_into_durable_failure() {
    let sandbox = tmp_home("gc-dead-dispatch");
    let home = AgentsHome::at(sandbox.root().join("agents"));
    home.ensure_root().unwrap();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let dead_repo = home.root().join("dead-repo");
    let done_repo = home.root().join("done-repo");
    for repo in [&dead_repo, &done_repo] {
        std::fs::create_dir_all(repo.join(".fno")).unwrap();
        assert!(crate::git_test_helpers::git_init(repo));
    }

    let dead_session = "target-run-dead";
    let done_session = "target-run-done";
    std::fs::write(
        dead_repo.join(".fno/target-state.md"),
        format!("---\nfno_id: {dead_session}\ninput: x-a35a\nplan_path: \"\"\n---\n"),
    )
    .unwrap();
    std::fs::write(
        done_repo.join(".fno/target-state.md"),
        format!("---\nfno_id: {done_session}\ninput: x-b44e\nplan_path: \"\"\n---\n"),
    )
    .unwrap();
    state::update_registry(&home.registry_json(), |r| {
        let mut dead = bg_claude_row("target-x-a35a-route-atomicity", "dead0001");
        dead.status = AgentStatus::Exited;
        dead.cwd = dead_repo.to_string_lossy().into_owned();
        dead.exited_at = Some("2020-01-01T00:00:00Z".into());
        dead.log_path = Some(stale_log(&dead_repo));
        dead.harness_session_id = Some("dead-harness-uuid".into());
        r.entries.push(dead);

        let mut done = bg_claude_row("target-x-b44e-finished", "done0002");
        done.status = AgentStatus::Exited;
        done.cwd = done_repo.to_string_lossy().into_owned();
        done.exited_at = Some("2020-01-01T00:00:00Z".into());
        done.log_path = Some(stale_log(&done_repo));
        done.harness_session_id = Some("done-harness-uuid".into());
        r.entries.push(done);
    })
    .unwrap();

    let global_events = home.root().parent().unwrap().join("events.jsonl");
    std::fs::write(
            done_repo.join(".fno/events.jsonl.1"),
            format!(
                "{{\"ts\":\"2026-07-24T00:00:00Z\",\"type\":\"termination\",\"source\":\"loop\",\"data\":{{\"session_id\":\"{done_session}\",\"reason\":\"DonePRGreen\",\"message\":\"done\"}}}}\n"
            ),
        )
        .unwrap();
    // Production rotation ingests a generation before the rename; the reader
    // answers from the store, so the seeded generation must be ingested too.
    crate::event_store::sync(&done_repo.join(".fno/events.jsonl")).unwrap();

    let summary = retire_sweep(
        &home,
        &emitter,
        &[
            ("dead-harness-uuid", "x-a35a", "done"),
            ("done-harness-uuid", "x-b44e", "done"),
        ],
        &|e| {
            e.log_path
                .as_deref()
                .map(|p| vec![std::path::PathBuf::from(p)])
        },
    );
    assert_eq!(summary.retired.len(), 2, "{:?}", summary.retired);

    let reaps = read_events(&home);
    // The reap event, by type: the removal accounting (x-a879) also emits
    // registry_row_removed into this log carrying the same short_id, so a
    // bare short_id match is ambiguous.
    let dead_reap = reaps
        .iter()
        .find(|e| e["type"] == "agent_row_reaped" && e["data"]["short_id"] == "dead0001")
        .expect("dead dispatch reap event");
    assert_eq!(dead_reap["data"]["node_id"], "x-a35a");
    assert_eq!(dead_reap["data"]["termination_event"], false);
    let done_reap = reaps
        .iter()
        .find(|e| e["type"] == "agent_row_reaped" && e["data"]["short_id"] == "done0002")
        .expect("completed dispatch reap event");
    assert_eq!(done_reap["data"]["node_id"], "x-b44e");
    assert_eq!(done_reap["data"]["termination_event"], true);

    let global = crate::events::committed_journal_text(&global_events);
    let failures: Vec<Value> = global
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|e: &Value| e["type"] == "node_failed")
        .collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["data"]["unit_id"], "x-a35a");
    assert_eq!(failures[0]["data"]["session_id"], dead_session);
    assert_eq!(
        failures[0]["data"]["reason"],
        "agent-row-reaped-no-termination"
    );
}

#[test]
fn gc_sweep_restores_row_when_termination_evidence_is_unknown() {
    let sandbox = tmp_home("gc-unknown-termination");
    let home = AgentsHome::at(sandbox.root().join("agents"));
    home.ensure_root().unwrap();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let repo = home.root().join("repo");
    std::fs::create_dir_all(repo.join(".fno")).unwrap();
    assert!(crate::git_test_helpers::git_init(&repo));
    std::fs::write(
        repo.join(".fno/target-state.md"),
        "---\nfno_id: reused-run\ninput: x-other\nplan_path: \"\"\n---\n",
    )
    .unwrap();
    state::update_registry(&home.registry_json(), |registry| {
        let mut row = bg_claude_row("target-x-a35a-route-atomicity", "dead0001");
        row.status = AgentStatus::Exited;
        row.cwd = repo.to_string_lossy().into_owned();
        row.exited_at = Some("2020-01-01T00:00:00Z".into());
        row.log_path = Some(stale_log(&repo));
        // A real bg worker carries its session identity, so the receipt
        // gate stages a record and the row reaches the dispatch path this
        // test exercises.
        row.harness_session_id = Some("019cdead-0000-7000-8000-000000000001".into());
        registry.entries.push(row);
    })
    .unwrap();

    let summary = retire_sweep(
        &home,
        &emitter,
        &[("019cdead-0000-7000-8000-000000000001", "x-other", "done")],
        &|e| {
            e.log_path
                .as_deref()
                .map(|p| vec![std::path::PathBuf::from(p)])
        },
    );

    assert!(summary.retired.is_empty());
    let registry = state::load_registry(&home.registry_json()).unwrap();
    assert!(registry
        .entries
        .iter()
        .any(|row| row.name == "target-x-a35a-route-atomicity"));
    let events = read_events(&home);
    assert!(events.iter().any(|event| {
        event["type"] == "daemon_recovery_error"
            && event["data"]["op"] == "observe_dead_dispatch_termination"
    }));
}

#[test]
fn gc_sweep_restores_row_when_dead_dispatch_receipt_cannot_persist() {
    let sandbox = tmp_home("gc-dead-dispatch-write-failure");
    let home = AgentsHome::at(sandbox.root().join("agents"));
    home.ensure_root().unwrap();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let repo = home.root().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(crate::git_test_helpers::git_init(&repo));
    state::update_registry(&home.registry_json(), |registry| {
        let mut row = bg_claude_row("target-x-a35a-route-atomicity", "dead0001");
        row.status = AgentStatus::Exited;
        row.cwd = repo.to_string_lossy().into_owned();
        row.exited_at = Some("2020-01-01T00:00:00Z".into());
        row.log_path = Some(stale_log(&repo));
        // Session identity present, so the receipt gate stages a record
        // and the sweep reaches the dead-dispatch write this test breaks.
        row.harness_session_id = Some("019cdead-0000-7000-8000-000000000001".into());
        registry.entries.push(row);
    })
    .unwrap();
    // The write this test breaks is the STORE commit now: a directory at the
    // store path refuses to open as SQLite, so the receipt persist fails the
    // same way a journal append to a directory did pre-cutover.
    std::fs::create_dir_all(global_events_path(&home)).unwrap();
    std::fs::create_dir_all(global_events_path(&home).with_file_name("events.db")).unwrap();

    let summary = retire_sweep(
        &home,
        &emitter,
        &[("019cdead-0000-7000-8000-000000000001", "x-a35a", "done")],
        &|e| {
            e.log_path
                .as_deref()
                .map(|p| vec![std::path::PathBuf::from(p)])
        },
    );

    assert!(summary.retired.is_empty());
    let registry = state::load_registry(&home.registry_json()).unwrap();
    assert!(registry
        .entries
        .iter()
        .any(|row| row.name == "target-x-a35a-route-atomicity"));
    let events = read_events(&home);
    assert!(events.iter().any(|event| {
        event["type"] == "daemon_recovery_error" && event["data"]["op"] == "record_dead_dispatch"
    }));
}

#[tokio::test]
async fn lifecycle_name_resolution_never_falls_back_on_ambiguity() {
    let mut named = rentry("deadbeef", AgentStatus::Live, None);
    named.short_id = "transport-a".into();
    named.harness_session_id = Some("aaaaaaaa-1111-2222-3333-444455556666".into());
    let mut short = rentry("other", AgentStatus::Live, None);
    short.short_id = "deadbeef".into();
    short.harness_session_id = Some("bbbbbbbb-1111-2222-3333-000000000002".into());
    let reg = crate::state::Registry {
        schema_version: crate::state::REGISTRY_SCHEMA_VERSION,
        entries: vec![named, short],
        ..Default::default()
    };

    let error = entry_for_lifecycle(
        &reg,
        "deadbeef",
        std::path::Path::new("/nonexistent/registry.json"),
        false,
    )
    .await
    .expect_err("ambiguous token must not fall back to the matching row name");

    assert!(error.contains("ambiguous across 2 agents"));
}

#[test]
fn recovery_orphan_pid_sweep_does_not_condemn_every_row_sharing_an_empty_short_id() {
    // x-9de7 task 1: the orphan-PID sweep (Step 6 of recover(), ~line 270)
    // collects reaped short_ids into a `BTreeSet<String>`, then marks EVERY
    // entry whose short_id is a MEMBER of that set as Exited -- not just the
    // specific entry that failed pid_is_ours. Every codex/gemini shellout
    // row shares the same empty short_id (see the comment at the top of
    // recover()), so one genuinely dead pane-hosted row poisons every live
    // one that happens to sit beside it in the registry. This is the writer
    // behind the false `exited` write on a live mux pane row.
    let home = tmp_home("recover-empty-short-id-collision");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let me = std::process::id();
    let Some(my_start) = process_start_time(me) else {
        return; // platform without start-time support; nothing to assert
    };
    state::update_registry(&home.registry_json(), |r| {
        // Genuinely dead: pid_is_ours must return false for this one.
        let mut dead = ask_row("dead-pane", None);
        dead.status = AgentStatus::Live;
        dead.pid = Some(0x7fff_fff0); // not a live process
        r.entries.push(dead);

        // Live: real pid, matching start time, hosted in a mux pane -- same
        // empty short_id as the dead row above.
        let mut live = ask_row("live-pane", None);
        live.status = AgentStatus::Live;
        live.pid = Some(me);
        live.pid_start_time = Some(my_start);
        live.mux = Some(state::MuxRef {
            session: "main".into(),
            pane_id: 1,
        });
        r.entries.push(live);
    })
    .unwrap();

    let _ = recover(&home, &emitter);
    let reg = state::load_registry(&home.registry_json()).unwrap();
    let live = reg.find("live-pane").unwrap();
    assert_eq!(
        live.status,
        AgentStatus::Live,
        "a live pane-hosted row must not be condemned by a sibling's empty short_id"
    );
    assert!(
        live.pid.is_some(),
        "the writer clears no pid; a fix must not start clearing it here either"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

// ---- x-cd31: idle-exit reads live workers, not registry emptiness ------

#[test]
fn idle_exit_fires_on_terminal_rows_with_no_live_worker() {
    // The exact defect box: a registry of TERMINAL rows (the roster an
    // established machine always has) with no live socket and no live pid
    // must let the daemon exit. Registry emptiness never held here.
    let home = short_home("idle-terminal");
    home.ensure_root().unwrap();
    state::update_registry(&home.registry_json(), |r| {
        r.entries
            .push(ask_row("done-1", Some("2020-01-01T00:00:00Z")));
        r.entries
            .push(ask_row("done-2", Some("2020-01-01T00:00:00Z")));
    })
    .unwrap();
    assert!(
        no_live_worker(&home),
        "terminal rows with dead pids and no sockets must not pin the daemon"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn idle_exit_held_by_a_live_worker_socket() {
    let home = short_home("idle-sock");
    home.ensure_root().unwrap();
    std::fs::create_dir_all(home.agent_dir("wka")).unwrap();
    // A REAL listener: since the stale-socket fix, file existence alone
    // does not pin the daemon - something must answer on the socket.
    let _listener = std::os::unix::net::UnixListener::bind(home.worker_sock("wka")).unwrap();
    state::update_registry(&home.registry_json(), |r| {
        // Terminal row, dead pid, but a live worker serving on its socket.
        let mut row = ask_row("wka", Some("2020-01-01T00:00:00Z"));
        row.short_id = "wka".to_string();
        r.entries.push(row);
    })
    .unwrap();
    assert!(
        !no_live_worker(&home),
        "a reachable worker socket pins the daemon regardless of what its row says"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn idle_exit_held_by_an_unreadable_registry() {
    // The fail-safe side: an unreadable registry is an absence with two
    // explanations, and the daemon must stay resident rather than exit on
    // a transient read failure (the old code exited: unwrap_or(true)).
    let home = short_home("idle-unreadable");
    home.ensure_root().unwrap();
    crate::registry_store::seed_raw(&home.registry_json(), "not json at all{");
    assert!(
        !no_live_worker(&home),
        "an unreadable registry must not license an idle exit"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test]
async fn stop_claude_pid_kills_a_real_child_and_spares_a_recycled_pid() {
    // x-a4b2: a row with a pid and no transport id must actually be stopped
    // (it used to be refused, leaving a live duplicate worker), and a pid
    // whose start time no longer matches must be left alone.
    let mut entry = ask_row("orphan", None);

    // A row with no pid at all has nothing to signal.
    assert!(
        !stop_claude_pid_confirmed(&entry).await,
        "no pid -> nothing to stop"
    );

    // Spawn the sleeper as a DETACHED grandchild: `sh` backgrounds it and
    // exits, so it is reparented away and is never this test's child. A
    // direct child would linger as a zombie after SIGTERM until reaped, and
    // `pid_is_ours` (a bare `kill(pid, 0)` probe) reads a zombie as alive.
    // The real claude worker is not the daemon's child either, so this also
    // matches production.
    let out = std::process::Command::new("sh")
        .arg("-c")
        // The redirect is load-bearing: a backgrounded child inherits sh's
        // stdout pipe, so without it `.output()` blocks for the full sleep
        // waiting on EOF instead of returning as soon as sh exits.
        .arg("sleep 60 >/dev/null 2>&1 & echo $!")
        .output()
        .expect("spawn detached sleeper");
    let pid: u32 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("sleeper pid");
    let start = process_start_time(pid);

    // Independent death oracle. Asserting with `pid_is_ours` would use the
    // subject's own probe as its judge, and that probe reports EPERM and a
    // recycled pid as not-ours too, so it can read "gone" over a process
    // that is still running. `ps` knows nothing about our guards.
    let ps_says_alive = |pid: u32| {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string()])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| l.split_whitespace().next() == Some(&pid.to_string()))
                    .count()
                    > 0
            })
            .unwrap_or(false)
    };

    // No incarnation token: bare liveness is not a licence to SIGKILL.
    entry.pid = Some(pid);
    entry.pid_start_time = None;
    assert!(
        !stop_claude_pid_confirmed(&entry).await,
        "no start token -> refuse"
    );
    assert!(ps_says_alive(pid), "a refused row must not be signalled");

    // Wrong incarnation token: the pid belongs to someone else now.
    if let Some(st) = start {
        entry.pid_start_time = Some(st.wrapping_add(1));
        assert!(
            !stop_claude_pid_confirmed(&entry).await,
            "recycled pid -> refuse"
        );
        assert!(
            ps_says_alive(pid),
            "an unrelated process must not be signalled"
        );
    }

    // Correct token: the process is really killed, not merely reported.
    entry.pid_start_time = start;
    if start.is_some() {
        assert!(
            stop_claude_pid_confirmed(&entry).await,
            "owned live pid -> stopped"
        );
        assert!(!ps_says_alive(pid), "process is gone");
    } else {
        // No readable start time on this platform: the guard above refuses
        // every row, so reap the sleeper rather than leaking it.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

#[test]
fn pid_is_ours_rejects_an_out_of_range_pid() {
    // u32::MAX wraps to -1 in signed pid_t, the "signal every process I may
    // signal" broadcast target. kill(-1, 0) succeeds and no start time is
    // readable, so without the range guard the probe returns true and the
    // caller broadcasts SIGTERM.
    assert!(!pid_is_ours(u32::MAX, None), "u32::MAX must never be ours");
    assert!(
        !pid_is_ours(i32::MAX as u32 + 1, Some(123)),
        "anything past i32::MAX wraps negative"
    );
    assert!(
        pid_confirmed_dead(u32::MAX),
        "out-of-range is never running"
    );
}

#[test]
fn recycle_and_death_each_demand_positive_evidence() {
    // The distinction `pid_gone_within` rests on. `!pid_is_ours` is NOT a
    // recycle test: it is also false for a live-but-unsignalable process, and
    // treating that as "gone" reports a clean stop over a running worker.
    let me = std::process::id();
    let Some(st) = process_start_time(me) else {
        return; // platform without start-time support
    };

    // Alive and ours: neither dead nor recycled.
    assert!(!pid_confirmed_dead(me), "a live pid is not dead");
    assert!(
        !pid_recycled(me, Some(st)),
        "matching token is not a recycle"
    );

    // Alive with a mismatched token: a positive recycle finding.
    assert!(
        pid_recycled(me, Some(st.wrapping_add(1))),
        "reachable + differing token is a recycle"
    );

    // No recorded token: no basis to claim a recycle either way.
    assert!(!pid_recycled(me, None), "no token -> no recycle verdict");

    // A dead pid is dead, and is never *also* reported as recycled -- the
    // caller must not be able to reach "gone" through an unproven path.
    let dead = 0x7fff_fff0u32;
    assert!(pid_confirmed_dead(dead), "unused high pid reads as dead");
    assert!(
        !pid_recycled(dead, Some(st)),
        "dead is not a recycle finding"
    );
}

#[test]
fn recovery_reaps_recycled_pid() {
    // ab-d19e6458: the recorded pid is ALIVE (our own), but its start time
    // does not match — the original worker died and the pid was reused by an
    // unrelated process. The reap must fire on the start-time mismatch, not
    // be fooled by bare liveness.
    let home = tmp_home("recover-recycled");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let me = std::process::id();
    if process_start_time(me).is_none() {
        std::fs::remove_dir_all(home.root()).ok();
        return; // start-time unsupported here; reuse detection N/A
    }
    state::update_registry(&home.registry_json(), |r| {
        r.entries.push(RegistryEntry {
            substrate: None,
            node: None,
            spawned_by_session: None,
            spawned_by_harness: None,
            spawned_by_cwd: None,
            launch_account: None,
            related_session_id: None,
            origin: None,
            name: "recycled".into(),
            short_id: "recycled".into(),
            legacy_provider: "codex".into(),
            provider: None,
            model: None,
            model_basis: None,
            effort: None,
            harness: None,
            harness_session_id: None,
            predecessor_session_ids: Vec::new(),
            forked_from_session_id: None,
            route_provider_id: None,
            model_name: None,
            account_record_id: None,
            cwd: "/tmp".into(),
            project_root: "/tmp".into(),
            session_id: None,
            spawn_trigger: None,
            legacy_claude_short_id: None,
            claude_session_uuid: None,
            messaging_socket_path: None,
            codex_session_id: None,
            gemini_session_id: None,
            mcp_channel_id: None,
            cc_session_id: None,
            host_mode: None,
            status: AgentStatus::Live,
            last_message_at: None,
            created_at: "2026-05-24T00:00:00Z".into(),
            pid: Some(me),
            // Bogus start time -> mismatch against our real one -> not ours.
            pid_start_time: Some(1),
            keeper_child_pid: None,
            log_path: None,
            last_reconciled_at: None,
            inside_leg: None,
            exited_at: None,
            mux: None,
            screen_state: None,
            role_level: None,
            role_scope: None,
            role_grantor: None,
            route_settings_path: None,
            fno_id: None,
            delivery_policy: None,
            sandbox_posture: None,
            git_grant: None,
            ..Default::default()
        });
    })
    .unwrap();
    let mut st = AgentState::new_pty("recycled");
    st.status = AgentStatus::Live;
    state::write_state_atomic(&home.state_json("recycled"), &st).unwrap();

    let report = recover(&home, &emitter).expect("startup recovery");
    assert_eq!(report.reaped_pids, vec![me]);
    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert_eq!(reg.find("recycled").unwrap().status, AgentStatus::Exited);
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn recovery_preserve_mode_keeps_orphan_state_dir() {
    let home = tmp_home("recover-preserve-orphan");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let mut st = AgentState::new_pty("loner");
    st.status = AgentStatus::Live;
    state::write_state_atomic(&home.state_json("loner"), &st).unwrap();

    let report = recover_with_policy(&home, &emitter, false).expect("preserve recovery");
    assert_eq!(report.recovery_mode, "preserve");
    assert!(report.archived_orphans.is_empty());
    assert!(
        home.agent_dir("loner").is_dir(),
        "preserve mode keeps the index"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn recovery_refuses_codex_thread_when_identity_is_missing_by_name() {
    let mut row = rentry("codex-thread-missing", AgentStatus::Live, None);
    row.harness = Some("codex".into());
    row.legacy_provider.clear();
    row.short_id.clear();
    row.host_mode = Some(crate::state::HOST_MODE_INTERACTIVE.into());
    row.cwd = "/tmp/codex-thread-worktree".into();

    let error = codex_thread_resume_identity(&row).unwrap_err();
    assert!(error.contains("harness_session_id"), "error: {error}");

    row.harness_session_id = Some("019f0000-0000-7000-8000-000000000001".into());
    row.cwd.clear();
    let error = codex_thread_resume_identity(&row).unwrap_err();
    assert!(error.contains("cwd"), "error: {error}");
}

#[test]
fn recovery_does_not_quarantine_a_temp_held_by_an_active_writer() {
    let home = tmp_home("recover-active-temp");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    state::update_registry(&home.registry_json(), |_| {}).unwrap();
    let temp = home.root().join(".registry.json.tmp.active");
    std::fs::write(&temp, b"partial").unwrap();
    let lock_path = std::path::PathBuf::from(format!("{}.lock", home.registry_json().display()));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    lock.lock().unwrap();

    let found = crate::quarantine::quarantine_interrupted_write_temps(&home, &emitter);

    assert!(found.is_empty());
    assert!(temp.exists(), "active writer temp must remain in place");
}

// --- plan_reconcile (US6.9): tri-state, status-aware transitions, budget ---

#[test]
fn session_transition_apply_preserves_succession_and_splits_live_branch() {
    let mut registry = state::Registry::default();
    let mut predecessor = rentry("worker", AgentStatus::Live, None);
    predecessor.harness_session_id = Some("session-a".into());
    predecessor.fno_id = Some("thread-a".into());
    registry.entries.push(predecessor);

    assert_eq!(
        apply_session_transition(&mut registry, "worker", "session-b", Some(false), "",).unwrap(),
        state::SessionTransition::Succession
    );
    assert_eq!(registry.entries.len(), 1);
    assert_eq!(registry.entries[0].fno_id.as_deref(), Some("thread-a"));
    assert_eq!(
        registry.entries[0].predecessor_session_ids,
        vec!["session-a"]
    );

    assert_eq!(
        apply_session_transition(
            &mut registry,
            "worker",
            "session-c",
            Some(true),
            "worker-branch",
        )
        .unwrap(),
        state::SessionTransition::Branch
    );
    assert_eq!(registry.entries.len(), 2);
    assert_eq!(
        registry.entries[0].harness_session_id.as_deref(),
        Some("session-b")
    );
    assert_eq!(
        registry.entries[1].harness_session_id.as_deref(),
        Some("session-c")
    );
    assert_eq!(
        registry.entries[1].forked_from_session_id.as_deref(),
        Some("session-b")
    );
    // The transition itself stamps no id: the branch leaves with None and the
    // registry write mints one (the write-time fill covers that contract).
    assert_eq!(registry.entries[1].fno_id, None);

    assert_eq!(
        apply_session_transition(
            &mut registry,
            "worker",
            "session-d",
            Some(true),
            "worker-branch",
        )
        .unwrap(),
        state::SessionTransition::Branch
    );
    let second_branch = registry
        .entries
        .iter()
        .find(|entry| entry.harness_session_id.as_deref() == Some("session-d"))
        .expect("second branch row");
    assert_eq!(second_branch.name, "worker-branch-2");
    // Both branches read None at the transition; distinct mints land at the
    // write (covered by the update_registry fill test).
    assert_eq!(second_branch.fno_id, None);
}

// ── x-8739: the sweep settles a stale open do row on a settled node ──
// The settle test family, moved out of daemon.rs (shrink-only) and out of
// its own module: same-module helpers resolve directly.

/// A home whose graph path (`home.root().parent()/graph.json`) lands INSIDE
/// the test's tmpdir; `tmp_home` would make it the shared temp dir instead.
pub(super) fn staged_graph_home() -> (tempfile::TempDir, AgentsHome) {
    let dir = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(dir.path().join("agents"));
    home.ensure_root().unwrap();
    crate::paths::pin_test_claims_root(dir.path());
    (dir, home)
}

/// Seed graph.db for tests that address the stable graph path anchor.
pub(super) fn stage_graph(dir: &std::path::Path, entries: Value) {
    crate::graph_store::seed_rows(
        &dir.join("graph.json"),
        entries.as_array().expect("entry rows"),
    )
    .unwrap();
}

/// The settled-node shape: done, GitHub-confirmed merged, no additional PR.
pub(super) fn done_node(id: &str, merge_status: Value, aprs: Value, sessions: Vec<Value>) -> Value {
    json!({
        "id": id,
        "status": "done",
        "completed_at": "2026-09-01T00:00:00Z",
        "merge_status": merge_status,
        "additional_prs": aprs,
        "sessions": sessions,
    })
}

/// One open do row.
pub(super) fn open_do_row(harness: &str, sid: &str) -> Value {
    json!({
        "phase": "execute",
        "harness": harness,
        "session_id": sid,
        "started_at": "2026-09-01T01:00:00Z",
    })
}

/// The shipped shell's order against a REAL graph file: settle first, then
/// the row pass over the production graph read. Only the transcript/stop/
/// tree seams are staged (gc::gc_sweep resolves those from live stores a
/// tmp home does not have); the settle -> run -> read ordering under test is
/// the wiring gc.rs ships.
fn settle_then_run(
    home: &AgentsHome,
    emitter: &EventEmitter,
    dry_run: bool,
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
    agents: crate::claude_roster::ClaudeAgentsSnapshot,
) -> GcSummary {
    if dry_run {
        let planned = gc_sweep::plan_stale_do_rows(home);
        let mut summary = gc_sweep::run(
            home,
            emitter,
            0,
            true,
            0,
            &|h| {
                gc_sweep::read_graph_entries(h).map(|g| gc_sweep::without_settled(g, &planned, &[]))
            },
            transcripts,
            &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
            &|_| true,
            &|_| crate::daemon::CascadeOutcome::NotApplicable,
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &move || agents.clone(),
            &|_| (None, None),
            &|_| None,
        );
        summary.settled_do_rows = planned
            .into_iter()
            .map(|row| (row.node, row.harness, row.session_id))
            .collect();
        summary
    } else {
        let mut read = |_path: &str, _cwd: &str| -> Option<gc_sweep::PrState> {
            panic!("no existing settle_then_run test may reach GitHub");
        };
        let (settled, refused) = gc_sweep::settle_stale_do_rows_with(home, &mut read);
        let mut summary = gc_sweep::run(
            home,
            emitter,
            0,
            false,
            7,
            &gc_sweep::read_graph_entries,
            transcripts,
            &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
            &|_| true,
            &|_| crate::daemon::CascadeOutcome::NotApplicable,
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &move || agents.clone(),
            &|_| (None, None),
            &|_| None,
        );
        summary.settled_do_rows = settled
            .into_iter()
            .map(|row| (row.node, row.harness, row.session_id))
            .collect();
        summary.settle_refused = refused;
        summary
    }
}

/// A registry row named on `sid`, spawned by fno, transcript-probeable.
fn spawn_row(reg: &mut state::Registry, name: &str, sid: &str) {
    let mut e = ask_row(name, None);
    e.harness = Some("claude".into());
    e.harness_session_id = Some(sid.into());
    e.origin = Some("spawn".into());
    // The verdict door reads the stored status first; these scenarios vary
    // the vendor word, so the row itself must be fno-undecided.
    e.status = crate::AgentStatus::Idle;
    reg.entries.push(e);
}

/// AC4-EDGE: the rehearsal names the settle and touches nothing - the graph
/// store version is unchanged, the row still open, and the row pass
/// reads it as would-retire.
#[test]
fn a_dry_run_settles_nothing_on_disk() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "d.jsonl", 2 * 3600);
    stage_graph(
        dir.path(),
        json!([done_node(
            "N4",
            json!("merged"),
            json!([]),
            vec![open_do_row("claude", "sess-d")],
        )]),
    );
    state::update_registry(&home.registry_json(), |r| {
        spawn_row(r, "row-d", "sess-d");
    })
    .unwrap();
    let before = crate::backlog::version(&dir.path().join("graph.json")).unwrap();

    let summary = settle_then_run(
        &home,
        &emitter,
        true,
        &move |_| Some(vec![quiet.clone()]),
        crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
            crate::claude_roster::ClaudeAgentRow::new("sess-d", Some("done")),
        ]),
    );

    assert_eq!(
        summary.settled_do_rows,
        vec![("N4".into(), "claude".into(), "sess-d".into())]
    );
    assert!(summary.kept_open_do_row.is_empty());
    let after = crate::backlog::version(&dir.path().join("graph.json")).unwrap();
    assert_eq!(before, after, "a dry run changed the store");
    let rows = crate::graph_store::read_rows(&dir.path().join("graph.json")).unwrap();
    assert!(rows[0]["sessions"][0].get("ended_at").is_none());
    assert!(
        !summary.dry_run_unverified.is_empty(),
        "the row stays on the retirement path, its remaining gate named: {summary:?}"
    );
}

/// AC3-EDGE: a settle that cannot read the graph is NAMED, never silent,
/// and the settle writes nothing. The write arm rides the same refusal
/// mapping (locked_mutate's Conflict / LockTimeout).
#[test]
fn a_settle_that_cannot_read_is_named_and_changes_nothing() {
    let (dir, home) = staged_graph_home();
    // An unimported malformed anchor makes the read refuse, never write; the
    // legacy db twin makes the ladder read this root spelling.
    std::fs::write(dir.path().join("graph.db"), b"SQLite format 3\0").unwrap();
    std::fs::write(dir.path().join("graph.json"), b"{not json").unwrap();

    let (settled, refused) = gc_sweep::settle_stale_do_rows(&home);

    assert!(settled.is_empty());
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].1.contains("graph unreadable"), "{refused:?}");
}

/// x-5aef AC5, hard version (the codex P1 on PR 1637): an open do row that
/// opened after the decision must hold the row BEFORE the effects fire. The
/// decision seam answers done-and-quiet; the real graph carries an open do
/// row naming this session. The stop and surface seams COUNT their calls,
/// proving the effects never fired; the row keeps under kept_open_do_row
/// with the node named. A held session whose process was already stopped is
/// not held at all - it is dead.
#[test]
fn a_graph_obligation_opened_after_the_decision_holds_before_the_effects() {
    use crate::daemon::CascadeOutcome;

    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    // The real graph: an open do row for this session on x-new - the
    // obligation that "opens after the decision".
    stage_graph(
        dir.path(),
        json!([{
            "id": "x-new",
            "status": "ready",
            "sessions": [open_do_row("codex", "s-commit")],
        }]),
    );
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "commitw".into();
        e.short_id = "commitw".into();
        e.origin = Some("spawn".into());
        e.harness = Some("codex".into());
        e.harness_session_id = Some("s-commit".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();
    let store = home.root().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let quiet = quiet_transcript(&store, "q.jsonl", 2 * 3600);
    let stop_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let surface_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    // AC1-HP: BOTH modes re-read the graph at staging and both keep
    // the row. One sweep runner, one counter pair, two modes.
    let run_once = |dry: bool,
                    stop_c: std::rc::Rc<std::cell::Cell<usize>>,
                    surf_c: std::rc::Rc<std::cell::Cell<usize>>|
     -> GcSummary {
        // The decision seam: done, no open do row - the stale evidence the
        // staging re-read must contradict.
        let graph = graph_read(&[("s-commit", "N1", "done")], &[]);
        let q = quiet.clone();
        gc_sweep::run(
            &home,
            &emitter,
            900,
            dry,
            7,
            &move |_| graph.clone(),
            &move |_| Some(vec![q.clone()]),
            &uniform_ages(2 * 3600),
            &move |_| {
                stop_c.set(stop_c.get() + 1);
                true
            },
            &move |_| {
                surf_c.set(surf_c.get() + 1);
                CascadeOutcome::Removed
            },
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &no_agents,
            &|_| (Some(true), Some(true)),
            &|_| None,
        )
    };
    let dry = run_once(
        true,
        std::rc::Rc::clone(&stop_calls),
        std::rc::Rc::clone(&surface_calls),
    );
    let acting = run_once(
        false,
        std::rc::Rc::clone(&stop_calls),
        std::rc::Rc::clone(&surface_calls),
    );
    for (mode, summary) in [("dry", dry), ("apply", acting)] {
        assert!(summary.retired.is_empty(), "{mode}: {:?}", summary.retired);
        assert!(
            summary
                .kept_open_do_row
                .iter()
                .any(|(id, node)| id == "commitw" && node == "x-new"),
            "{mode}: {:?}",
            summary.kept_open_do_row
        );
    }
    assert_eq!(
        stop_calls.get(),
        0,
        "the stop must never fire in either mode"
    );
    assert_eq!(
        surface_calls.get(),
        0,
        "the surface removal must never fire in either mode"
    );
    assert!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .iter()
            .any(|e| e.name == "commitw"),
        "the row survives"
    );
}

/// AC1-ERR: the staging re-read is unavailable (a corrupt graph.json
/// at the state root), so BOTH modes hold the row, the output names the
/// graph read, and no effect closure runs. A read that cannot answer is not
/// evidence the obligation is gone.
#[test]
fn an_unreadable_staging_graph_holds_the_row_in_both_modes_without_effects() {
    use crate::daemon::CascadeOutcome;

    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "corruptw".into();
        e.short_id = "t-corrupt".into();
        e.origin = Some("spawn".into());
        e.harness = Some("codex".into());
        e.harness_session_id = Some("s-corrupt".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();
    // The registry shares graph.db, so the graph turns unreadable by losing
    // its version, not by corrupt bytes.
    stage_graph(dir.path(), json!([]));
    rusqlite::Connection::open(crate::backlog::database_path(
        &dir.path().join("graph.json"),
    ))
    .unwrap()
    .execute("DELETE FROM graph_meta WHERE key = 'version'", [])
    .unwrap();
    let store = home.root().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let quiet = quiet_transcript(&store, "q.jsonl", 2 * 3600);
    let stop_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let surface_calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let run_once = |dry: bool,
                    stop_c: std::rc::Rc<std::cell::Cell<usize>>,
                    surf_c: std::rc::Rc<std::cell::Cell<usize>>|
     -> GcSummary {
        // The decision seam stays VALID: only the staging re-read (the real
        // file) is unreadable, so the row classifies would-retire and the
        // refusal must come from staging's own read.
        let graph = graph_read(&[("s-corrupt", "N1", "done")], &[]);
        let q = quiet.clone();
        gc_sweep::run(
            &home,
            &emitter,
            900,
            dry,
            7,
            &move |_| graph.clone(),
            &move |_| Some(vec![q.clone()]),
            &uniform_ages(2 * 3600),
            &move |_| {
                stop_c.set(stop_c.get() + 1);
                true
            },
            &move |_| {
                surf_c.set(surf_c.get() + 1);
                CascadeOutcome::Removed
            },
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &no_agents,
            &|_| (Some(true), Some(true)),
            &|_| None,
        )
    };
    let dry = run_once(
        true,
        std::rc::Rc::clone(&stop_calls),
        std::rc::Rc::clone(&surface_calls),
    );
    let acting = run_once(
        false,
        std::rc::Rc::clone(&stop_calls),
        std::rc::Rc::clone(&surface_calls),
    );
    for (mode, summary) in [("dry", dry), ("apply", acting)] {
        assert!(summary.retired.is_empty(), "{mode}: {:?}", summary.retired);
        assert!(
            summary
                .kept_graph_unreadable
                .iter()
                .any(|id| id == "t-corrupt"),
            "{mode}: {:?}",
            summary.kept_graph_unreadable
        );
    }
    assert_eq!(
        stop_calls.get(),
        0,
        "the stop must never fire in either mode"
    );
    assert_eq!(
        surface_calls.get(),
        0,
        "the surface removal must never fire in either mode"
    );
}

/// The commit gate as the SECOND belt: with the pre-effects re-check passed
/// (no open do row at stage time on the real graph), a receipt staged by a
/// direct stage call is not counted when the graph re-read at commit finds
/// the obligation. This is the only way to observe the stage-to-commit span
/// in a test: drive commit_retirements directly.
#[test]
fn the_commit_gate_drops_an_order_whose_obligation_opened() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    stage_graph(
        dir.path(),
        json!([{
            "id": "x-late",
            "status": "ready",
            "sessions": [open_do_row("codex", "s-late")],
        }]),
    );
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "latew".into();
        e.short_id = "latew".into();
        e.origin = Some("spawn".into());
        e.harness = Some("codex".into());
        e.harness_session_id = Some("s-late".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        r.entries.push(e);
    })
    .unwrap();
    let entries = state::load_registry(&home.registry_json()).unwrap();
    let entry = entries.entries.first().unwrap();
    let mut receipt =
        crate::receipt::build_reap_receipt(entry, None, crate::receipt::Writer::GcSweep).unwrap();
    receipt.effects = vec![crate::gc_native::stop_outcome_effect(true, None)];
    let mut receipts = std::collections::BTreeMap::new();
    receipts.insert(entry.name.clone(), receipt);
    let order = gc_sweep::RetireOrder {
        via_release: false,
        pr_settled_live: false,
        id: "latew".into(),
        basis: "test".into(),
        created_at: entry.created_at.clone(),
        tree: crate::gc::TreeAction::None,
        worktree: None,
        released: false,
        idle_hour_past: false,
    };
    let mut to_retire = std::collections::BTreeMap::new();
    to_retire.insert(entry.name.clone(), order);
    let report = gc_sweep::commit_retirements(
        &home,
        &emitter,
        "test",
        std::slice::from_ref(entry),
        &mut to_retire,
        &receipts,
        &|_| None,
    );
    assert!(report.retired.is_empty(), "{:?}", report.retired);
    assert!(
        report
            .kept_no_receipt
            .iter()
            .any(|(id, reason)| id == "latew"
                && reason.contains("graph obligation opened after the decision: x-late")),
        "{:?}",
        report.kept_no_receipt
    );
    assert!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .iter()
            .any(|e| e.name == "latew"),
        "the row survives the commit gate"
    );
}

/// x-5aef AC6-HP, the archived-session RECORD journey. The receipt is
/// built through the real capability table and persisted to the store;
/// the session's original cwd is then DELETED; resolution by exact
/// session id still yields the resume tokens and a locator naming a
/// transcript that exists and reads back. The verifier, given this
/// session as its expected cohort, certifies the retirement - so a pass
/// means the record journey, not merely a file on disk.
///
/// Ceiling, stated plainly: this proves the RECORD survives cwd deletion
/// and resolves; it does NOT launch `claude --resume` and assert the
/// native harness reopened the session. Nothing in CI can.
#[test]
fn the_archived_session_record_survives_cwd_deletion_and_resolves() {
    let (dir, home) = staged_graph_home();
    let store = dir.path().join("native-store");
    std::fs::create_dir_all(&store).unwrap();
    let transcript = store.join("journey-0000-0000-0000-000000000000.jsonl");
    std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();
    let cwd = dir.path().join("proj-gone");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut e = state::RegistryEntry::default();
    e.name = "journeyw".into();
    e.short_id = "journeyw".into();
    e.origin = Some("spawn".into());
    e.harness = Some("claude".into());
    e.harness_session_id = Some("journey-0000-0000-0000-000000000000".into());
    e.cwd = cwd.to_string_lossy().to_string();
    e.created_at = "2026-09-01T00:00:00Z".into();
    // The record: built through the REAL capability table (resume form
    // rendered, locator staged), then localized to the fixture store the
    // way the harness's own index resolves a live session.
    let mut receipt =
        crate::receipt::build_reap_receipt(&e, None, crate::receipt::Writer::GcSweep).unwrap();
    receipt.native_locator = Some(json!({ "transcripts": [transcript.to_string_lossy()] }));
    receipt.effects = vec![
        crate::gc_native::stop_outcome_effect(true, None),
        crate::daemon::CascadeOutcome::Removed.effect_record("active-surface"),
        crate::daemon::CascadeOutcome::NotApplicable.effect_record("mux-member"),
        crate::gc_sweep::resume_evidence_effect(&receipt),
    ];
    receipt.writer_build = Some(crate::gc_verify::current_build());
    crate::receipt::write_reap_receipt(&home, &receipt).unwrap();

    // The journey: the original cwd is gone; resolution by session id.
    std::fs::remove_dir_all(&cwd).unwrap();
    assert!(crate::resume_receipt::maybe_hint_preserved_session(
        &home,
        "journey-0000-0000-0000-000000000000"
    ));
    // The data the resolution prints: resume tokens and a live transcript.
    let resolved =
        crate::receipt::read_reap_receipt(&crate::receipt::reap_receipt_path(&home, &receipt))
            .unwrap();
    assert!(
        !resolved.resume_argv.is_empty(),
        "resume tokens survive: {resolved:?}"
    );
    assert!(transcript.exists(), "the located transcript still exists");
    let body = std::fs::read_to_string(&transcript).unwrap();
    assert!(body.contains("\"type\""), "the transcript reads back");

    // The gate, given this session as its cohort, certifies the record.
    let report = crate::gc_verify::verify(
        &home,
        24 * 3600,
        &["journey-0000-0000-0000-000000000000".to_string()],
    );
    assert!(report.passes(), "{:?}", report.problems);
    assert!(report.missing.is_empty());
}

// ── positive death evidence in the reaper ─────────────────────────────────

/// A sweep with the agents snapshot and stop seam staged separately, so a
/// test can prove WHICH evidence confirmed (or refused) the stop.
#[allow(clippy::too_many_arguments)]
pub(super) fn evidence_sweep(
    sweep_home: &AgentsHome,
    emitter: &EventEmitter,
    grace_secs: i64,
    dry_run: bool,
    graph: Option<GraphRead>,
    transcripts: &dyn Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
    agents: crate::claude_roster::ClaudeAgentsSnapshot,
    stop: &dyn Fn(&state::RegistryEntry) -> bool,
) -> GcSummary {
    gc_sweep::run(
        sweep_home,
        emitter,
        grace_secs,
        dry_run,
        7,
        &move |_| graph.clone(),
        transcripts,
        &staged_ages_at(crate::daemon::now_epoch_secs(), transcripts),
        stop,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &move || agents.clone(),
        &|_| (None, None),
        &|_| None,
    )
}

pub(super) fn claude_worker_row(name: &str, short: &str) -> state::RegistryEntry {
    let mut row = ask_row(name, None);
    row.short_id = short.into();
    row.harness = Some("claude".into());
    row.harness_session_id = Some(format!("{short}-1111-2222-3333-444444444444"));
    row.origin = Some("spawn".into());
    // The verdict door reads the stored status first (Exited would decide
    // the row fno-Finished before the vendor word these scenarios vary);
    // a live worker row is non-terminal.
    row.status = crate::AgentStatus::Idle;
    row
}

#[test]
fn blocked_roster_state_is_not_death_evidence() {
    let home = tmp_home("gc-evidence-blocked");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "a.jsonl", 7200);
    state::update_registry(&home.registry_json(), |registry| {
        registry
            .entries
            .push(claude_worker_row("row-blocked", "eeee5555"));
    })
    .unwrap();
    let agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("eeee5555", Some("blocked")),
    ]);
    let summary = evidence_sweep(
        &home,
        &emitter,
        0,
        false,
        graph_read(
            &[("eeee5555-1111-2222-3333-444444444444", "N1", "done")],
            &[],
        ),
        &|_| Some(vec![quiet.clone()]),
        agents,
        &|_| false,
    );
    assert!(summary.retired.is_empty());
    assert!(summary
        .stop_refused
        .iter()
        .any(|(id, why)| id == "eeee5555" && why.contains("no death evidence")));
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn dry_run_promises_only_provable_rows() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet_a = quiet_transcript(transcripts.path(), "a.jsonl", 7200);
    let quiet_b = quiet_transcript(transcripts.path(), "b.jsonl", 7200);
    // The STAGING re-read reads the real graph file, so the done
    // nodes live on disk, not only in an injected decision seam.
    stage_graph(
        dir.path(),
        json!([
            {"id": "N1", "status": "done", "sessions": [
                {"phase": "do", "harness": "claude", "session_id": "ffff6666-1111-2222-3333-444444444444", "started_at": "2026-09-01T01:00:00Z", "ended_at": "2026-09-01T02:00:00Z"}
            ]},
            {"id": "N2", "status": "done", "sessions": [
                {"phase": "do", "harness": "claude", "session_id": "aaaa7777-1111-2222-3333-444444444444", "started_at": "2026-09-01T01:00:00Z", "ended_at": "2026-09-01T02:00:00Z"}
            ]}
        ]),
    );
    state::update_registry(&home.registry_json(), |registry| {
        registry
            .entries
            .push(claude_worker_row("row-done", "ffff6666"));
        registry
            .entries
            .push(claude_worker_row("row-live", "aaaa7777"));
    })
    .unwrap();
    let agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("ffff6666", Some("done")),
        crate::claude_roster::ClaudeAgentRow::new("aaaa7777", Some("working")),
    ]);
    let stops = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let surfaces = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let stops_seam = std::rc::Rc::clone(&stops);
    let surfaces_seam = std::rc::Rc::clone(&surfaces);
    let picks = |e: &state::RegistryEntry| match e.harness_session_id.as_deref() {
        Some("ffff6666-1111-2222-3333-444444444444") => Some(vec![quiet_a.clone()]),
        Some("aaaa7777-1111-2222-3333-444444444444") => Some(vec![quiet_b.clone()]),
        _ => None,
    };
    let summary = gc_sweep::run(
        &home,
        &emitter,
        0,
        true,
        7,
        &gc_sweep::read_graph_entries,
        &picks,
        &staged_ages_at(crate::daemon::now_epoch_secs(), &picks),
        &move |_| {
            stops_seam.set(stops_seam.get() + 1);
            true
        },
        &move |_| {
            surfaces_seam.set(surfaces_seam.get() + 1);
            crate::daemon::CascadeOutcome::Failed("dry must never touch the surface".into())
        },
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &move || agents.clone(),
        &|_| (None, None),
        &|_| None,
    );
    // AC2-HP: positive stop evidence advances to the active-surface gate -
    // which a dry run cannot evaluate - so the row is NAMED, never retired,
    // and the would-refusing surface closure proves it was never invoked.
    assert!(summary.retired.is_empty(), "{:?}", summary.retired);
    assert_eq!(
        summary.dry_run_unverified,
        vec![(
            "ffff6666".to_string(),
            "active-surface removal was not evaluated".to_string()
        )]
    );
    // AC2-EDGE: no positive stop evidence stays ONLY under needs_live_stop;
    // it is not duplicated under dry_run_unverified.
    assert_eq!(
        summary.needs_live_stop.len(),
        1,
        "{:?}",
        summary.needs_live_stop
    );
    assert!(
        summary.needs_live_stop[0].0 == "aaaa7777"
            && summary.needs_live_stop[0].1.contains("does not promise"),
        "{:?}",
        summary.needs_live_stop
    );
    assert!(!summary
        .dry_run_unverified
        .iter()
        .any(|(id, _)| id == "aaaa7777"));
    assert_eq!(stops.get(), 0, "dry run never stops anything");
    assert_eq!(
        surfaces.get(),
        0,
        "a dry run never invokes the surface removal, not even to ask"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

// == x-a33f: the stage ends a row's process the way rm does ==============

fn staged_row(name: &str, harness: &str) -> state::RegistryEntry {
    let mut e = ask_row(name, None);
    e.short_id = format!("sid{name}");
    e.harness = Some(harness.into());
    e.harness_session_id = Some(format!("sess-{name}"));
    e.origin = Some("spawn".into());
    e
}

fn stage_direct(
    home: &AgentsHome,
    e: &state::RegistryEntry,
    stop: &dyn Fn(&state::RegistryEntry) -> bool,
    kill: &dyn Fn(&str, u64) -> Result<bool, String>,
) -> Result<crate::gc_sweep::StagedRetirement, crate::gc_sweep::RetireRefusal> {
    let mut receipts = std::collections::BTreeMap::new();
    crate::gc_sweep::stage_session_retirement(
        home,
        e,
        None,
        crate::gc_sweep::RetireMode::Apply,
        crate::gc_sweep::StopObservation::Unproven,
        false,
        false,
        stop,
        kill,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &mut receipts,
    )
}

/// AC7-ERR: a failing pane kill holds the row under `stop refused`, and the
/// hold detail quotes the kill's reason.
#[test]
fn ac7_stage_holds_the_row_when_the_pane_kill_fails() {
    let home = tmp_home("gc-ac7-err");
    let mut row = staged_row("muxrow", "codex");
    row.mux = Some(state::MuxRef {
        session: "main".into(),
        pane_id: 2,
    });
    let staged = stage_direct(
        &home,
        &row,
        &|_| panic!("a mux row must not reach the stop seam"),
        &|_, _| Err("pane server unreachable".into()),
    );
    match staged {
        Err(crate::gc_sweep::RetireRefusal::StopRefused(reason)) => {
            assert!(reason.contains("mux pane main:2 kill failed"), "{reason}");
            assert!(reason.contains("pane server unreachable"), "{reason}");
        }
        other => match other {
            Ok(_) => panic!("expected a stop refusal, got Retired"),
            Err(crate::gc_sweep::RetireRefusal::StopRefused(r)) => {
                panic!("wrong stop refusal: {r}")
            }
            Err(_) => panic!("expected a stop refusal, got a different refusal"),
        },
    }
    std::fs::remove_dir_all(home.root()).ok();
}

// ── the live-claim keep ──────────────────────────────────────

/// A live claim naming the session keeps a quiet, done-node row the sweep
/// would otherwise retire. The 2026-09-25 registry sweeps retired thread
/// workers whose claims were live; the claim fact now rides the row and
/// the grace gate keeps it past quiet.
#[test]
fn a_live_claim_keeps_a_quiet_row_the_sweep_would_retire() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (dir, home) = staged_graph_home();
    // Force-pin: the sweep's claims read must see THIS test's root, not a
    // root an earlier test's set-if-unset pin left behind. The guard
    // unsets the pin on scope exit, panic included, so an assert failure
    // never leaks it into a concurrently running test.
    struct ClaimsRootGuard;
    impl Drop for ClaimsRootGuard {
        fn drop(&mut self) {
            std::env::remove_var("FNO_CLAIMS_ROOT");
        }
    }
    std::env::set_var("FNO_CLAIMS_ROOT", dir.path());
    let _claims_root_guard = ClaimsRootGuard;
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "q.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut row = ask_row("row-claim", None);
        row.short_id = "t-claim".into();
        row.harness = Some("claude".into());
        row.harness_session_id = Some("s-done".into());
        row.origin = Some("spawn".into());
        r.entries.push(row);
    })
    .unwrap();
    // A PID-liveness claim naming s-done: the holder pid is this test
    // process, so classification reads Live through the same claims
    // `list` the production sweep calls, in the table `list` scans.
    let claims_root = dir.path().join(".fno").join("claims");
    let rec = crate::claims::ClaimRecord {
        schema_version: crate::claims::SCHEMA_VERSION,
        key: "node:x-dddd".into(),
        holder: "spawn-handover:t-x-dddd-glm".into(),
        acquired_at: crate::claims::now_ms(),
        pid: Some(std::process::id() as i32),
        host: crate::claims::hostname(),
        pid_unavailable: false,
        expires_at: None,
        reason: None,
        harness: Some("claude".into()),
        session_id: Some("s-done".into()),
        pid_provenance: Some("session-prover".into()),
        machine_id: None,
        metadata: Default::default(),
    };
    crate::claim_store::seed_at_path(&claims_root.join("node%3Ax-dddd.lock"), &rec);

    let graph = graph_read(&[("s-done", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &|e| match e.harness_session_id.as_deref() {
            Some("s-done") => Some(vec![quiet.clone()]),
            _ => None,
        },
        &staged_ages_at(
            crate::daemon::now_epoch_secs(),
            &|e| match e.harness_session_id.as_deref() {
                Some("s-done") => Some(vec![quiet.clone()]),
                _ => None,
            },
        ),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
    );
    assert!(summary.retired.is_empty(), "{summary:?}");
    assert_eq!(
        summary.kept_live_claim,
        vec![(
            "t-claim".to_string(),
            "node:x-dddd (holder spawn-handover:t-x-dddd-glm)".to_string()
        )],
        "{summary:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

/// The blueprint retirement families, split by the file budget; the
/// fixtures above are the shared seams.
#[path = "gc_receipts/blueprint_retirement.rs"]
mod blueprint_retirement;

/// The additional-PR settle families (tasks 1.1 and 1.2): the three graph
/// rules, the one-GitHub-read stamp, and the pass that applies it.
#[path = "gc_receipts/additional_pr_settle.rs"]
mod additional_pr_settle;

/// The retirement-removes-the-session families: the production active-surface
/// seam runs for real against a fake `claude` on PATH.
#[path = "gc_receipts/retire_removes_session.rs"]
mod retire_removes_session;

/// The spawn-edge kind families (a PEER handoff never holds its spawner),
/// split by the file budget; the fixtures above are the shared seams.
#[path = "gc_receipts/lineage_kind.rs"]
mod lineage_kind;

/// The open-PR reap guard families: keep + resume for the driver with no
/// termination, alert for the reap that goes through on an open-PR node.
#[path = "gc_receipts/open_pr_guard.rs"]
mod open_pr_guard;
