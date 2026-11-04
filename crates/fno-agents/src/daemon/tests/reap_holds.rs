//! The x-e3cc test families: the hold clock on every reaper hold, the
//! release command's rulings and refusals. Same-module helpers resolve through
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

/// AC2-HP: rows whose sources disagree, whose transcript is unresolved or
/// that carry an open do row hold nothing now. The one hold left is the
/// claude row a dry run cannot promise a stop for, and its line carries
/// an age and a basis.
#[test]
fn ac2_hp_every_hold_line_carries_an_age_and_basis() {
    let home = tmp_home("gc-ac2-hp");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 2 * 3600);
    // Past the grace, inside the 30-minute done-node window, so the
    // work-state keeps still hold.
    let recent = quiet_transcript(transcripts.path(), "recent.jsonl", 1500);
    let stamp = |ago_s: i64| {
        (chrono::Utc::now() - chrono::Duration::seconds(ago_s))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
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
        tu.created_at = stamp(1500);
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
            Some("sess-conf") | Some("sess-odr") => Some(vec![recent.clone()]),
            _ => Some(vec![quiet.clone()]),
        },
        no_agents(),
        &|_| false,
    );

    let ids: Vec<&str> = summary.holds.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["nlsrow"], "holds: {:?}", summary.holds);
    let h = find_hold(&summary, "nlsrow");
    assert_eq!(h.reason, "needs live stop", "{h:?}");
    assert!(h.age_s.is_some(), "{h:?}");
    assert_eq!(h.age_basis, "transcript quiet", "{h:?}");
    assert_hold_line(&summary, true, "nlsrow", "[held ");
}

/// A summary holding one row under `needs live stop`, the hold the dry run
/// still produces, with the given clock.
fn staged_stop_hold(id: &str, age_s: Option<i64>) -> GcSummary {
    let mut summary = GcSummary::default();
    summary.needs_live_stop.push((
        id.to_string(),
        "no terminal roster state and no dead pid".into(),
    ));
    summary.holds.push(crate::gc_sweep::Hold {
        id: id.to_string(),
        reason: "needs live stop",
        detail: "no terminal roster state and no dead pid".into(),
        age_s,
        age_basis: if age_s.is_some() {
            "transcript quiet"
        } else {
            "unmeasured"
        },
        escalated: false,
    });
    summary
}

/// AC2-ESC: a hold aged 3001s past a 3000s threshold is escalated in the
/// JSON and its text line names the release command.
#[test]
fn ac2_esc_escalated_hold_names_the_release_command() {
    let mut summary = staged_stop_hold("escrow", Some(3001));
    summary.mark_escalated(std::time::Duration::from_secs(3000));

    let h = find_hold(&summary, "escrow");
    assert!(h.escalated, "{h:?}");
    let text = crate::reap_render::render_reap(&summary, false, true);
    assert!(
        text.contains("fno agents reap --release escrow"),
        "text: {text}"
    );
    let json_text = crate::reap_render::render_reap(&summary, true, true);
    assert!(
        json_text.contains("\"escalated\":true"),
        "json: {json_text}"
    );
}

/// AC2-EDGE: a hold with no clock is `unmeasured` and never escalates.
#[test]
fn ac2_edge_unmeasured_hold_never_escalates() {
    let mut summary = staged_stop_hold("unmrow", None);
    summary.mark_escalated(std::time::Duration::from_secs(5400));

    let h = find_hold(&summary, "unmrow");
    assert_eq!(h.age_basis, "unmeasured");
    assert!(!h.escalated, "{h:?}");
    assert_hold_line(&summary, true, "unmrow", "[held unmeasured]");
}

// ── x-e3cc change 3: the release command applies a ruling through the
// sweep's own door ────────────────────────────────────────────────────────

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
        // Inside the 30-minute done-node window, so the row holds as
        // transcript unresolved.
        row.created_at = (chrono::Utc::now() - chrono::Duration::seconds(1500))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
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
        false,
        Some(&release),
        None,
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

/// AC3-ERR: the command refuses a row inside its idle window (it holds
/// nothing), and a hold below the threshold names the threshold.
#[test]
fn ac3_err_the_command_refuses_a_fresh_hold_below_the_threshold() {
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

    let mut staged = staged_stop_hold("freshrow", Some(60));
    staged.mark_escalated(std::time::Duration::from_secs(5400));
    let hold = find_hold(&staged, "freshrow");
    assert!(!hold.escalated);
    let message = crate::reap_release::fresh_hold_refusal(hold, 5400);
    assert!(
        message.contains("below agents.hold_escalate_after_s"),
        "{message}"
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
    // Past the grace, so the keep asks the PR; inside the idle hour, so the
    // keep still holds.
    let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 1800);
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
        "kept buckets: {summary:?}"
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
