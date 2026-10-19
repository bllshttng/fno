//! The x-2774 session-truth family: the reaper asks the session, not only
//! the node. Moved verbatim from gc_receipts (file budget: gc_receipts grew
//! past the shrink-only line; test motion is the sanctioned shrink). Same
//! fixtures: shared `pub(super)` helpers resolve through the globs.
use super::*;

use super::gc_receipts::*;

use crate::gc_sweep::{self, GcSummary};

// ── x-2774: the reaper asks the session, not only the node ──────────────

/// An in-review node with the given session ids joined through sessions[].
fn x2774_open_node(id: &str, status: &str, sids: &[&str]) -> Value {
    let rows: Vec<Value> = sids
        .iter()
        .map(|sid| {
            json!({
                "phase": "execute",
                "harness": "claude",
                "session_id": sid,
                "started_at": "2026-09-01T01:00:00Z",
            })
        })
        .collect();
    json!({
        "id": id,
        "status": status,
        "sessions": rows,
    })
}

/// A spawn-origin claude registry row with a pinned short id.
fn x2774_spawn(name: &str, short_id: &str, sid: &str) -> state::RegistryEntry {
    let mut e = ask_row(name, None);
    e.short_id = short_id.into();
    e.harness = Some("claude".into());
    e.harness_session_id = Some(sid.into());
    e.origin = Some("spawn".into());
    // The verdict door reads the stored status first; the x2774 buckets
    // vary the vendor word, so the row itself must be fno-undecided.
    e.status = crate::AgentStatus::Idle;
    e
}

/// The x-2774 sweep harness: production graph read over a staged graph.json,
/// staged transcripts, stop confirmed, no tree.
#[allow(clippy::too_many_arguments)]
fn x2774_sweep_at(
    home: &AgentsHome,
    emitter: &EventEmitter,
    grace: i64,
    dry_run: bool,
    now: i64,
    transcripts: impl Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
    agents: crate::claude_roster::ClaudeAgentsSnapshot,
) -> GcSummary {
    gc_sweep::run(
        home,
        emitter,
        grace,
        dry_run,
        7,
        &gc_sweep::read_graph_entries,
        &transcripts,
        &staged_ages_at(now, &transcripts),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &move || agents.clone(),
        &|_| (None, None),
        &|_| None,
    )
}

/// The wall-clock wrapper: each pass reads its own now, as before the
/// injected-clock variant existed.
fn x2774_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    grace: i64,
    dry_run: bool,
    transcripts: impl Fn(&state::RegistryEntry) -> Option<Vec<std::path::PathBuf>>,
    agents: crate::claude_roster::ClaudeAgentsSnapshot,
) -> GcSummary {
    x2774_sweep_at(
        home,
        emitter,
        grace,
        dry_run,
        crate::daemon::now_epoch_secs(),
        transcripts,
        agents,
    )
}

/// Change 1 + 2: a spawn row whose node reads in_review and whose harness
/// state reads done retires with a basis naming the session state, and the
/// agent_row_reaped event carries it. `working` keeps; a failed roster read
/// keeps (fail closed).
#[test]
fn x2774_terminal_harness_state_releases_an_open_work_row() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "q.jsonl", 2 * 3600);
    stage_graph(
        dir.path(),
        json!([x2774_open_node("N1", "in_review", &["s-term"])]),
    );
    state::update_registry(&home.registry_json(), |r| {
        r.entries.push(x2774_spawn("row-term", "t-term", "s-term"));
    })
    .unwrap();
    let picks = |e: &state::RegistryEntry| match e.harness_session_id.as_deref() {
        Some("s-term") => Some(vec![quiet.clone()]),
        _ => None,
    };

    // The inverse FIRST (a dry run mutates nothing, and the apply pass below
    // retires the row): a working state keeps under open work, and the keep
    // names the reader (change 4).
    let working_agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("t-term", Some("working")),
    ]);
    let summary = x2774_sweep(&home, &emitter, 900, true, picks, working_agents);
    assert!(summary.retired.is_empty(), "{summary:?}");
    assert_eq!(
        summary.kept_open_work_stale,
        vec![(
            "t-term".to_string(),
            "N1".to_string(),
            "in_review".to_string(),
            "sessions".to_string()
        )]
    );

    // A roster read that fails outright keeps everything (change 1).
    let unreadable = crate::claude_roster::ClaudeAgentsSnapshot::unknown("staged: unreadable");
    let summary = x2774_sweep(&home, &emitter, 900, true, picks, unreadable);
    assert!(summary.retired.is_empty(), "{summary:?}");
    assert_eq!(summary.kept_open_work_stale.len(), 1, "{summary:?}");

    // Terminal roster state: the row retires on the session question. APPLY
    // mode, run LAST: the basis is an effect-outcome artifact, so
    // the claim is proved where the effects run - and the retirement drops
    // the row from the registry.
    let done_agents = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("t-term", Some("done")),
    ]);
    let summary = x2774_sweep(&home, &emitter, 900, false, picks, done_agents);
    assert_eq!(summary.retired.len(), 1, "{summary:?}");
    assert!(
        summary.retired[0]
            .1
            .starts_with("session terminal: harness state done (via sessions); node N1 in_review"),
        "basis names the session state: {summary:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

/// Change 3: one node, two spawn rows. Older quiet, newer live: the OLDER
/// retires naming the live peer, the newer keeps under active. Both quiet:
/// neither retires on the supersession path.
#[test]
fn x2774_supersession_and_its_fail_closed_corner() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "old.jsonl", 2 * 3600);
    let fresh = quiet_transcript(transcripts.path(), "new.jsonl", 100);
    stage_graph(
        dir.path(),
        json!([x2774_open_node("N1", "in_review", &["s-old", "s-new"])]),
    );
    state::update_registry(&home.registry_json(), |r| {
        let mut older = x2774_spawn("row-old", "t-old", "s-old");
        older.created_at = "2026-09-09T22:00:00Z".into();
        let mut newer = x2774_spawn("row-new", "t-new", "s-new");
        newer.created_at = "2026-09-09T23:00:00Z".into();
        r.entries.push(older);
        r.entries.push(newer);
    })
    .unwrap();
    let older_quiet = |e: &state::RegistryEntry| match e.harness_session_id.as_deref() {
        Some("s-old") => Some(vec![quiet.clone()]),
        Some("s-new") => Some(vec![fresh.clone()]),
        _ => None,
    };
    let summary = x2774_sweep(&home, &emitter, 900, false, older_quiet, no_agents());
    assert_eq!(
        summary.retired,
        vec![(
            "t-old".to_string(),
            "superseded on N1 by live peer row-new (created 2026-09-09T23:00:00Z)".to_string()
        )],
        "{summary:?}"
    );
    assert_eq!(
        summary.kept_open_work_stale,
        vec![(
            "t-new".to_string(),
            "N1".to_string(),
            "in_review".to_string(),
            "sessions".to_string()
        )],
        "the live newest row keeps under its own open-work shield: {summary:?}"
    );

    let both_quiet = |e: &state::RegistryEntry| match e.harness_session_id.as_deref() {
        Some("s-old") | Some("s-new") => Some(vec![quiet.clone()]),
        _ => None,
    };
    // Run 1 (acting) retired and removed t-old; restore it for the
    // both-quiet world.
    state::update_registry(&home.registry_json(), |r| {
        let mut older = x2774_spawn("row-old", "t-old", "s-old");
        older.created_at = "2026-09-09T22:00:00Z".into();
        r.entries.push(older);
    })
    .unwrap();
    let summary = x2774_sweep(&home, &emitter, 900, false, both_quiet, no_agents());
    assert!(summary.retired.is_empty(), "{summary:?}");
    assert_eq!(summary.kept_open_work_stale.len(), 2, "{summary:?}");
    std::fs::remove_dir_all(home.root()).ok();
}

/// Change 5: one staged world judged twice. Dry and acting agree row for
/// row on every read-only policy hold, and the permitted divergences are
/// each asserted by name: dry_run_unverified (dry only: rows with
/// provable death advance to the unevaluated active-surface gate) and
/// needs_live_stop (dry only: no positive stop evidence - the codex row's
/// stop is as unevaluatable as the claude one), plus the freshness
/// re-check (acting only, covered by
/// activity_arriving_in_the_apply_window_keeps_the_row in gc.rs), and a
/// pane row the precheck calls NeedsKill (dry only;
/// x58a5_dry_and_acting_agree_on_a_gone_pid_pane_row asserts the pane
/// buckets agree row for row).
#[test]
fn x2774_dry_and_acting_agree_row_for_row() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet = quiet_transcript(transcripts.path(), "quiet.jsonl", 2 * 3600);
    let fresh = quiet_transcript(transcripts.path(), "fresh.jsonl", 10);
    let done_node = json!({
        "id": "ND",
        "status": "done",
        "sessions": [
            {"phase": "execute", "harness": "codex", "session_id": "s-done", "started_at": "2026-09-01T01:00:00Z", "ended_at": "2026-09-01T02:00:00Z"},
            {"phase": "execute", "harness": "claude", "session_id": "s-nostop", "started_at": "2026-09-01T01:00:00Z", "ended_at": "2026-09-01T02:00:00Z"},
            {"phase": "execute", "harness": "claude", "session_id": "s-donefresh", "started_at": "2026-09-01T01:00:00Z", "ended_at": "2026-09-01T02:00:00Z"}
        ]
    });
    stage_graph(
        dir.path(),
        json!([
            done_node,
            x2774_open_node("N1", "in_review", &["s-term", "s-work"])
        ]),
    );
    state::update_registry(&home.registry_json(), |r| {
        let mut done = x2774_spawn("row-done", "t-done", "s-done");
        done.harness = Some("codex".into());
        r.entries.push(done);
        r.entries.push(x2774_spawn("row-term", "t-term", "s-term"));
        r.entries.push(x2774_spawn("row-work", "t-work", "s-work"));
        let mut nostop = x2774_spawn("row-nostop", "t-nostop", "s-nostop");
        nostop.harness = Some("claude".into());
        r.entries.push(nostop);
        r.entries
            .push(x2774_spawn("row-donefresh", "t-donefresh", "s-donefresh"));
    })
    .unwrap();
    let picks = |e: &state::RegistryEntry| match e.harness_session_id.as_deref() {
        Some("s-done") => Some(vec![quiet.clone()]),
        Some("s-donefresh") => Some(vec![fresh.clone()]),
        Some("s-nostop") | Some("s-term") | Some("s-work") => Some(vec![quiet.clone()]),
        _ => None,
    };
    let roster = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("t-term", Some("done")),
        crate::claude_roster::ClaudeAgentRow::new("t-work", Some("working")),
        crate::claude_roster::ClaudeAgentRow::new("t-nostop", Some("working")),
        crate::claude_roster::ClaudeAgentRow::new("t-donefresh", Some("done")),
    ]);
    // One now for both passes: the seam answers identical ages, so the
    // agreement asserts below hold byte for byte (CI flake 2026-09-27 read
    // active ages 10 against 11 across the two wall reads).
    let now = crate::daemon::now_epoch_secs();
    let dry = x2774_sweep_at(&home, &emitter, 900, true, now, &picks, roster.clone());
    let acting = x2774_sweep_at(&home, &emitter, 900, false, now, &picks, roster);
    assert_eq!(
        dry.kept_open_work_stale, acting.kept_open_work_stale,
        "open-work bucket agrees"
    );
    // The bucket's second element is the transcript age. Both passes judged
    // the staged world against ONE injected now, so the tuples agree byte
    // for byte; per-pass wall reads flaked here when a row crossed a second
    // boundary between the passes (CI, 2026-09-27: 10 vs 11).
    assert_eq!(
        dry.kept_active, acting.kept_active,
        "active bucket agrees row for row"
    );
    assert_eq!(
        dry.kept_no_provenance, acting.kept_no_provenance,
        "provenance bucket agrees"
    );
    assert_eq!(
        dry.kept_open_do_row, acting.kept_open_do_row,
        "the staging re-read agrees row for row"
    );
    assert_eq!(
        dry.kept_graph_unreadable, acting.kept_graph_unreadable,
        "the graph read agrees row for row"
    );
    // The one permitted boundary: dry-run reports effect-only
    // uncertainty. A row with provable death evidence advances to the
    // active-surface gate, which a rehearsal cannot evaluate, so it is
    // named in dry_run_unverified and never planted in retired. Apply
    // records the effect outcome.
    assert!(dry.retired.is_empty(), "{:?}", dry.retired);
    let mut dry_unverified: Vec<&str> = dry
        .dry_run_unverified
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    dry_unverified.sort_unstable();
    assert_eq!(
        dry_unverified,
        vec!["t-term"],
        "positive stop evidence advances to the unevaluated active-surface \
         gate; t-donefresh keeps under active because recency inside the \
         grace outranks the terminal state, so it never reaches a gate"
    );
    assert!(
        dry.dry_run_unverified
            .iter()
            .all(|(_, gate)| gate == "active-surface removal was not evaluated"),
        "{:?}",
        dry.dry_run_unverified
    );
    let mut dry_waiting: Vec<&str> = dry
        .needs_live_stop
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    dry_waiting.sort_unstable();
    assert_eq!(
        dry_waiting,
        vec!["t-done", "t-nostop"],
        "no-evidence rows wait: the codex row's stop is as unevaluatable as \
         the claude one"
    );
    let mut acting_ids: Vec<&str> = acting.retired.iter().map(|(id, _)| id.as_str()).collect();
    acting_ids.sort_unstable();
    assert_eq!(
        acting_ids,
        vec!["t-done", "t-nostop", "t-term"],
        "apply records the effect outcome: every row whose gates confirmed \
         retired; t-donefresh keeps under active because recency outranks the terminal state inside the grace"
    );
    assert!(
        acting.retired.iter().all(|(id, _)| id != "t-donefresh"),
        "a fresh transcript is never reaped as unattended: {:?}",
        acting.retired
    );
    assert_eq!(
        acting.needs_live_stop.len(),
        0,
        "{:?}",
        acting.needs_live_stop
    );
    assert_eq!(acting.stop_refused.len(), 0, "{:?}", acting.stop_refused);
    assert_eq!(
        acting.dry_run_unverified.len(),
        0,
        "apply never reports dry-run uncertainty: {:?}",
        acting.dry_run_unverified
    );
    std::fs::remove_dir_all(home.root()).ok();
}

/// x-b7f8 change 3, amended: a terminal row whose transcript is
/// inside the grace keeps AT CLASSIFICATION - the terminal state no longer
/// overrides recency, so the apply-window re-check never sees the row. The
/// decreasing age seam still answers, and the row keeps with the
/// classification age. (Past the grace the terminal state retires; that is
/// x2774_terminal_harness_state_releases_an_open_work_row.)
#[test]
fn xb7f8_activity_arriving_in_the_apply_window_keeps_a_terminal_row() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    stage_graph(
        dir.path(),
        json!([{
            "id": "NW",
            "status": "done",
            "sessions": [
                {"phase": "execute", "harness": "claude", "session_id": "s-wrote", "started_at": "2026-09-01T01:00:00Z", "ended_at": "2026-09-01T02:00:00Z"}
            ]
        }]),
    );
    state::update_registry(&home.registry_json(), |r| {
        r.entries
            .push(x2774_spawn("row-wrote", "t-wrote", "s-wrote"));
    })
    .unwrap();
    // The age seam answers 274 in the classification batch and 3 on the
    // apply-window re-read: the session wrote between the two reads.
    let counter = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let c2 = std::rc::Rc::clone(&counter);
    let age_many = move |entries: &[&state::RegistryEntry]| {
        let n = c2.get();
        c2.set(n + 1);
        entries
            .iter()
            .map(|e| (crate::gc::row_handle(e), Some(if n == 0 { 274 } else { 3 })))
            .collect::<std::collections::HashMap<String, Option<i64>>>()
    };
    let picks = |e: &state::RegistryEntry| match e.harness_session_id.as_deref() {
        Some("s-wrote") => Some(vec![]),
        _ => None,
    };
    let roster = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("t-wrote", Some("done")),
    ]);
    let roster_for_sweep = roster.clone();
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900,
        false,
        7,
        &gc_sweep::read_graph_entries,
        &picks,
        &age_many,
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &move || roster_for_sweep.clone(),
        &|_| (None, None),
        &|_| None,
    );
    assert!(
        summary.retired.is_empty(),
        "the stop never fires while activity arrived: {summary:?}"
    );
    assert_eq!(
        summary.kept_active,
        vec![("t-wrote".to_string(), 274)],
        "recency keeps at classification: {summary:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

// ── x-586d: a done ledger never overrides an attended session ───────────

/// A transcript whose rows carry a typed user turn `typed_age_s` old plus an
/// older keepalive ping. The mtime is NOT the instrument here: the sweep's
/// age seam is injected, and the test stages the incident's divergence - the
/// age answer reads quiet while the file itself holds the fresh typed turn.
fn typed_turn_transcript(
    dir: &std::path::Path,
    name: &str,
    typed_age_s: i64,
) -> std::path::PathBuf {
    use std::io::Write;
    let now = crate::daemon::now_epoch_secs();
    let ts = |age: i64| {
        chrono::DateTime::from_timestamp(now - age, 0)
            .unwrap()
            .to_rfc3339()
    };
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        "{}",
        json!({"type":"user","timestamp":ts(typed_age_s + 600),"message":{"role":"user","content":"[cache-keepalive] Ping 1/4"}})
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        json!({"type":"user","timestamp":ts(typed_age_s),"message":{"role":"user","content":"checking your notes on x-bc73 now"}})
    )
    .unwrap();
    path
}

/// The node reads done and merged, the age seam answers 2 hours quiet, and
/// the transcript carries a typed turn 2 minutes old: the sweep must hold
/// the row under kept_attended, never reap it.
#[test]
fn x586d_done_node_session_with_a_typed_turn_two_minutes_old_survives_the_sweep() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let attended = typed_turn_transcript(transcripts.path(), "s-attended.jsonl", 120);
    stage_graph(
        dir.path(),
        json!([{
            "id": "N1",
            "status": "done",
            "merge_status": "merged",
            "sessions": [
                {"phase": "execute", "harness": "claude", "session_id": "s-attended", "started_at": "2026-09-01T01:00:00Z"}
            ]
        }]),
    );
    state::update_registry(&home.registry_json(), |r| {
        r.entries
            .push(x2774_spawn("row-attended", "t-attended", "s-attended"));
    })
    .unwrap();
    let picks = |e: &state::RegistryEntry| match e.harness_session_id.as_deref() {
        Some("s-attended") => Some(vec![attended.clone()]),
        _ => None,
    };
    let roster = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("t-attended", Some("done")),
    ]);
    let summary = gc_sweep::run(
        &home,
        &emitter,
        900, // grace: 7200s is far past it, so only the attended gate holds
        false,
        7,
        &gc_sweep::read_graph_entries,
        &picks,
        &uniform_ages(2 * 3600),
        &|_| true,
        &|_| crate::daemon::CascadeOutcome::NotApplicable,
        &|_e| crate::daemon::CascadeOutcome::NotApplicable,
        &move || roster.clone(),
        &|_| (None, None),
        &|_| None,
    );
    assert!(
        summary.retired.is_empty(),
        "an attended session is never reaped: {summary:?}"
    );
    assert!(
        summary.kept_active.is_empty(),
        "the age seam read quiet, so the hold must come from the typed turn: {summary:?}"
    );
    // The detail names the typed turn with a live age: the sweep reads the
    // clock after the fixture stamps the rows, so a second can cross and
    // the exact "120s" would flake. The hold is what the contract pins.
    assert_eq!(summary.kept_attended.len(), 1, "{summary:?}");
    assert_eq!(summary.kept_attended[0].0, "t-attended");
    let detail = &summary.kept_attended[0].1;
    let age: i64 = detail
        .strip_prefix("typed user turn ")
        .and_then(|rest| rest.strip_suffix("s ago"))
        .and_then(|num| num.parse().ok())
        .unwrap_or(i64::MAX);
    assert!(
        (120..=crate::attended::WINDOW_SECS).contains(&age),
        "the attended hold names the typed turn inside the window: {summary:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

// ── x-7066: a person typed here, so the row keeps - past every window ───

// ── the keep mark: a ruling outlives the row ─────────────────────────────

/// The keep mark, in the sweep: two done, merged, quiet rows; the marked
/// twin is named under `kept_marked`, the unmarked twin retires. The gate
/// sits ahead of the origin gate and the policy, so a marked adopted row
/// and a marked-and-typed row both read as `keep mark` - placement, not a
/// second test.
#[test]
fn a_keep_mark_holds_a_quiet_done_row_the_unmarked_twin_retires() {
    let (dir, home) = staged_graph_home();
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let transcripts = tempfile::tempdir().unwrap();
    let quiet_marked = quiet_transcript(transcripts.path(), "s-marked.jsonl", 2 * 3600);
    let quiet_plain = quiet_transcript(transcripts.path(), "s-plain.jsonl", 2 * 3600);
    stage_graph(
        dir.path(),
        json!([{
            "id": "N1",
            "status": "done",
            "merge_status": "merged",
            "sessions": [
                {"phase": "execute", "harness": "claude", "session_id": "s-marked", "started_at": "2026-09-01T01:00:00Z", "ended_at": "2026-09-01T02:00:00Z"},
                {"phase": "execute", "harness": "claude", "session_id": "s-plain", "started_at": "2026-09-01T01:00:00Z", "ended_at": "2026-09-01T02:00:00Z"}
            ]
        }]),
    );
    state::update_registry(&home.registry_json(), |r| {
        r.entries
            .push(x2774_spawn("row-marked", "t-marked", "s-marked"));
        r.entries
            .push(x2774_spawn("row-plain", "t-plain", "s-plain"));
    })
    .unwrap();
    crate::keep_mark::set(&home, "s-marked", "t-marked").unwrap();
    let picks = |e: &state::RegistryEntry| match e.harness_session_id.as_deref() {
        Some("s-marked") => Some(vec![quiet_marked.clone()]),
        Some("s-plain") => Some(vec![quiet_plain.clone()]),
        _ => None,
    };
    let roster = crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
        crate::claude_roster::ClaudeAgentRow::new("t-marked", Some("done")),
        crate::claude_roster::ClaudeAgentRow::new("t-plain", Some("done")),
    ]);
    let summary = x2774_sweep_at(
        &home,
        &emitter,
        900,
        false,
        crate::daemon::now_epoch_secs(),
        &picks,
        roster,
    );
    assert_eq!(
        summary
            .retired
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["t-plain"],
        "the unmarked twin retires: {summary:?}"
    );
    assert_eq!(summary.kept_marked.len(), 1, "{summary:?}");
    assert_eq!(summary.kept_marked[0].0, "t-marked");
    assert!(
        summary.kept_marked[0].1.starts_with("keep mark: set "),
        "the keep names the set time and the setter: {summary:?}"
    );
    assert!(
        summary.kept_user_typed.is_empty(),
        "the mark prints ahead of user typed: {summary:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}
