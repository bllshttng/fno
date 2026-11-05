//! The blueprint retirement families: the probe-unread hold, the polled stop
//! confirmation's apply path, and the empty-planning-set basis. Split out of
//! `gc_receipts` (file budget); the shared fixtures resolve through it.

use super::*;
use super::{no_agents, quiet_transcript, stage_graph, staged_graph_home, uniform_ages};
use crate::gc_sweep::{self, GcSummary};

/// The classify-pass age: past the origin ruling's one idle hour, so the
/// row reads would-retire and the apply-time re-read is what holds it.
const CLASSIFY_AGE_S: i64 = crate::gc::SPAWN_IDLE_RETIRE_SECS + 100;

/// The team specimen (2026-09-14): a do-phase claude row whose transcript
/// ends mid-Edit with the badge still `working` and live. A fresh working
/// report is a turn plausibly in flight, so it blocks the tick retirement
/// even with an unanswered probe and an unchanged seq.
#[test]
fn a_live_working_report_blocks_the_tick_retirement() {
    let (_dir, home) = staged_probe_unread_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let now = chrono::Utc::now();
    crate::state::update_registry(&home.registry_json(), |r| {
        if let Some(row) = r.entries.iter_mut().find(|e| e.name == "worker-x-u1") {
            let leg = row.inside_leg.as_mut().unwrap();
            leg.state = state::InsideLegState::Working;
            leg.ttl_ms = Some(90_000);
            leg.received_at = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        }
    })
    .unwrap();
    let calls = std::cell::Cell::new(0u32);
    let age_seam = |entries: &[&state::RegistryEntry]| {
        let n = calls.get();
        calls.set(n + 1);
        entries
            .iter()
            .map(|e| {
                let age = if n == 0 { Some(CLASSIFY_AGE_S) } else { None };
                (crate::gc::row_handle(e), age)
            })
            .collect()
    };
    let summary = run_probe_unread_sweep(&home, &emitter, &age_seam, false, None);
    assert_eq!(summary.retired.len(), 0, "{:?}", summary.retired);
    let (id, detail) = summary
        .kept_probe_unread
        .iter()
        .find(|(id, _)| id == "worker-x-u1")
        .expect("the live working row holds");
    assert_eq!(id, "worker-x-u1");
    assert!(
        detail.contains("a live working report is on the row"),
        "{detail}"
    );
    assert!(summary.kept_active.is_empty(), "{:?}", summary.kept_active);
}

/// AC1-ERR: same row, but between classification and the re-read the
/// inside-leg seq ADVANCED (a new turn report landed). The row keeps in
/// `kept_probe_unread` with a `probe unread` hold naming the seq move; it
/// never appears in kept_active.
#[test]
fn an_unanswered_re_read_after_a_new_turn_holds_by_name() {
    let (_dir, home) = staged_probe_unread_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let reg = home.registry_json();
    let calls = std::cell::Cell::new(0u32);
    let age_seam = move |entries: &[&state::RegistryEntry]| {
        let n = calls.get();
        calls.set(n + 1);
        if n > 0 {
            // The staged timeout: and while the probe starves, a new turn
            // report lands - the seq advances.
            crate::state::update_registry(&reg, |r| {
                if let Some(row) = r.entries.iter_mut().find(|e| e.name == "worker-x-u1") {
                    if let Some(leg) = row.inside_leg.as_mut() {
                        leg.seq += 1;
                    }
                }
            })
            .unwrap();
        }
        entries
            .iter()
            .map(|e| {
                let age = if n == 0 { Some(CLASSIFY_AGE_S) } else { None };
                (crate::gc::row_handle(e), age)
            })
            .collect()
    };
    let summary = run_probe_unread_sweep(&home, &emitter, &age_seam, false, None);
    assert_eq!(summary.retired.len(), 0, "{:?}", summary.retired);
    let (id, detail) = summary
        .kept_probe_unread
        .iter()
        .find(|(id, _)| id == "worker-x-u1")
        .expect("the row holds as probe unread");
    assert_eq!(id, "worker-x-u1");
    assert!(
        detail.contains("seq moved 4 -> 5"),
        "the detail names the seq change: {detail}"
    );
    let hold = summary
        .holds
        .iter()
        .find(|h| h.id == "worker-x-u1")
        .expect("a hold rides the bucket");
    assert_eq!(hold.reason, "probe unread");
    assert!(summary.kept_active.is_empty(), "{:?}", summary.kept_active);
}

/// The 2026-11-04 specimens: hand-started rows with an empty created_at, an
/// exit stamp, no pid, no inside-leg report and no transcript the probe can
/// read. An unread probe is an unknown, so the transcript on disk decides: a
/// row whose transcript is gone retires at any age, and a row whose
/// transcript file was just written holds by name. The dry run makes the same
/// re-read, so it predicts the live split instead of promising all three.
#[test]
fn an_unread_probe_defers_to_the_transcript_on_disk_and_the_dry_run_agrees() {
    let (dir, home) = staged_probe_unread_home();
    let session = |sid: &str| {
        json!({
            "phase": "execute",
            "harness": "claude",
            "session_id": sid,
            "started_at": "2026-09-01T00:00:00Z",
            "ended_at": "2026-09-01T01:00:00Z",
        })
    };
    stage_graph(
        dir.path(),
        json!([{
            "id": "x-u1",
            "status": "done",
            "sessions": [session("s-u1"), session("s-u2"), session("s-u3")],
        }]),
    );
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let ago = |s: i64| {
        (chrono::Utc::now() - chrono::Duration::seconds(s))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    };
    let fresh = dir.path().join("s-u3.jsonl");
    std::fs::write(&fresh, "{}\n").unwrap();
    let gone = dir.path().join("deleted.jsonl");
    crate::state::update_registry(&home.registry_json(), |r| {
        let template = r.entries[0].clone();
        r.entries.clear();
        for (name, sid, exited, transcript) in [
            ("t-old", "s-u1", ago(2 * 86_400), &gone),
            ("t-gone", "s-u2", ago(3 * 3_600), &gone),
            ("t-fresh", "s-u3", ago(3 * 3_600), &fresh),
        ] {
            let mut e = template.clone();
            e.name = name.into();
            e.short_id = name.into();
            e.harness_session_id = Some(sid.into());
            e.created_at = String::new();
            e.inside_leg = None;
            e.status = crate::AgentStatus::Exited;
            e.transcript_path = Some(transcript.to_string_lossy().into_owned());
            // The live specimens carry their last report stamp; it is the
            // classifier's clock when no transcript answers.
            e.last_report_at = Some(exited.clone());
            e.exited_at = Some(exited);
            r.entries.push(e);
        }
    })
    .unwrap();
    let unread = |entries: &[&state::RegistryEntry]| {
        entries
            .iter()
            .map(|e| (crate::gc::row_handle(e), None))
            .collect()
    };
    let held = |s: &GcSummary| -> Vec<String> {
        s.kept_probe_unread
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    };
    let ids = |s: &GcSummary| -> Vec<String> {
        let mut ids: Vec<String> = s.retired.iter().map(|(id, _)| id.clone()).collect();
        ids.sort();
        ids
    };
    // Unpreviewed, the rehearsal names the surface gate it cannot apply.
    let blind = run_probe_unread_sweep(&home, &emitter, &unread, true, None);
    assert!(ids(&blind).is_empty(), "{blind:?}");
    let clear = |_: &state::RegistryEntry| true;
    let dry = run_probe_unread_sweep(&home, &emitter, &unread, true, Some(&clear));
    assert_eq!(held(&dry), vec!["t-fresh".to_string()], "{dry:?}");
    assert!(dry.dry_run_unverified.is_empty(), "{dry:?}");
    let live = run_probe_unread_sweep(&home, &emitter, &unread, false, None);
    assert_eq!(held(&live), vec!["t-fresh".to_string()], "{live:?}");
    assert_eq!(ids(&live), ["t-gone", "t-old"], "{:?}", live.retired);
    assert_eq!(
        ids(&dry),
        ids(&live),
        "the previewed dry run predicts the live sweep"
    );
    let hold = live.holds.iter().find(|h| h.id == "t-fresh").expect("hold");
    assert_eq!(hold.age_basis, "row exited");
    assert!(
        hold.detail.contains("transcript on disk"),
        "{}",
        hold.detail
    );
}

/// AC4-EDGE: a `bp-` row whose planning set is EMPTY retires through
/// the session release - the basis uses the session arm, never the planning
/// wording. The `released` spelling is reserved for hold_released.
#[test]
fn a_bp_row_with_an_empty_planning_set_retires_on_the_session_basis() {
    use crate::daemon::CascadeOutcome;

    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    // The node exists but names no sessions row for s-rel, so the reverse
    // join gives the bp row an EMPTY assignment set; the name route still
    // resolves the node.
    stage_graph(
        dir.path(),
        json!([{
            "id": "x-a75f",
            "status": "idea",
            "project": "p",
        }]),
    );
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "bp-x-a75f-repro".into();
        e.short_id = "bp-x-a75f-repro".into();
        e.origin = Some("spawn".into());
        e.harness = Some("codex".into());
        e.harness_session_id = Some("s-rel".into());
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
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| CascadeOutcome::Removed,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
    );
    assert_eq!(summary.retired.len(), 1, "{:?}", summary.retired);
    assert!(
        summary.retired[0].1.starts_with("node x-a75f idea; idle "),
        "{:?}",
        summary.retired[0].1
    );
    assert!(
        !summary.retired[0].1.contains("released"),
        "the planning wording stays reserved: {:?}",
        summary.retired[0].1
    );
}

/// The AC1 fixture: one claude thread row (`pid` null is the
/// default), `inside_leg` present, staged on a done node so classification
/// answers would-retire on its quiet age.
fn staged_probe_unread_home() -> (tempfile::TempDir, AgentsHome) {
    let (dir, home) = staged_graph_home();
    stage_graph(
        dir.path(),
        json!([{
            "id": "x-u1",
            "status": "done",
            "sessions": [{
                "phase": "execute",
                "harness": "claude",
                "session_id": "s-u1",
                "started_at": "2026-09-01T00:00:00Z",
                "ended_at": "2026-09-01T01:00:00Z",
            }],
        }]),
    );
    crate::state::update_registry(&home.registry_json(), |r| {
        let mut e = state::RegistryEntry::default();
        e.name = "worker-x-u1".into();
        e.short_id = "worker-x-u1".into();
        e.origin = Some("spawn".into());
        e.harness = Some("claude".into());
        e.harness_session_id = Some("s-u1".into());
        e.created_at = "2026-09-01T00:00:00Z".into();
        e.inside_leg = Some(state::InsideLegReport {
            state: state::InsideLegState::Done,
            seq: 4,
            reason: None,
            received_at: "2026-09-01T00:00:00Z".into(),
            ttl_ms: None,
            posture: None,
        });
        r.entries.push(e);
    })
    .unwrap();
    (dir, home)
}

/// The AC1 sweep shape: grace 900, apply mode, a stop seam that confirms,
/// and the age seam handed in per test (batch answers, single re-read
/// stages the timeout).
fn run_probe_unread_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    age_seam: &dyn Fn(&[&state::RegistryEntry]) -> std::collections::HashMap<String, Option<i64>>,
    dry_run: bool,
    surface_preview: Option<&dyn Fn(&state::RegistryEntry) -> bool>,
) -> GcSummary {
    gc_sweep::run_with_release(
        home,
        emitter,
        900,
        dry_run,
        7,
        &crate::gc_sweep::read_graph_entries,
        &|_| Some(vec![]),
        age_seam,
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::Removed,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &no_agents,
        &|_| (Some(true), Some(true)),
        &|_| None,
        false,
        None,
        surface_preview,
    )
}
