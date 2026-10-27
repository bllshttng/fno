//! parity-stage: characterization
//! parity-oracle: fno.pr_watch._lead_wake.run_lead_wake
//!
//! Characterization for the daemon lead-wake arm's refill-respawn leg. The
//! goldens under tests/golden/lead_wake_pass/ were captured on 2026-10-27
//! from the Python pr-watch phase over the injected-fn fixture shapes of
//! cli/tests/unit/test_lead_wake_phase.py, before the phase was deleted.
//! Capture mode refuses: a golden can only be captured while the old leg
//! runs.
//!
//! The temp root folds to `<ROOT>` and the space slug to `<ROOTSLUG>`. The
//! pass runs in-process against the same seams the Python tests injected:
//! truth, unread mail, the answered records, the answer projection, the
//! graph rows, the scope read, dispatch, ask, emit, the clock and the tick
//! budget. The registry, manifests and sidecars are real files, so the
//! ledger ops (`admit-wake` under the manifest lock) run for real.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use common::golden_dir;
use fno_agents::lead_wake_triggers::{
    build_walk_argv, hash_rows, run_with, ScopeRead, Seams, TriggerConfig, TriggerOutcome,
    TruthAnswer,
};
use serde_json::{json, Value};

mod common;

/// 2026-08-29T12:00:00Z, the fixtures' fixed clock.
const NOW: i64 = 1788004800;
const NOW_ISO: &str = "2026-08-29T12:00:00Z";
const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const BIRTH: &str = "2026-08-28T12:00:00Z";
const CURSOR_SEED: &str = "2026-08-29T10:00:00Z";
const PROMPT: &str =
    r#"Answer to your question q-ab12cd34 "what does the operator want for epic-x?": ship it"#;
const ANSWER_CURSOR: &str = r#"{"ts":"2026-08-29T11:00:00Z","id":"q-ab12cd34","seq":2}"#;

/// Serializes the env-mutating fixtures. The lib's `claims::test_env_lock`
/// is #[cfg(test)]-gated, so an integration binary carries its own.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Restores the pins the leg's `space_dir` resolution reads through,
/// holding the shared env lock so a parallel test's pin swap cannot flip
/// the resolution mid-read (the lead_wake tests' guard shape).
struct PinGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl PinGuard {
    fn take(base: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = ["FNO_SPACES_DIR", fno_agents::paths::HOME_ENV]
            .iter()
            .map(|var| (*var, std::env::var_os(var)))
            .collect();
        std::env::set_var("FNO_SPACES_DIR", base.join("spaces"));
        std::env::set_var(fno_agents::paths::HOME_ENV, base.join("home"));
        Self { _lock: lock, saved }
    }
}

impl Drop for PinGuard {
    fn drop(&mut self) {
        for (var, saved) in &self.saved {
            match saved {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
    }
}

/// One fixture world: a pinned agents home, a spaces root and a project
/// root the registry rows' cwds point at.
struct World {
    tmp: tempfile::TempDir,
    _pins: PinGuard,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let pins = PinGuard::take(tmp.path());
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        fs::create_dir_all(tmp.path().join("spaces")).unwrap();
        fs::create_dir_all(tmp.path().join("proj")).unwrap();
        Self { tmp, _pins: pins }
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("proj")
    }

    /// Overwrite the registry with the given row objects and return the
    /// loaded entries.
    fn set_registry(&self, agents: Value) -> Vec<fno_agents::state::RegistryEntry> {
        let path = self.home().join("registry.json");
        fs::write(
            &path,
            json!({
                "schema_version": fno_agents::state::REGISTRY_SCHEMA_VERSION,
                "agents": agents,
            })
            .to_string(),
        )
        .unwrap();
        fno_agents::state::load_registry(&path).unwrap().entries
    }

    /// Write one scope's manifest, fixed created_at so the birth-cursor
    /// seed is deterministic; `extra` carries respawn budget or ledger
    /// lines.
    fn manifest(&self, scope: &str, extra: &str) -> PathBuf {
        let leads = fno_agents::paths::space_dir(&self.root()).join("leads");
        fs::create_dir_all(&leads).unwrap();
        let path = leads.join(format!("{scope}.md"));
        fs::write(
            &path,
            format!(
                "---\nfno_id: wake-fixture\nscope: {scope}\nharness: claude\n\
                 harness_session_id: {SESSION}\ncreated_at: {BIRTH}\n{extra}---\n"
            ),
        )
        .unwrap();
        path
    }

    /// Drop and rewrite one scope's manifest fresh, dropping the ledger
    /// bill (the shape the Python tests' `fresh_manifest=True` produced).
    fn rewrite_manifest(&self, scope: &str, extra: &str) {
        let path = self.manifest(scope, extra);
        let _ = path;
    }

    fn sidecar(&self, scope: &str) -> PathBuf {
        let leads = fno_agents::paths::space_dir(&self.root()).join("leads");
        fs::create_dir_all(&leads).unwrap();
        leads.join(format!("{scope}.wake.json"))
    }

    fn write_sidecar(&self, scope: &str, payload: Value) {
        fs::write(self.sidecar(scope), payload.to_string()).unwrap();
    }

    fn read_sidecar(&self, scope: &str) -> Option<Value> {
        fs::read_to_string(self.sidecar(scope))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
    }

    fn wake_log(&self, scope: &str) -> Option<String> {
        let leads = fno_agents::paths::space_dir(&self.root()).join("leads");
        fs::read_to_string(leads.join(format!("{scope}.md.wake.log"))).ok()
    }

    /// The manifest's `wake_times` stamps after a run.
    fn wake_times(&self, scope: &str) -> Option<Vec<String>> {
        let leads = fno_agents::paths::space_dir(&self.root()).join("leads");
        let text = fs::read_to_string(leads.join(format!("{scope}.md"))).ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("wake_times:") {
                let stamps: Vec<String> = rest
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                return Some(stamps);
            }
        }
        None
    }
}

/// The registry refuses two rows with one session identity, so only
/// `lead-x` carries the manifest's SESSION; every other lead gets its own.
fn row(name: &str, cwd: &Path, short_id: &str, scope: &str) -> Value {
    let session = if name == "lead-x" {
        SESSION.to_string()
    } else {
        let tag: String = name.bytes().map(|b| format!("{b:02x}")).collect();
        format!("00000000-0000-4000-8000-{tag:0>12.12}")
    };
    json!({
        "name": name,
        "status": "live",
        "cwd": cwd.display().to_string(),
        "short_id": short_id,
        "role_scope": scope,
        "role_level": 1,
        "created_at": "2026-08-28T12:00:00Z",
        "harness_session_id": session,
    })
}

type TruthStub =
    Arc<dyn Fn(&[String], Option<Instant>) -> (HashMap<String, TruthAnswer>, bool) + Send + Sync>;

fn truth_of(state: &str, reason: &str) -> TruthStub {
    let state = state.to_string();
    let reason = reason.to_string();
    Arc::new(move |handles: &[String], _: Option<Instant>| {
        let answers = handles
            .iter()
            .map(|h| {
                (
                    h.clone(),
                    TruthAnswer {
                        state: state.clone(),
                        reason: reason.clone(),
                    },
                )
            })
            .collect();
        (answers, true)
    })
}

fn truth_done() -> TruthStub {
    truth_of("done", "")
}

/// The fixture scope read: the rows whose `project` is `proj`, with the
/// not-closed count over them (the Python tests' `_scope_of`).
fn scope_of(board: &[Value]) -> Arc<dyn Fn(&str, &[Value]) -> ScopeRead + Send + Sync> {
    let proj: Vec<Value> = board
        .iter()
        .filter(|r| r.get("project").and_then(Value::as_str) == Some("proj"))
        .cloned()
        .collect();
    let ids: Vec<String> = proj
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str))
        .map(String::from)
        .collect();
    let undelivered = proj
        .iter()
        .filter(|r| !fno_agents::graph_store::is_terminal_entry(r))
        .count() as i64;
    Arc::new(move |_scope, _rows| ScopeRead {
        ids: Some(ids.clone()),
        undelivered,
    })
}

/// The collected observations one pass leaves behind, in the golden run's
/// shape.
#[derive(Default, Clone)]
struct Rec {
    events: Arc<Mutex<Vec<Value>>>,
    dispatches: Arc<Mutex<Vec<Value>>>,
    successor: Arc<Mutex<Vec<bool>>>,
    asks: Arc<Mutex<Vec<Value>>>,
    unread_calls: Arc<Mutex<Vec<String>>>,
}

impl Rec {
    fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }

    fn dispatches(&self) -> Vec<Value> {
        self.dispatches.lock().unwrap().clone()
    }

    fn successor(&self) -> Vec<bool> {
        self.successor.lock().unwrap().clone()
    }

    fn asks(&self) -> Vec<Value> {
        self.asks.lock().unwrap().clone()
    }

    fn unread(&self) -> Vec<String> {
        self.unread_calls.lock().unwrap().clone()
    }
}

/// The seams one case runs with: recorder seams plus the case's answers.
fn seams(
    _world: &World,
    rows: Vec<fno_agents::state::RegistryEntry>,
    rec: &Rec,
    truth: TruthStub,
    unread_hits: Vec<String>,
    next_answer: Arc<dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync>,
    entries: Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync>,
    scope_read: Arc<dyn Fn(&str, &[Value]) -> ScopeRead + Send + Sync>,
) -> Seams {
    let unread_calls = Arc::clone(&rec.unread_calls);
    let hits: std::collections::HashSet<String> = unread_hits.into_iter().collect();
    let dispatches = Arc::clone(&rec.dispatches);
    let successor = Arc::clone(&rec.successor);
    let asks = Arc::clone(&rec.asks);
    let events = Arc::clone(&rec.events);
    Seams {
        now: NOW,
        now_iso: NOW_ISO.to_string(),
        registry_rows: rows,
        truth,
        unread: Arc::new(move |address: &str| {
            unread_calls.lock().unwrap().push(address.to_string());
            hits.contains(address)
        }),
        answered: Arc::new(|| Vec::new()),
        next_answer,
        entries,
        scope_read,
        dispatch: Some(Box::new(
            move |target, reason, address, detail, successor_flag| {
                dispatches
                    .lock()
                    .unwrap()
                    .push(json!([target.scope, reason, address, detail,]));
                successor.lock().unwrap().push(successor_flag);
                true
            },
        )),
        ask: Box::new(move |target, count, ceiling, _marker| {
            asks.lock()
                .unwrap()
                .push(json!([target.scope, count, ceiling]));
        }),
        emit: Box::new(move |kind, payload| {
            events.lock().unwrap().push(json!([kind, payload]));
        }),
        seconds_left: None,
        truth_wait_s: 10.0,
    }
}

/// Fold the temp root and the space slug out of every string in the run.
fn fold(value: &mut Value, root: &Path) {
    match value {
        Value::String(text) => {
            let slug = root.to_string_lossy().replace('/', "-");
            *text = text
                .replace(&slug, "<ROOTSLUG>")
                .replace(&root.to_string_lossy().into_owned(), "<ROOT>");
        }
        Value::Array(items) => items.iter_mut().for_each(|v| fold(v, root)),
        Value::Object(map) => map.values_mut().for_each(|v| fold(v, root)),
        _ => {}
    }
}

/// The run JSON one pass leaves, in the golden's shape.
fn run_json(
    outcome: &TriggerOutcome,
    rec: &Rec,
    world: &World,
    scope: &str,
    wake_log: bool,
) -> Value {
    let summary = if outcome.armed {
        let mut map = serde_json::Map::new();
        map.insert("armed".into(), json!(true));
        map.insert("roles".into(), json!(outcome.roles));
        map.insert("evaluated".into(), json!(outcome.evaluated));
        map.insert("truth_reads".into(), json!(outcome.truth_reads));
        map.insert("woke".into(), json!(outcome.woke));
        map.insert(
            "refused".into(),
            json!(outcome
                .refused
                .iter()
                .map(|(scope, word)| json!({"scope": scope, "refusal": word}))
                .collect::<Vec<_>>()),
        );
        map.insert("note".into(), json!(outcome.note));
        map.insert("team_incomplete".into(), json!(outcome.team_incomplete));
        if outcome.budget_spent {
            map.insert("budget_spent".into(), json!(true));
        }
        Value::Object(map)
    } else {
        json!({"armed": false})
    };
    json!({
        "summary": summary,
        "events": rec.events(),
        "dispatches": rec.dispatches(),
        "successor_flags": rec.successor(),
        "asks": rec.asks(),
        "unread_calls": rec.unread(),
        "sidecar": world.read_sidecar(scope),
        "wake_times": world.wake_times(scope),
        "wake_log": if wake_log { world.wake_log(scope) } else { None },
    })
}

/// Compare one pass against the golden's run at `index`.
fn assert_run(label: &str, index: usize, mut mine: Value, root: &Path) {
    fold(&mut mine, root);
    let key = common::slug(label);
    let text = fs::read_to_string(golden_dir("lead_wake_pass").join(format!("{key}.json")))
        .unwrap_or_else(|e| panic!("[{label}] golden unreadable: {e}"));
    let golden: Value = serde_json::from_str(&text).unwrap();
    let want = &golden["runs"][index];
    assert_eq!(
        &mine, want,
        "[{label}] run {index} differs\ngolden={want}\nmine={mine}"
    );
}

fn base_board(id: &str) -> Value {
    json!({
        "id": id,
        "project": "proj",
        "status": "ready",
        "_kanban_column": "ready",
        "priority": "p1",
    })
}

fn board_a() -> Vec<Value> {
    vec![base_board("x-1")]
}

fn board_c() -> Vec<Value> {
    vec![base_board("x-1"), base_board("x-2"), base_board("x-3")]
}

fn no_answers() -> Arc<dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync>
{
    Arc::new(|_addresses, _records, cursor| (None, cursor.to_string()))
}

fn answer_due() -> Arc<dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync>
{
    Arc::new(|_addresses, _records, _cursor| (Some(PROMPT.to_string()), ANSWER_CURSOR.to_string()))
}

fn static_entries(rows: Vec<Value>) -> Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync> {
    Arc::new(move || Ok(rows.clone()))
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[test]
fn answered_escalation_wakes_with_the_answer_as_the_prompt() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    world.write_sidecar("epic-x", json!({"answered_cursor": CURSOR_SEED}));
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec![],
            answer_due(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "answered escalation wakes with the answer as the prompt",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn the_answer_trigger_fires_despite_the_debounce() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "wake_times: 2026-08-29T11:58:00Z\n");
    world.write_sidecar("epic-x", json!({"answered_cursor": CURSOR_SEED}));
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec!["lead-x".into()],
            answer_due(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "the answer trigger fires despite the debounce",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

fn armed() -> TriggerConfig {
    TriggerConfig {
        enabled: true,
        ceiling: 32,
        debounce_s: 900,
        backstop_s: 1800,
    }
}

#[test]
fn a_ceiling_refused_answer_keeps_the_cursor_so_it_stays_a_trigger() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    let stamps: Vec<String> = (0..32)
        .map(|_| "2026-08-29T11:00:00Z".to_string())
        .collect();
    world.manifest("epic-x", &format!("wake_times: {}\n", stamps.join(",")));
    world.write_sidecar("epic-x", json!({"answered_cursor": CURSOR_SEED}));
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows.clone(),
            &rec,
            truth_done(),
            vec![],
            answer_due(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "a ceiling refused answer keeps the cursor so it stays a trigger",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
    // The window rolled; the answer waits and then wakes.
    let rec2 = Rec::default();
    let mut seams2 = seams(
        &world,
        rows,
        &rec2,
        truth_done(),
        vec![],
        answer_due(),
        static_entries(Vec::new()),
        scope_of(&[]),
    );
    seams2.now = NOW + 25 * 3600;
    let outcome2 = run_with(&armed(), seams2);
    assert_run(
        "a ceiling refused answer keeps the cursor so it stays a trigger",
        1,
        run_json(&outcome2, &rec2, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn a_dead_holder_wakes_a_successor_under_the_recorded_role() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_of("unknown", "not-found"),
            vec!["lead-x".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "a dead holder wakes a successor under the recorded role",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn a_dead_holder_at_the_respawn_ceiling_spawns_nothing_and_escalates() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "respawn_count: 4\nrespawn_ceiling: 4\n");
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_of("unknown", "not-found"),
            vec!["lead-x".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "a dead holder at the respawn ceiling spawns nothing and escalates",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn present_holder_states_never_wake() {
    let states = [
        ("working", "working", ""),
        ("watching", "watching", ""),
        ("your-move", "your-move", ""),
        ("stalled", "stalled", ""),
        ("unknown no-records", "unknown", "no-records"),
        ("unknown resolver-error", "unknown", "resolver-error"),
    ];
    for (label, state, reason) in states {
        let world = World::new();
        let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
        world.manifest("epic-x", "");
        let rec = Rec::default();
        let outcome = run_with(
            &armed(),
            seams(
                &world,
                rows,
                &rec,
                truth_of(state, reason),
                vec!["lead-x".into()],
                no_answers(),
                static_entries(Vec::new()),
                scope_of(&[]),
            ),
        );
        assert_run(
            &format!("present holder {label} never wakes"),
            0,
            run_json(&outcome, &rec, &world, "epic-x", false),
            world.root().as_path(),
        );
    }
}

#[test]
fn unarmed_phase_reads_no_bus_and_emits_nothing() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    let rec = Rec::default();
    let config = TriggerConfig {
        enabled: false,
        ceiling: 32,
        debounce_s: 900,
        backstop_s: 1800,
    };
    let outcome = run_with(
        &config,
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "unarmed phase reads no bus and emits nothing",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn a_fresh_wake_is_debounced_and_bills_nothing() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "wake_times: 2026-08-29T11:55:00Z\n");
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec!["lead-x".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "a fresh wake is debounced and bills nothing",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn the_33rd_wake_is_refused_naming_ceiling_and_asks_once() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    // 32 real bills, ages 30m..1270m at 40m spacing: all inside 24h, all
    // past the debounce.
    let stamps: Vec<String> = (0..32)
        .map(|i| {
            let secs = NOW - (30 + 40 * i) * 60;
            stamp(secs)
        })
        .collect();
    world.manifest("epic-x", &format!("wake_times: {}\n", stamps.join(",")));
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec!["lead-x".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "the 33rd wake is refused naming ceiling and asks once",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn a_conflicted_scope_is_skipped() {
    let world = World::new();
    let rows = world.set_registry(json!([
        row("lead-a", &world.root(), "", "epic-x"),
        row("lead-b", &world.root(), "", "epic-x"),
    ]));
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec!["lead-a".into(), "lead-b".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "a conflicted scope is skipped",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

/// RFC3339 seconds stamp for a fixture epoch.
fn stamp(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

#[test]
fn the_receipt_names_refusals_and_dropped_roles() {
    let world = World::new();
    let rows = world.set_registry(json!([
        row("lead-x", &world.root(), "aa11bb22", "epic-x"),
        row("lead-y", &world.root(), "cc33dd44", "epic-y"),
    ]));
    world.manifest("epic-x", "");
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_of("working", ""),
            vec!["lead-x".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "the receipt names refusals and dropped roles",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn an_unreadable_registry_wakes_nothing() {
    let world = World::new();
    fs::write(world.home().join("registry.json"), "not json").unwrap();
    let rec = Rec::default();
    let home = fno_agents::paths::AgentsHome::at(world.home());
    let outcome = fno_agents::lead_wake_triggers::run(&home, &world.root(), &armed());
    assert_run(
        "an unreadable registry wakes nothing",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn the_board_diff_names_the_two_added_rows_and_nothing_unchanged() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    // Run 1: the seed pass. The seeded board's backstop bills a wake; the
    // manifest is then rewritten fresh, dropping that bill, the shape the
    // Python test's second `_run` produced.
    let rec = Rec::default();
    let _ = run_with(
        &armed(),
        seams(
            &world,
            rows.clone(),
            &rec,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(board_a()),
            scope_of(&board_a()),
        ),
    );
    world.rewrite_manifest("epic-x", "");
    let rec2 = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec2,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(board_c()),
            scope_of(&board_c()),
        ),
    );
    assert_run(
        "the board diff names the two added rows and nothing unchanged",
        0,
        run_json(&outcome, &rec2, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn an_empty_board_stores_an_observation_so_its_first_node_is_a_change() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    let elsewhere = vec![json!({
        "id": "o-1",
        "project": "elsewhere",
        "status": "ready",
        "_kanban_column": "ready",
        "priority": "p1",
    })];
    let rec = Rec::default();
    let outcome1 = run_with(
        &armed(),
        seams(
            &world,
            rows.clone(),
            &rec,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(elsewhere),
            scope_of(&[]),
        ),
    );
    let quiet_refill = vec![
        json!({
            "id": "x-1",
            "project": "proj",
            "status": "done",
            "completed_at": "2026-09-06T00:00:00Z",
            "_kanban_column": "done",
            "priority": "p2",
        }),
        json!({
            "id": "x-2",
            "project": "proj",
            "status": "done",
            "completed_at": "2026-09-06T00:00:00Z",
            "_kanban_column": "done",
            "priority": "p2",
        }),
    ];
    let rec2 = Rec::default();
    let outcome2 = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec2,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(quiet_refill.clone()),
            scope_of(&quiet_refill),
        ),
    );
    assert_run(
        "an empty board stores an observation so its first node is a change",
        0,
        run_json(&outcome1, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
    assert_run(
        "an empty board stores an observation so its first node is a change",
        1,
        run_json(&outcome2, &rec2, &world, "epic-x", false),
        world.root().as_path(),
    );
}

fn board_rows_of(board: &[Value]) -> Vec<[String; 4]> {
    let mut rows: Vec<[String; 4]> = board
        .iter()
        .filter(|r| r.get("project").and_then(Value::as_str) == Some("proj"))
        .map(|r| {
            [
                r["id"].as_str().unwrap_or("").to_string(),
                r["status"].as_str().unwrap_or("").to_string(),
                r["_kanban_column"].as_str().unwrap_or("").to_string(),
                r["priority"].as_str().unwrap_or("").to_string(),
            ]
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_refused_board_change_keeps_the_old_hash_so_it_stays_a_trigger() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "wake_times: 2026-08-29T11:58:00Z\n");
    let rows_a = board_rows_of(&board_a());
    world.write_sidecar(
        "epic-x",
        json!({
            "board_hash": hash_rows(&rows_a),
            "board_rows": rows_a.iter().map(|r| json!(r)).collect::<Vec<_>>(),
        }),
    );
    let rec = Rec::default();
    let board_b = vec![base_board("x-1"), base_board("x-2")];
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(board_b.clone()),
            scope_of(&board_b),
        ),
    );
    assert_run(
        "a refused board change keeps the old hash so it stays a trigger",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn the_backstop_fires_when_no_event_did() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    let rows_a = board_rows_of(&board_a());
    world.write_sidecar(
        "epic-x",
        json!({
            "board_hash": hash_rows(&rows_a),
            "board_rows": rows_a.iter().map(|r| json!(r)).collect::<Vec<_>>(),
        }),
    );
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(board_a()),
            scope_of(&board_a()),
        ),
    );
    assert_run(
        "the backstop fires when no event did",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

#[test]
fn first_observation_seeds_the_cursor_and_the_board_hash() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    // A terminal row keeps undelivered at 0, so the timer backstop stays
    // quiet and the case isolates the seeding writes.
    let quiet = vec![json!({
        "id": "x-1",
        "project": "proj",
        "status": "done",
        "completed_at": "2026-09-06T00:00:00Z",
        "_kanban_column": "done",
        "priority": "p2",
    })];
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec![],
            no_answers(),
            static_entries(quiet.clone()),
            scope_of(&quiet),
        ),
    );
    assert_run(
        "first observation seeds the cursor and the board hash",
        0,
        run_json(&outcome, &rec, &world, "epic-x", false),
        world.root().as_path(),
    );
}

/// N promoted scopes with a seeded cursor sidecar each, over one root.
fn multi_role_world(world: &World, count: usize) -> Vec<fno_agents::state::RegistryEntry> {
    let mut agents = Vec::new();
    for i in 0..count {
        agents.push(row(
            &format!("lead-{i}"),
            &world.root(),
            &format!("aaaa{i:04}"),
            &format!("epic-{i}"),
        ));
        world.manifest(&format!("epic-{i}"), "");
        world.write_sidecar(
            &format!("epic-{i}"),
            json!({"answered_cursor": CURSOR_SEED}),
        );
    }
    world.set_registry(json!(agents))
}

#[test]
fn five_quiet_roles_skip_the_truth_read_one_mail_role_pays_it() {
    let world = World::new();
    let rows = multi_role_world(&world, 6);
    let rec = Rec::default();
    let outcome = run_with(
        &armed(),
        seams(
            &world,
            rows,
            &rec,
            truth_done(),
            vec!["lead-3".into()],
            no_answers(),
            static_entries(Vec::new()),
            scope_of(&[]),
        ),
    );
    assert_run(
        "five quiet roles skip the truth read one mail role pays it",
        0,
        run_json(&outcome, &rec, &world, "epic-3", false),
        world.root().as_path(),
    );
}

#[test]
fn the_pass_stops_under_its_floor_and_names_what_it_evaluated() {
    let world = World::new();
    let rows = multi_role_world(&world, 5);
    let rec = Rec::default();
    let budget = Arc::new(Mutex::new(vec![100.0, 100.0, 100.0, 100.0, 14.0]));
    let mut base = seams(
        &world,
        rows,
        &rec,
        truth_done(),
        vec![
            "lead-0".into(),
            "lead-1".into(),
            "lead-2".into(),
            "lead-3".into(),
            "lead-4".into(),
        ],
        no_answers(),
        static_entries(Vec::new()),
        scope_of(&[]),
    );
    base.seconds_left = Some(Box::new(move || budget.lock().unwrap().pop()));
    let outcome = run_with(&armed(), base);
    assert_run(
        "the pass stops under its floor and names what it evaluated",
        0,
        run_json(&outcome, &rec, &world, "epic-2", false),
        world.root().as_path(),
    );
}

#[test]
fn a_refused_spawn_says_so_in_the_feed() {
    let world = World::new();
    let rows = world.set_registry(json!([row("lead-x", &world.root(), "aa11bb22", "epic-x")]));
    world.manifest("epic-x", "");
    let rec = Rec::default();
    // dispatch seam None: the real walk runs, refuses on the missing model
    // pin, and both the refusal and the spent-bill receipt land.
    let mut base = seams(
        &world,
        rows,
        &rec,
        truth_done(),
        vec!["lead-x".into()],
        no_answers(),
        static_entries(Vec::new()),
        scope_of(&[]),
    );
    base.dispatch = None;
    let outcome = run_with(&armed(), base);
    assert_run(
        "a refused spawn says so in the feed",
        0,
        run_json(&outcome, &rec, &world, "epic-x", true),
        world.root().as_path(),
    );
}

#[test]
fn a_blocked_truth_read_yields_the_role_and_still_wakes_the_next() {
    let world = World::new();
    let rows = world.set_registry(json!([
        row("lead-a", &world.root(), "aa11bb22", "epic-a1"),
        row("lead-b", &world.root(), "cc22dd33", "epic-b1"),
    ]));
    world.manifest("epic-a1", "");
    world.manifest("epic-b1", "");
    let rec = Rec::default();
    let truth: TruthStub = Arc::new(|handles, deadline| {
        let mut answers = HashMap::new();
        let mut measured = true;
        for handle in handles {
            if handle == "lead-a" {
                // Outlive the 0.2s bound: the pass yields the role.
                let pause = Some(Instant::now() + std::time::Duration::from_millis(400));
                if let Some(end) = deadline {
                    if end < pause.unwrap() {
                        std::thread::sleep(end - Instant::now());
                        measured = false;
                    }
                }
                answers.insert(
                    handle.clone(),
                    TruthAnswer {
                        state: "done".to_string(),
                        reason: String::new(),
                    },
                );
            } else {
                answers.insert(
                    handle.clone(),
                    TruthAnswer {
                        state: "done".to_string(),
                        reason: String::new(),
                    },
                );
            }
        }
        (answers, measured)
    });
    let mut base = seams(
        &world,
        rows,
        &rec,
        truth,
        vec!["lead-a".into(), "lead-b".into()],
        no_answers(),
        static_entries(Vec::new()),
        scope_of(&[]),
    );
    base.truth_wait_s = 0.2;
    let outcome = run_with(&armed(), base);
    assert_run(
        "a blocked truth read yields the role and still wakes the next",
        0,
        run_json(&outcome, &rec, &world, "epic-b1", false),
        world.root().as_path(),
    );
}

#[test]
fn a_blocked_graph_read_degrades_and_mail_still_wakes() {
    let world = World::new();
    let rows = world.set_registry(json!([
        row("lead-a", &world.root(), "aa11bb22", "epic-a1"),
        row("lead-b", &world.root(), "cc22dd33", "epic-b1"),
    ]));
    world.manifest("epic-a1", "");
    world.manifest("epic-b1", "");
    let rec = Rec::default();
    let entries: Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync> = Arc::new(|| {
        std::thread::sleep(std::time::Duration::from_millis(400));
        Ok(Vec::new())
    });
    let mut base = seams(
        &world,
        rows,
        &rec,
        truth_done(),
        vec!["lead-b".into(), "cc22dd33".into(), "epic-b1".into()],
        no_answers(),
        entries,
        scope_of(&[]),
    );
    base.truth_wait_s = 0.2;
    let outcome = run_with(&armed(), base);
    assert_run(
        "a blocked graph read degrades and mail still wakes",
        0,
        run_json(&outcome, &rec, &world, "epic-b1", false),
        world.root().as_path(),
    );
}

#[test]
fn the_spawned_walk_argv_carries_address_reason_holder_detail_model_and_successor() {
    let label = "the spawned walk argv carries address reason holder detail model and successor";
    let key = common::slug(label);
    let text =
        fs::read_to_string(golden_dir("lead_wake_pass").join(format!("{key}.json"))).unwrap();
    let golden: Value = serde_json::from_str(&text).unwrap();
    let target = fno_agents::lead_wake_triggers::Target {
        holder: "lead-x".to_string(),
        scope: "epic-x".to_string(),
        root: PathBuf::from("/tmp"),
        manifest: PathBuf::from("/tmp/leads/epic-x.md"),
        short_id: "aa11bb22".to_string(),
    };
    let argv = build_walk_argv(
        &target,
        "board",
        Path::new("fno-agents"),
        Some("aa11bb22"),
        Some("added: x-2 (ready/p1)"),
        true,
        "glm-5.3-flash[1m]",
    );
    assert_eq!(json!(argv), golden["argv"], "[{label}] argv differs");
}
