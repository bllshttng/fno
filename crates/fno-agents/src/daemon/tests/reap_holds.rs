//! The x-e3cc test families: the hold clock on every reaper hold, the
//! release verb's rulings and refusals. Same-module helpers resolve through
//! the parent glob; gc_receipts' fixtures are `pub(super)`.

use super::gc_receipts::*;
use super::*;
use crate::gc_sweep::{self, GcSummary, GraphRead};
use std::collections::HashMap;

// ── x-e3cc: every hold carries an age, a basis and an escalation ─────────

fn find_hold<'a>(summary: &'a GcSummary, id: &str) -> &'a crate::gc_sweep::Hold {
    summary
        .holds
        .iter()
        .find(|h| h.id == id)
        .unwrap_or_else(|| panic!("no hold for {id} in {:?}", summary.holds))
}

fn assert_hold_line(summary: &GcSummary, dry_run: bool, id: &str, needle: &str) {
    let text = crate::reap_render::render_reap(summary, false, dry_run);
    let line = text
        .lines()
        .find(|l| l.contains(&format!("kept {id} (")) || l.contains(&format!("held {id} (")))
        .unwrap_or_else(|| panic!("no hold line for {id} in:\n{text}"));
    assert!(line.contains(needle), "line for {id}: {line}");
}

/// AC2-HP: one row held under each of sources disagree, transcript
/// unresolved, open do row and needs live stop in one dry run; every hold
/// line found by its id carries `[held ` and an age basis, and the summary's
/// `holds` projection answers for all four.
#[test]
fn ac2_hp_every_hold_line_carries_an_age_and_basis() {
    let home = tmp_home("gc-ac2-hp");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        // sources disagree: sessions names x-aaaa, the registry field x-cccc.
        let mut conf = ask_row("conf-row", None);
        conf.short_id = "confrow".into();
        conf.node = Some("x-cccc".into());
        conf.harness_session_id = Some("sess-conf".into());
        conf.origin = Some("spawn".into());
        // The verdict door reads the stored status first; these scenarios vary
        // the vendor word, so the row itself must be fno-undecided.
        conf.status = crate::AgentStatus::Idle;
        r.entries.push(conf);
        // transcript unresolved: the age seam answers nothing for this row.
        let mut tu = ask_row("tu-row", None);
        tu.short_id = "turow".into();
        tu.harness_session_id = Some("sess-tu".into());
        tu.origin = Some("spawn".into());
        // The verdict door reads the stored status first; these scenarios vary
        // the vendor word, so the row itself must be fno-undecided.
        tu.status = crate::AgentStatus::Idle;
        r.entries.push(tu);
        // open do row on a done node.
        let mut odr = ask_row("odr-row", None);
        odr.short_id = "odrrow".into();
        odr.harness_session_id = Some("sess-odr".into());
        odr.origin = Some("spawn".into());
        // The verdict door reads the stored status first; these scenarios vary
        // the vendor word, so the row itself must be fno-undecided.
        odr.status = crate::AgentStatus::Idle;
        r.entries.push(odr);
        // needs live stop: a claude row a dry run cannot promise a stop for.
        let nls = claude_worker_row("nls-row", "nlsrow");
        r.entries.push(nls);
    })
    .unwrap();

    let summary = evidence_sweep(
        &home,
        &emitter,
        900,
        true,
        graph_read(
            &[
                ("sess-conf", "x-aaaa", "done"),
                ("x-cccc", "x-cccc", "done"),
                ("sess-odr", "N1", "done"),
                ("sess-tu", "N2", "done"),
                ("nlsrow-1111-2222-3333-444444444444", "N3", "done"),
            ],
            &[("sess-odr", "N1")],
        ),
        &|e| match e.harness_session_id.as_deref() {
            Some("sess-tu") => None,
            _ => Some(vec![quiet.clone()]),
        },
        no_agents(),
        &|_| false,
    );

    let ids: Vec<&str> = summary.holds.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids.len(), 4, "holds: {:?}", summary.holds);
    for (id, reason, basis) in [
        ("confrow", "sources disagree", "transcript quiet"),
        ("turow", "transcript unresolved", "row created"),
        ("odrrow", "open do row on done node", "transcript quiet"),
        ("nlsrow", "needs live stop", "transcript quiet"),
    ] {
        let h = find_hold(&summary, id);
        assert!(h.reason.contains(reason), "{id}: {:?}", h);
        assert!(h.age_s.is_some(), "{id}: {:?}", h);
        assert_eq!(h.age_basis, basis, "{id}: {:?}", h);
        // The TU line renders main's x-1b90 clock (`for {age}`); the other
        // holds render the [held ...] suffix.
        if id == "turow" {
            assert_hold_line(&summary, true, id, "transcript unresolved for");
        } else {
            assert_hold_line(&summary, true, id, "[held ");
        }
    }
}

/// AC2-ESC: a hold aged 5401s past a 5400s threshold is escalated in the
/// JSON and its text line names the release verb.
#[test]
fn ac2_esc_escalated_hold_names_the_release_verb() {
    let home = tmp_home("gc-ac2-esc");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    state::update_registry(&home.registry_json(), |r| {
        let mut row = ask_row("esc-row", None);
        row.short_id = "escrow".into();
        row.node = Some("x-cccc".into());
        row.harness_session_id = Some("sess-esc".into());
        row.origin = Some("spawn".into());
        r.entries.push(row);
    })
    .unwrap();

    let graph = graph_read(
        &[("sess-esc", "x-aaaa", "done"), ("x-cccc", "x-cccc", "done")],
        &[],
    );
    let mut summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        true,
        7,
        &move |_| graph.clone(),
        &|_| None,
        &uniform_ages(5401),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
    );
    summary.mark_escalated(std::time::Duration::from_secs(5400));

    let h = find_hold(&summary, "escrow");
    assert_eq!(h.reason, "sources disagree");
    assert_eq!(h.age_s, Some(5401));
    assert!(h.escalated, "{h:?}");
    let text = crate::reap_render::render_reap(&summary, false, true);
    assert!(
        text.contains(&format!("fno agents reap --release escrow")),
        "text: {text}"
    );
    let json_text = crate::reap_render::render_reap(&summary, true, true);
    assert!(
        json_text.contains("\"escalated\":true"),
        "json: {json_text}"
    );
    let h2 = find_hold(&summary, "escrow");
    assert!(h2.escalated, "{h2:?}");
}

/// AC2-EDGE: a `transcript unresolved` row whose `created_at` does not
/// parse is `unmeasured` and never escalates.
#[test]
fn ac2_edge_unmeasured_hold_never_escalates() {
    let home = tmp_home("gc-ac2-unmeasured");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    state::update_registry(&home.registry_json(), |r| {
        let mut row = ask_row("unm-row", None);
        row.short_id = "unmrow".into();
        row.harness_session_id = Some("sess-unm".into());
        row.origin = Some("spawn".into());
        row.created_at = "not-a-stamp".into();
        r.entries.push(row);
    })
    .unwrap();

    let graph = graph_read(&[("sess-unm", "N1", "done")], &[]);
    let mut summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        true,
        7,
        &move |_| graph.clone(),
        &|_| None,
        &staged_ages_at(crate::daemon::now_epoch_secs(), &|_| None),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
    );
    summary.mark_escalated(std::time::Duration::from_secs(5400));

    let h = find_hold(&summary, "unmrow");
    assert!(h.age_s.is_none(), "{h:?}");
    assert_eq!(h.age_basis, "unmeasured");
    assert!(!h.escalated, "{h:?}");
    // The projection carries the unmeasured basis even though the TU line
    // renders main's clock.
    assert_hold_line(&summary, true, "unmrow", "transcript unresolved for");
}

// ── x-e3cc change 3: the release verb applies a ruling through the
// sweep's own door ────────────────────────────────────────────────────────

/// AC3-HP: a release retires the old transcript-unresolved row, keeps the
/// fresh hold, and keeps the active row - all in that one summary.
#[test]
fn ac3_hp_the_release_retires_the_ruled_row_and_keeps_the_rest() {
    let home = tmp_home("gc-ac3-hp");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let fresh = quiet_transcript(transcripts.path(), "fresh.jsonl", 30);
    let stamp = |ago_s: i64| {
        (chrono::Utc::now() - chrono::Duration::seconds(ago_s))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    state::update_registry(&home.registry_json(), |r| {
        let mut old = ask_row("old-row", None);
        old.short_id = "oldrow".into();
        old.harness_session_id = Some("sess-old".into());
        old.origin = Some("spawn".into());
        old.created_at = stamp(7200);
        // The verdict door reads the stored status first; these scenarios vary
        // the vendor word, so the row itself must be fno-undecided.
        old.status = crate::AgentStatus::Idle;
        r.entries.push(old);
        let mut mid = ask_row("mid-row", None);
        mid.short_id = "midrow".into();
        mid.harness_session_id = Some("sess-mid".into());
        mid.origin = Some("spawn".into());
        mid.created_at = stamp(60);
        // The verdict door reads the stored status first; these scenarios vary
        // the vendor word, so the row itself must be fno-undecided.
        mid.status = crate::AgentStatus::Idle;
        r.entries.push(mid);
        let mut act = ask_row("act-row", None);
        act.short_id = "actrow".into();
        act.harness_session_id = Some("sess-act".into());
        act.origin = Some("spawn".into());
        // The verdict door reads the stored status first; these scenarios vary
        // the vendor word, so the row itself must be fno-undecided.
        act.status = crate::AgentStatus::Idle;
        r.entries.push(act);
    })
    .unwrap();

    let graph = graph_read(
        &[
            ("sess-old", "N1", "done"),
            ("sess-mid", "N2", "done"),
            ("sess-act", "N3", "done"),
        ],
        &[],
    );
    let release = gc_sweep::Release {
        handle: "oldrow".to_string(),
        reason: "transcript unresolved".to_string(),
        detail: "absence is not quiet".to_string(),
    };
    let summary = gc_sweep::run_with_release(
        &home,
        &emitter,
        900,
        false,
        7,
        &move |_| graph.clone(),
        &|_| None,
        &staged_ages_at(
            crate::daemon::now_epoch_secs(),
            &|e| match e.harness_session_id.as_deref() {
                Some("sess-act") => Some(vec![fresh.clone()]),
                _ => None,
            },
        ),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
        Some(&release),
    );

    let (id, basis) = &summary.retired[0];
    assert_eq!(id, "oldrow");
    assert!(
        basis.starts_with("released transcript unresolved held 2h"),
        "basis: {basis}"
    );
    assert!(
        summary
            .kept_transcript_unresolved
            .iter()
            .any(|h| h.id == "midrow"),
        "{:?}",
        summary.kept_transcript_unresolved
    );
    assert!(
        summary.kept_active.iter().any(|(id, _)| id == "actrow"),
        "{:?}",
        summary.kept_active
    );
}

/// AC3-EDGE, the changed hold: a release whose captured hold no longer
/// matches the row's current hold keeps the row and names the change.
#[test]
fn ac3_edge_a_changed_hold_refuses_the_release() {
    let home = tmp_home("gc-ac3-changed");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    state::update_registry(&home.registry_json(), |r| {
        let mut row = ask_row("chg-row", None);
        row.short_id = "chgrow".into();
        row.harness_session_id = Some("sess-chg".into());
        row.origin = Some("spawn".into());
        r.entries.push(row);
    })
    .unwrap();

    let graph = graph_read(&[("sess-chg", "N1", "done")], &[]);
    let release = gc_sweep::Release {
        handle: "chgrow".to_string(),
        reason: "sources disagree".to_string(),
        detail: "sessions x-aaaa vs registry x-cccc".to_string(),
    };
    let summary = gc_sweep::run_with_release(
        &home,
        &emitter,
        900,
        true,
        7,
        &move |_| graph.clone(),
        &|_| None,
        &staged_ages_at(crate::daemon::now_epoch_secs(), &|_| None),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
        Some(&release),
    );

    assert!(summary.retired.is_empty(), "{:?}", summary.retired);
    assert!(
        summary
            .release_refused
            .iter()
            .any(|line| line.contains("release refused: hold changed from sources disagree")),
        "{:?}",
        summary.release_refused
    );
    let text = crate::reap_render::render_reap(&summary, false, true);
    assert!(
        text.contains("release refused: hold changed"),
        "text: {text}"
    );
}

/// AC3-ERR, the fresh hold: the verb refuses with exit 2 and the named
/// line, built by the pure refusal builders the tests assert on.
#[test]
fn ac3_err_the_verb_refuses_a_fresh_hold_below_the_threshold() {
    let (dir, home) = staged_graph_home();
    stage_graph(
        dir.path(),
        json!([done_node(
            "N1",
            json!("merged"),
            json!([]),
            vec![open_do_row("claude", "sess-fresh")],
        )]),
    );
    let created = (chrono::Utc::now() - chrono::Duration::seconds(60))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    state::update_registry(&home.registry_json(), |r| {
        let mut row = ask_row("fresh-row", None);
        row.short_id = "freshrow".into();
        row.harness_session_id = Some("sess-fresh".into());
        row.origin = Some("spawn".into());
        row.created_at = created.clone();
        r.entries.push(row);
    })
    .unwrap();

    let code = crate::reap_release::run(&home, dir.path(), "freshrow");
    assert_eq!(code, 2);

    // The refusal line, asserted through its builder so the wording is
    // pinned without capturing stderr.
    let mut dry = gc_sweep_dry_run(&home, 900);
    dry.mark_escalated(std::time::Duration::from_secs(5400));
    let hold = dry.holds.iter().find(|h| h.id == "freshrow").unwrap();
    assert!(!hold.escalated);
    let message = crate::reap_release::fresh_hold_refusal(hold, 5400);
    assert!(
        message.contains("below agents.hold_escalate_after_s"),
        "{message}"
    );
}

/// AC3-ERR, open work: an escalated `sources disagree` hold whose witness
/// node reads ready refuses; a release never retires open work.
#[test]
fn ac3_err_the_verb_refuses_a_not_done_witness() {
    let (dir, home) = staged_graph_home();
    let sessions_node = json!({
        "id": "x-84b2",
        "status": "ready",
        "additional_prs": [],
        "sessions": [open_do_row("claude", "sess-wit")],
    });
    let registry_node = json!({
        "id": "x-cccc",
        "status": "done",
        "merge_status": "merged",
        "additional_prs": [],
        "sessions": [],
    });
    stage_graph(dir.path(), json!([sessions_node, registry_node]));
    let created = (chrono::Utc::now() - chrono::Duration::seconds(7200))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    state::update_registry(&home.registry_json(), |r| {
        let mut row = ask_row("wit-row", None);
        row.short_id = "witrow".into();
        row.node = Some("x-cccc".into());
        row.harness_session_id = Some("sess-wit".into());
        row.origin = Some("spawn".into());
        row.created_at = created.clone();
        r.entries.push(row);
    })
    .unwrap();

    let code = crate::reap_release::run(&home, dir.path(), "witrow");
    assert_eq!(code, 2);
    // And the refusal names the open node through the pure builder.
    let mut dry = gc_sweep_dry_run(&home, 900);
    dry.mark_escalated(std::time::Duration::from_secs(5400));
    eprintln!("HOLDS: {:?}", dry.holds);
    eprintln!("CONFLICTS: {:?}", dry.kept_node_conflict);
    eprintln!("OPENWORK: {:?}", dry.kept_open_work);
    let hold = dry.holds.iter().find(|h| h.id == "witrow").unwrap();
    assert_eq!(hold.reason, "sources disagree");
    let refusal = crate::reap_release::witness_refusal(&home, hold).unwrap();
    assert!(
        refusal.contains("x-84b2 reads ready; a release never retires open work"),
        "{refusal}"
    );
}

// ── the open-PR keep through the sweep ──────────────────────────────────────

/// A stopped claude spawn row whose session has a do row on an in_review
/// node carrying pr_number 1943 (merge_status unrecorded): the sweep keeps
/// the row under `open pr`, projects a hold with a clock, and exposes the
/// nudge-ladder row. A peer without a do row on the node changes nothing.
#[test]
fn ac1_hp_open_pr_keep_survives_a_terminal_state_through_the_sweep() {
    let home = tmp_home("gc-open-pr");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 2 * 3600);
    state::update_registry(&home.registry_json(), |r| {
        let mut row = claude_worker_row("pr-row", "cccc9999");
        row.origin = Some("spawn".into());
        r.entries.push(row);
    })
    .unwrap();
    let sid = "cccc9999-1111-2222-3333-444444444444";
    let graph = Some(GraphRead {
        index: HashMap::from([(
            sid.to_string(),
            vec![("x-node".to_string(), "in_review".to_string())],
        )]),
        work_index: HashMap::from([(
            sid.to_string(),
            vec![("x-node".to_string(), "in_review".to_string())],
        )]),
        statuses: HashMap::from([("x-node".to_string(), "in_review".to_string())]),
        pr_state: HashMap::from([("x-node".to_string(), (None, 0, 0))]),
        pr_number: HashMap::from([("x-node".to_string(), Some(1943))]),
        do_nodes: HashMap::from([(
            sid.to_string(),
            std::collections::HashSet::from(["x-node".to_string()]),
        )]),
        // The row is quiet past the grace, so the keep asks the PR: stage
        // the open answer the sweep must read (no test touches the network).
        pr_reads: HashMap::from([("/tmp".to_string(), 1943u64)])
            .into_iter()
            .map(|(cwd, pr)| ((cwd, pr), Some(true)))
            .collect(),
        ..Default::default()
    });
    // The roster reads stopped: the exact state that reaped seven rows on
    // 2026-09-13 before the keep existed.
    let agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("cccc9999", Some("stopped")),
    ]);
    let summary = evidence_sweep(
        &home,
        &emitter,
        900,
        false,
        graph,
        &|_| Some(vec![quiet.clone()]),
        agents,
        &|_| true,
    );
    // The row handle is the short id when one is recorded.
    assert_eq!(
        summary.kept_open_pr,
        vec![("cccc9999".to_string(), "x-node".to_string())],
        "kept buckets: {:?}",
        summary.kept_open_work
    );
    let hold = find_hold(&summary, "cccc9999");
    assert_eq!(hold.reason, "open pr");
    assert_eq!(hold.detail, "x-node #1943");
    assert_eq!(summary.open_pr_rows.len(), 1, "{:?}", summary.open_pr_rows);
    let ladder_row = &summary.open_pr_rows[0];
    assert_eq!(ladder_row.node, "x-node");
    assert_eq!(ladder_row.pr, Some(1943));
    assert_eq!(ladder_row.session_id, sid);
    // A stopped roster state reads as not live: the ladder's resume arm.
    assert!(!ladder_row.live);
    assert!(!ladder_row.busy, "a stopped row is never mid-turn");
    assert_eq!(summary.retired, vec![], "nothing retires");
    std::fs::remove_dir_all(home.root()).ok();
}

// ── the dead-worker keep (dead open work) through the sweep ────────────────

/// The staged shape every dead-open-work test varies: a spawn row whose
/// session has a do row on an in_progress node with no PR, a transcript
/// quiet past the grace, and a roster the test stages. The peer roster row
/// carries a pid so the listing provably carries pids: the missing pid on
/// the target row is then a death witness, not a missing column.
fn dead_work_sweep(
    tag: &str,
    roster: crate::claude_roster::ClaudeAgentsSnapshot,
    peer_row: bool,
) -> crate::gc_sweep::GcSummary {
    let home = tmp_home(tag);
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 2 * 3600);
    // The live-peer map only trusts a peer whose transcript is inside the
    // grace window, so the peer row reads fresh while the dead row stays
    // quiet past it.
    let fresh = quiet_transcript(transcripts.path(), "fresh.jsonl", 60);
    state::update_registry(&home.registry_json(), |r| {
        let mut row = claude_worker_row("dead-row", "cccc9999");
        row.origin = Some("spawn".into());
        r.entries.push(row);
        if peer_row {
            let mut peer = claude_worker_row("fresh-row", "dddd0000");
            peer.origin = Some("spawn".into());
            peer.created_at = "2026-09-26T00:00:00Z".into();
            r.entries.push(peer);
        }
    })
    .unwrap();
    let sid = "cccc9999-1111-2222-3333-444444444444";
    let mut index = HashMap::from([(
        sid.to_string(),
        vec![("x-node".to_string(), "in_progress".to_string())],
    )]);
    let mut do_nodes = HashMap::from([(
        sid.to_string(),
        std::collections::HashSet::from(["x-node".to_string()]),
    )]);
    if peer_row {
        let peer_sid = "dddd0000-1111-2222-3333-444444444444";
        index.insert(
            peer_sid.to_string(),
            vec![("x-node".to_string(), "in_progress".to_string())],
        );
        do_nodes.insert(
            peer_sid.to_string(),
            std::collections::HashSet::from(["x-node".to_string()]),
        );
    }
    let graph = Some(GraphRead {
        index: index.clone(),
        work_index: index,
        statuses: HashMap::from([("x-node".to_string(), "in_progress".to_string())]),
        pr_state: HashMap::from([("x-node".to_string(), (None, 0, 0))]),
        pr_number: HashMap::from([("x-node".to_string(), None)]),
        do_nodes,
        pr_reads: HashMap::new(),
        ..Default::default()
    });
    evidence_sweep(
        &home,
        &emitter,
        900,
        false,
        graph,
        &|e: &crate::state::RegistryEntry| {
            if e.name == "fresh-row" {
                Some(vec![fresh.clone()])
            } else {
                Some(vec![quiet.clone()])
            }
        },
        roster,
        &|_| true,
    )
}

/// AC2-HP: the worker's process is gone (blocked row, no pid, in a listing
/// that carries pids), the node is in_progress with no PR. The row is kept
/// under `dead open work`, projected into `dead_work_rows` with pr: null and
/// live: false, and nothing retires: law d-71d03643, resumed never stranded.
#[test]
fn ac2_hp_dead_worker_on_in_progress_node_is_kept_and_laddered() {
    let agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("cccc9999", Some("blocked")),
        crate::claude_roster::ClaudeAgentRow::new("peer0001", Some("working")).with_pid(Some(5001)),
    ]);
    let summary = dead_work_sweep("gc-dead-work", agents, false);
    assert!(
        summary
            .kept_open_work
            .iter()
            .any(|(id, node, status, _)| id == "cccc9999"
                && node == "x-node"
                && status == "in_progress"),
        "kept buckets: {:?}",
        summary.kept_open_work
    );
    let hold = find_hold(&summary, "cccc9999");
    assert_eq!(hold.reason, "dead open work");
    assert_eq!(summary.retired, vec![], "nothing retires");
    assert_eq!(
        summary.dead_work_rows.len(),
        1,
        "{:?}",
        summary.dead_work_rows
    );
    let dead = &summary.dead_work_rows[0];
    assert_eq!(dead.id, "cccc9999");
    assert_eq!(dead.node, "x-node");
    assert_eq!(dead.pr, None, "no PR exists yet");
    assert_eq!(dead.session_id, "cccc9999-1111-2222-3333-444444444444");
    assert!(!dead.live, "the ladder's Resume rung reads this");
    assert!(summary.open_pr_rows.is_empty(), "not an open-PR row");
    // shape 1: a transcript-guessed row is not the node's dead
    // worker. No sessions row, no registry node, no name match - the node
    // resolves only through the transcript mention - so the rung must not
    // hold or ladder it, and the ordinary open-work stale keep answers.
    {
        let home = tmp_home("gc-dead-work-guess");
        let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
        let transcripts = tempfile::tempdir().unwrap();
        let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 2 * 3600);
        let mention = transcripts.path().join("mention.jsonl");
        std::fs::write(
            &mention,
            "{\"type\":\"user\",\"message\":{\"content\":\"drive x-abcd to a PR\"}}",
        )
        .unwrap();
        state::update_registry(&home.registry_json(), |r| {
            let mut row = claude_worker_row("guess-row", "eeee1111");
            row.origin = Some("spawn".into());
            r.entries.push(row);
        })
        .unwrap();
        let guess_sid = "eeee1111-1111-2222-3333-444444444444";
        let graph = Some(GraphRead {
            statuses: HashMap::from([
                ("x-node".to_string(), "in_progress".to_string()),
                ("x-abcd".to_string(), "in_progress".to_string()),
            ]),
            pr_state: HashMap::from([
                ("x-node".to_string(), (None, 0, 0)),
                ("x-abcd".to_string(), (None, 0, 0)),
            ]),
            pr_number: HashMap::from([("x-node".to_string(), None), ("x-abcd".to_string(), None)]),
            ..Default::default()
        });
        let summary = evidence_sweep(
            &home,
            &emitter,
            900,
            false,
            graph,
            &|e: &crate::state::RegistryEntry| {
                if e.harness_session_id.as_deref() == Some(guess_sid) {
                    Some(vec![mention.clone()])
                } else {
                    Some(vec![quiet.clone()])
                }
            },
            crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new("eeee1111", Some("blocked")),
                crate::claude_roster::ClaudeAgentRow::new("peer0001", Some("working"))
                    .with_pid(Some(5001)),
            ]),
            &|_| true,
        );
        assert_eq!(
            summary.dead_work_rows,
            vec![],
            "a transcript-guessed row is never laddered: {:?}",
            summary.dead_work_rows
        );
        assert_eq!(summary.retired, vec![], "{:?}", summary.retired);
        assert!(
            !summary
                .holds
                .iter()
                .any(|h| h.id == "eeee1111" && h.reason == "dead open work"),
            "{:?}",
            summary.holds
        );
        assert!(
            summary
                .kept_open_work_stale
                .iter()
                .any(|(id, node, _, _)| id == "eeee1111" && node == "x-abcd"),
            "the ordinary open-work keep still names the row: {:?}",
            summary.kept_open_work_stale
        );
    }

    // shape 2: the node another live session claims is not this
    // dead row's to resume. A sessions row + do row put the row on the
    // node, and a live node claim names a DIFFERENT session, so no
    // dead-work hold or ladder row may answer.
    {
        let home = tmp_home("gc-dead-work-claimheld");
        let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
        let transcripts = tempfile::tempdir().unwrap();
        let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 2 * 3600);
        state::update_registry(&home.registry_json(), |r| {
            let mut row = claude_worker_row("held-row", "ffff2222");
            row.origin = Some("spawn".into());
            r.entries.push(row);
        })
        .unwrap();
        let held_sid = "ffff2222-1111-2222-3333-444444444444";
        let claims_dir =
            crate::claims_root::global_claims_dir().expect("tmp_home pinned a claims root");
        let rec = crate::claims::ClaimRecord {
            schema_version: crate::claims::SCHEMA_VERSION,
            key: "node:x-abcd".into(),
            holder: "spawn-handover:t-other-glm".into(),
            acquired_at: crate::claims::now_ms(),
            pid: Some(std::process::id() as i32),
            host: crate::claims::hostname(),
            pid_unavailable: false,
            expires_at: None,
            reason: None,
            harness: Some("claude".into()),
            session_id: Some("dddd9999-1111-2222-3333-444444444444".into()),
            pid_provenance: Some("session-prover".into()),
            machine_id: None,
            metadata: Default::default(),
        };
        crate::claim_store::seed_at_path(&claims_dir.join("node:x-abcd.lock"), &rec);
        let graph = Some(GraphRead {
            index: HashMap::from([(
                held_sid.to_string(),
                vec![("x-abcd".to_string(), "in_progress".to_string())],
            )]),
            work_index: HashMap::from([(
                held_sid.to_string(),
                vec![("x-abcd".to_string(), "in_progress".to_string())],
            )]),
            statuses: HashMap::from([("x-abcd".to_string(), "in_progress".to_string())]),
            pr_state: HashMap::from([("x-abcd".to_string(), (None, 0, 0))]),
            pr_number: HashMap::from([("x-abcd".to_string(), None)]),
            do_nodes: HashMap::from([(
                held_sid.to_string(),
                std::collections::HashSet::from(["x-abcd".to_string()]),
            )]),
            pr_reads: HashMap::new(),
            ..Default::default()
        });
        let summary = evidence_sweep(
            &home,
            &emitter,
            900,
            false,
            graph,
            &|_| Some(vec![quiet.clone()]),
            crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new("ffff2222", Some("blocked")),
                crate::claude_roster::ClaudeAgentRow::new("peer0001", Some("working"))
                    .with_pid(Some(5001)),
            ]),
            &|_| true,
        );
        assert_eq!(
            summary.dead_work_rows,
            vec![],
            "a claim-held node is never laddered: {:?}",
            summary.dead_work_rows
        );
        assert_eq!(summary.retired, vec![], "{:?}", summary.retired);
        assert!(
            !summary.holds.iter().any(|h| h.reason == "dead open work"),
            "{:?}",
            summary.holds
        );
        assert!(
            summary
                .kept_open_work_stale
                .iter()
                .any(|(id, node, _, _)| id == "ffff2222" && node == "x-abcd"),
            "the ordinary open-work keep still names the row: {:?}",
            summary.kept_open_work_stale
        );
    }
}

/// A blocked row WITH a pid is a live session: it keeps under the ordinary
/// open-work reasons and is never laddered as a dead worker.
#[test]
fn a_blocked_row_with_a_pid_stays_open_work_and_is_not_laddered() {
    let agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("cccc9999", Some("blocked")).with_pid(Some(5003)),
        crate::claude_roster::ClaudeAgentRow::new("peer0001", Some("working")).with_pid(Some(5001)),
    ]);
    let summary = dead_work_sweep("gc-dead-work-alive", agents, false);
    assert_eq!(summary.dead_work_rows, vec![]);
    assert_eq!(summary.retired, vec![]);
    // A live row on open work keeps under either open-work reason: the
    // 2h-old fixture transcript sits inside the retire window, so the stale
    // bucket is the one that names it.
    assert!(
        !summary.kept_open_work.is_empty() || !summary.kept_open_work_stale.is_empty(),
        "the ordinary open-work keep holds: {:?} / {:?}",
        summary.kept_open_work,
        summary.kept_open_work_stale
    );
}

// ── the open-PR keep asks the PR ─────────────────────────────────────────

/// The candidate's graph: one in_review node this session drives, PR 4242.
fn open_candidate_graph() -> GraphRead {
    let mut g = graph_read(&[("sess-pr", "N1", "in_review")], &[]).unwrap();
    g.pr_number.insert("N1".into(), Some(4242));
    g.do_nodes.insert(
        "sess-pr".into(),
        std::collections::HashSet::from(["N1".into()]),
    );
    g
}

/// The read gate and the answer fold, one fixture: a candidate inside the
/// grace window is kept without the read firing at all, and once the read
/// fires an open answer holds, merged or closed settles, both a failed read
/// and no reader hold under unread - never a retirement on an unread answer.
/// The attribution gate is unchanged: a session that never drove the node is
/// no candidate and pays no read.
#[test]
fn open_pr_verdict_asks_only_a_quiet_candidate_and_folds_the_answer() {
    let g = open_candidate_graph();
    let mut calls = 0u32;
    {
        let mut reader = |_pr: u64, _cwd: &str| -> Option<bool> {
            calls += 1;
            Some(true)
        };
        let fresh =
            gc_sweep::open_pr_verdict(&g, "sess-pr", "N1", "/tmp", false, Some(&mut reader));
        assert!(
            matches!(fresh, gc_sweep::OpenPrVerdict::Holds { .. }),
            "{fresh:?}"
        );
    }
    assert_eq!(calls, 0, "a fresh candidate pays no read");
    let mut calls2 = 0u32;
    let mut reader2 = |_pr: u64, _cwd: &str| -> Option<bool> {
        calls2 += 1;
        Some(true)
    };
    let quiet = gc_sweep::open_pr_verdict(&g, "sess-pr", "N1", "/tmp", true, Some(&mut reader2));
    assert!(matches!(quiet, gc_sweep::OpenPrVerdict::Holds { .. }));
    assert_eq!(calls2, 1);
    let mut closed = |_pr: u64, _cwd: &str| -> Option<bool> { Some(false) };
    assert!(matches!(
        gc_sweep::open_pr_verdict(&g, "sess-pr", "N1", "/tmp", true, Some(&mut closed)),
        gc_sweep::OpenPrVerdict::Settled { .. }
    ));
    let mut failed = |_pr: u64, _cwd: &str| -> Option<bool> { None };
    assert!(matches!(
        gc_sweep::open_pr_verdict(&g, "sess-pr", "N1", "/tmp", true, Some(&mut failed)),
        gc_sweep::OpenPrVerdict::Unread { .. }
    ));
    assert!(matches!(
        gc_sweep::open_pr_verdict(&g, "sess-pr", "N1", "/tmp", true, None),
        gc_sweep::OpenPrVerdict::Unread { .. }
    ));
    let mut never = |_pr: u64, _cwd: &str| -> Option<bool> { panic!("no candidate pays a read") };
    assert!(matches!(
        gc_sweep::open_pr_verdict(&g, "sess-other", "N1", "/tmp", true, Some(&mut never)),
        gc_sweep::OpenPrVerdict::None
    ));
}

// ── an adopted row keeps only while there is a session to own it ────────

/// Presence in a known snapshot keeps, whatever the registry's stale status
/// reads: the listing is the live fact, so the roster-listed adopted row
/// stays under `not a spawn row`. A codex row (no claude roster governs it)
/// with a terminal registry status takes the adopted-retire carve-out, and
/// its quiet transcript retires it.
#[test]
fn a_roster_listed_adopted_row_keeps_and_an_unlisted_one_takes_the_carve_out() {
    let home = tmp_home("gc-corpse-live");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 7200);
    state::update_registry(&home.registry_json(), |r| {
        let mut present = claude_worker_row("pr-row", "prrow000");
        present.origin = Some("adopted".into());
        r.entries.push(present);
        let mut codex = ask_row("cx-row", None);
        codex.short_id = "cxrow000".into();
        codex.harness = Some("codex".into());
        codex.harness_session_id = Some("sess-cx".into());
        codex.origin = Some("adopted".into());
        r.entries.push(codex);
    })
    .unwrap();

    let graph = graph_read(
        &[
            ("prrow000-1111-2222-3333-444444444444", "NM", "done"),
            ("sess-cx", "NM", "done"),
        ],
        &[],
    );
    let agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("prrow000", Some("idle")),
    ]);
    let summary = evidence_sweep(
        &home,
        &emitter,
        900,
        true,
        graph,
        &|_| Some(vec![quiet.clone()]),
        agents,
        &|_| true,
    );

    assert_eq!(
        summary.needs_live_stop.len(),
        1,
        "the unlisted codex row reaches the pipeline and holds at the stop proof: {summary:?}"
    );
    assert_eq!(summary.needs_live_stop[0].0, "cxrow000", "{summary:?}");
    assert_eq!(
        summary.kept_not_spawn,
        vec![("prrow000".to_string(), "adopted".to_string())],
        "the roster-listed row keeps: {summary:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

// ── a row that resolved no node releases on its own done report ─────────

// ── the receipt names a registry it cannot write ────────────────────────

/// A staged registry one version ahead of this binary: the summary carries
/// the skew and the rendered receipt names both versions and the refused
/// write, in text and JSON.
#[test]
fn a_forward_registry_names_itself_in_the_receipt() {
    let home = tmp_home("gc-skew-hp");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let forward = crate::state::REGISTRY_SCHEMA_VERSION + 1;
    let understood = crate::state::REGISTRY_SCHEMA_VERSION;
    let registry = serde_json::json!({
        "schema_version": forward,
        "agents": [{
            "name": "skew-row",
            "cwd": "/tmp",
            "status": "exited",
            "created_at": "2026-09-06T00:00:00Z",
            "harness": "claude",
            "harness_session_id": "sess-skew",
            "short_id": "skewrow1",
            "origin": "spawn",
        }],
    });
    std::fs::create_dir_all(home.root()).unwrap();
    std::fs::write(
        home.registry_json(),
        serde_json::to_string(&registry).unwrap(),
    )
    .unwrap();

    let graph = graph_read(&[("sess-skew", "N1", "done")], &[]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        true,
        7,
        &move |_| graph.clone(),
        &|_| None,
        &uniform_ages(7200),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (None, None),
        &|_| None,
    );

    assert_eq!(summary.schema_skew, Some((forward, understood)));
    let text = crate::reap_render::render_reap(&summary, false, true);
    assert!(
        text.contains(&format!(
            "schema v{forward} is ahead of the v{understood} this fno understands"
        )),
        "text: {text}"
    );
    assert!(
        text.contains("no retirement can be written"),
        "text must name the refused write: {text}"
    );
    let json_text = crate::reap_render::render_reap(&summary, true, true);
    assert!(
        json_text.contains(&format!(
            "\"schema_skew\":{{\"on_disk\":{forward},\"understood\":{understood}}}"
        )),
        "json: {json_text}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}
