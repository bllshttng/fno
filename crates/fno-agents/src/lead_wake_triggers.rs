//! The lead-wake arm's refill-respawn leg: which lead that left its board is
//! woken this beat. A lead that exits `NoWork` cannot see its board refill,
//! so something outside it must wake it. Four triggers, best first: an
//! answer to the lead's own escalation, undrained mail, a board change
//! against the sidecar's stored hash, the timer backstop. The manifest's
//! wake ledger is the rate bound, billed under one lock before dispatch; a
//! holder proven gone is replaced through the respawn budget, not woken.
//!
//! Ported from the pr-watch tick's Python wake phase (run_lead_wake, deleted
//! beside this module's landing), with one sanctioned source change: the
//! targets come from the live registry rows (the same filter
//! `territory::live_teams` applies, grouped per canonical scope) instead of
//! the tick's team gather, so a scope with no registry row is never
//! enumerated and the gather-only drop words (holderless, empty scope) are
//! unreachable by construction. Everything else, including the bounded-read
//! budget shape and the clock-keyed rotation, is carried over verbatim; the
//! parity test (`lead_wake_pass_parity.rs`) freezes the old leg's answers
//! as goldens.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::paths::AgentsHome;

/// Rolling-window wake ceiling default; 0 is the unbounded spelling.
pub(crate) const DEFAULT_WAKE_CEILING: i64 = 32;
/// Minimum seconds between two non-escalation wakes on one manifest.
pub(crate) const DEFAULT_DEBOUNCE_S: i64 = 900;
/// The backstop re-check interval when no event trigger fired.
pub(crate) const DEFAULT_BACKSTOP_S: i64 = 1800;
/// The wake-ledger stamps the bill keeps, matching the overlay's keep.
pub(crate) const WAKE_LEDGER_KEEP: usize = 32;

/// A pass stops under this much budget left rather than be cut mid-step.
const STEP_FLOOR_S: f64 = 15.0;
/// Wait bound on one truth read: a slower read yields its role.
const TRUTH_WAIT_S: f64 = 10.0;
/// Character bound on one rendered diff: it rides the walk's argv.
pub(crate) const MAX_DETAIL_CHARS: usize = 2000;

/// Ceiling-refusal question markers, deduped through the shared
/// already-asked fold so one stranded scope asks once.
const CEILING_MARKER: &str = "lead-wake-ceiling";
const SUCCESSOR_CEILING_MARKER: &str = "lead-wake-respawn-ceiling";

const GRAPH_TIMED_OUT_NOTE: &str = "graph read timed out; later roles poll the same read";
const TEAM_STEP: &str = "team";

/// One promoted scope the leg may wake: the holder, its canonical scope,
/// the repo root the row registered from, the scope's manifest, and the
/// reply-handle short id mail carries.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub holder: String,
    pub scope: String,
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub short_id: String,
}

/// The `lead.*` wake keys, read once per pass. A 0 is the unbounded
/// spelling; an unparseable value reads as the default.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerConfig {
    pub enabled: bool,
    pub ceiling: i64,
    pub debounce_s: i64,
    pub backstop_s: i64,
}

impl TriggerConfig {
    /// The layered config lookup, Python `_cfg_int`'s None-vs-zero shape:
    /// an absent key is the default, a present 0 stays 0 (unbounded).
    pub(crate) fn read(config_cwd: &Path) -> Self {
        let int_key = |key: &str, default: i64| -> i64 {
            match crate::agents_config::config_lookup(config_cwd, &["lead", key]) {
                Some(v) => v.as_integer().map(|i| i as i64).unwrap_or(default),
                None => default,
            }
        };
        let enabled = matches!(
            crate::agents_config::config_lookup(config_cwd, &["lead", "wake_enabled"]),
            Some(v) if v.as_bool() == Some(true)
        );
        Self {
            enabled,
            ceiling: int_key("wake_ceiling", DEFAULT_WAKE_CEILING),
            debounce_s: int_key("wake_debounce_seconds", DEFAULT_DEBOUNCE_S),
            backstop_s: int_key("wake_backstop_seconds", DEFAULT_BACKSTOP_S),
        }
    }
}

/// The liveness read for one holder: `state` plus the resolver `reason`
/// that only `unknown` carries. Only `done` and `unknown`/`not-found` are
/// absence; every instrument failure fails closed.
#[derive(Debug, Clone, PartialEq)]
pub struct TruthAnswer {
    pub state: String,
    pub reason: String,
}

impl TruthAnswer {
    /// The production mapping from the native probe. A handle the batch
    /// measured but did not answer fails closed to `no-evidence`.
    pub(crate) fn from_probe(probe: &crate::truth_probe::TruthProbe) -> Self {
        let reason = if probe.state == "unknown" {
            probe.last_activity_basis.clone().unwrap_or_default()
        } else {
            String::new()
        };
        Self {
            state: probe.state.clone(),
            reason,
        }
    }

    pub(crate) fn no_evidence() -> Self {
        Self {
            state: "unknown".to_string(),
            reason: "no-evidence".to_string(),
        }
    }
}

/// One scope's board read: `None` ids is an uncompilable scope - no signal,
/// never an empty board.
#[derive(Debug, Clone)]
pub struct ScopeRead {
    pub ids: Option<Vec<String>>,
    pub undelivered: i64,
}

/// The graph read's answer: rows, or a named outage. The receiver's wait
/// bound owns the timeout, so no variant carries it.
pub(crate) enum GraphOutcome {
    Rows(Vec<Value>),
    Failed(String),
}

/// The leg's pass outcome: the counts the tick row's detail carries.
#[derive(Default)]
pub struct TriggerOutcome {
    pub armed: bool,
    pub roles: usize,
    pub evaluated: usize,
    pub truth_reads: usize,
    /// One receipt per dispatch, the summary's `woke` entries.
    pub woke: Vec<Value>,
    pub refused: Vec<(String, String)>,
    pub note: String,
    pub team_incomplete: bool,
    pub budget_spent: bool,
    pub acted: usize,
}

impl TriggerOutcome {
    /// The disarmed summary the parity test maps to `{"armed": false}`.
    pub(crate) fn disarmed() -> Self {
        Self {
            armed: false,
            ..Default::default()
        }
    }

    /// The detail line the tick row carries.
    pub(crate) fn detail_line(&self) -> String {
        let mut line = format!(
            "evaluated={}/{} truth_reads={}",
            self.evaluated, self.roles, self.truth_reads
        );
        if !self.woke.is_empty() {
            let woke: Vec<String> = self
                .woke
                .iter()
                .map(|receipt| {
                    format!(
                        "{}:{}",
                        receipt["scope"].as_str().unwrap_or("?"),
                        receipt["reason"].as_str().unwrap_or("?")
                    )
                })
                .collect();
            line.push_str(&format!(" woke={}", woke.join(",")));
        }
        let refused = fold_refusals(&self.refused);
        if !refused.is_empty() {
            line.push_str(&format!(" refused={refused}"));
        }
        if !self.note.is_empty() {
            line.push_str(&format!(" note={}", self.note));
        }
        line
    }
}

fn fold_refusals(refused: &[(String, String)]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, word) in refused {
        *counts.entry(word.as_str()).or_insert(0) += 1;
    }
    counts
        .iter()
        .map(|(word, n)| format!("{word}:{n}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The seams the pass reads through. The production wiring lives in
/// [`build_seams`]; the parity test builds the same struct with the fixture
/// stubs the old leg's tests injected, so both legs take identical seams.
pub struct Seams {
    /// Fixed clock: epoch seconds and the `%Y-%m-%dT%H:%M:%SZ` form.
    pub now: i64,
    pub now_iso: String,
    /// The live registry rows: the pass targets. The production wiring
    /// loads the registry; the parity fixtures build the rows.
    pub registry_rows: Vec<crate::state::RegistryEntry>,
    /// One bounded batch per role: handles to (state, reason), plus whether
    /// the instrument measured inside its deadline (false = timed out).
    pub truth: Arc<
        dyn Fn(&[String], Option<Instant>) -> (HashMap<String, TruthAnswer>, bool) + Send + Sync,
    >,
    /// Whether one address has undrained mail.
    pub unread: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// The answered-questions records, read once per pass.
    pub answered: Arc<dyn Fn() -> Vec<Value> + Send + Sync>,
    /// The oldest unprocessed answer for `addresses` past `cursor`:
    /// `(prompt, cursor_to_store)`, prompt `None` when none is due.
    pub next_answer:
        Arc<dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync>,
    /// The graph rows, read once on their own thread at pass start.
    pub entries: Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync>,
    /// One scope's board read over the in-hand graph rows; None ids is an
    /// uncompilable scope - no signal, never an empty board.
    pub scope_read: Arc<dyn Fn(&str, &[Value]) -> ScopeRead + Send + Sync>,
    /// The dispatch seam: `None` uses the real detached walk spawn.
    pub dispatch: Option<Box<dyn FnMut(&Target, &str, Option<&str>, Option<&str>, bool) -> bool>>,
    /// The durable ceiling question, already deduped by the caller.
    pub ask: Box<dyn FnMut(&Target, i64, i64, &str)>,
    /// The journal emitter (kind, payload).
    pub emit: Box<dyn Fn(&str, Value)>,
    /// The tick slice's remaining seconds, when the caller budgets.
    pub seconds_left: Option<Box<dyn Fn() -> Option<f64>>>,
    /// The truth wait bound; the fixtures shrink it to make a read yield.
    pub truth_wait_s: f64,
}

/// The pass, seam-injectable. The daemon arm calls this through [`run`].
pub fn run_with(config: &TriggerConfig, seams: Seams) -> TriggerOutcome {
    if !config.enabled {
        return TriggerOutcome::disarmed();
    }
    // Destructure once, so the FnMut fields borrow disjointly in the loop.
    let Seams {
        now,
        now_iso: _,
        registry_rows,
        truth,
        unread,
        answered,
        next_answer,
        entries,
        scope_read,
        mut dispatch,
        mut ask,
        emit,
        mut seconds_left,
        truth_wait_s,
    } = seams;
    let emit = &*emit;
    let ask = &mut *ask;

    let mut summary = TriggerOutcome {
        armed: true,
        ..Default::default()
    };

    // Budget slot 1: the team step. The read itself is a local registry
    // load now, but the slot keeps the pass's budget arithmetic identical.
    if budget_exhausted(seconds_left.as_deref_mut()) {
        return budget_stop(summary, TEAM_STEP);
    }

    // The graph read starts with the pass; later roles poll the same read.
    let (graph_rx, mut graph_cut) = start_graph_read(&entries);

    let selection = select_targets(&registry_rows);
    let targets = selection.ok;
    summary.roles = targets.len();
    summary.note = selection.drops.join("; ");

    // One answered-questions read per pass, shared by every scope. An
    // unreadable journal is not a trigger.
    // One clock reading here, as every bounded step spends: the floor check
    // reads the same value the wait cap uses.
    let left = read_seconds_left(seconds_left.as_deref_mut());
    if left.is_some_and(|l| l < STEP_FLOOR_S) {
        return budget_stop(summary, "answers");
    }
    let (answered, answers_timed_out) = bounded(move || answered(), wait_cap(left, truth_wait_s));
    let _ = answers_timed_out;
    let answered_records = answered.unwrap_or_default();

    let mut graph: Option<Vec<Value>> = None;
    // Clock-keyed rotation, no progress kept: the offset advances one role
    // per window.
    let offset = if targets.is_empty() {
        0
    } else {
        (now / config.debounce_s.max(1)) as usize % targets.len()
    };
    for target in targets[offset..].iter().chain(targets[..offset].iter()) {
        let sidecar = read_sidecar(&sidecar_of(target));
        let mut reason: Option<String> = None;
        let mut wake_address: Option<String> = None;
        let mut wake_detail: Option<String> = None;
        let mut answered_cursor_to_store = String::new();
        let pending_answer_seed = !sidecar.contains_key("answered_cursor");

        if !pending_answer_seed {
            let (prompt, matched) = escalation_trigger(
                &next_answer,
                target,
                &answered_records,
                sidecar
                    .get("answered_cursor")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            );
            if let Some(mut answer_prompt) = prompt {
                reason = Some("escalation_answered".to_string());
                // Delivery mail addressed the full session id: name it so
                // the row can be drained.
                let full_id = manifest_field(&target.manifest, "harness_session_id");
                if !full_id.is_empty() {
                    answer_prompt.push_str(&format!(
                        " The answer was also delivered as mail addressed to {full_id}: \
                         run `fno agents mail unread --name {full_id}` and drain it, \
                         then advance the cursor with \
                         `fno agents mail ack <id> --name {full_id}`."
                    ));
                }
                if answer_prompt.chars().count() > MAX_DETAIL_CHARS {
                    // One argv element: a pasted log must not abort the pass.
                    let mut out: String = answer_prompt.chars().take(MAX_DETAIL_CHARS).collect();
                    out.push_str(" ...(truncated)");
                    answer_prompt = out;
                }
                wake_detail = Some(answer_prompt);
                answered_cursor_to_store = matched;
            }
        }
        if reason.is_none() {
            if let Some(address) = mail_trigger(target, unread.as_ref()) {
                reason = Some("mail".to_string());
                wake_address = Some(address);
            }
        }
        let mut fresh_board_hash: Option<String> = None;
        let mut fresh_board_rows: Option<Vec<[String; 4]>> = None;
        let mut first_observation = false;
        if reason.is_none() {
            // The board lane: poll the shared graph read on its wait cap.
            if graph.is_none() && !graph_cut {
                let left = read_seconds_left(seconds_left.as_deref_mut());
                if left.is_some_and(|l| l < STEP_FLOOR_S) {
                    return budget_stop(summary, "graph");
                }
                match graph_rx.recv_timeout(Duration::from_secs_f64(wait_cap(left, truth_wait_s))) {
                    Ok(GraphOutcome::Rows(rows)) => graph = Some(rows),
                    Ok(GraphOutcome::Failed(e)) => {
                        graph_cut = true;
                        append_note(&mut summary, &format!("graph read failed: {e}"));
                    }
                    Err(_) => {
                        graph_cut = true;
                        append_note(&mut summary, GRAPH_TIMED_OUT_NOTE);
                    }
                }
            }
            // One compile feeds both lanes; None rows (empty or uncompilable
            // scope) is no signal for either.
            let (rows, undelivered) = match graph.as_deref() {
                Some(rows) if !rows.is_empty() => {
                    let read = scope_read(&target.scope, rows);
                    (board_rows(read.ids.as_deref(), rows), read.undelivered)
                }
                _ => (None, 0),
            };
            let (changed, hash, rows_store, diff, first) =
                board_trigger(target, rows.as_deref(), &sidecar);
            fresh_board_hash = hash;
            fresh_board_rows = rows_store;
            first_observation = first;
            if changed {
                reason = Some("board".to_string());
                wake_detail = diff;
            } else if rows.is_some() && backstop_due(target, undelivered, now, config.backstop_s) {
                reason = Some("backstop".to_string());
            }
        }
        if reason.is_none() {
            // Seeds record what this pass observed, so a role that cannot
            // wake never pays the read that was meant to gate them.
            if pending_answer_seed {
                seed_answered_cursor(target);
            }
            if first_observation {
                if let Some(hash) = &fresh_board_hash {
                    let rows = fresh_board_rows.as_deref().unwrap_or(&[]);
                    store_board_hash(target, hash, rows);
                }
            }
            summary.evaluated += 1;
            continue;
        }
        let reason_word = reason.clone().unwrap_or_default();
        let left = read_seconds_left(seconds_left.as_deref_mut());
        if left.is_some_and(|l| l < STEP_FLOOR_S) {
            let step = format!("truth:{}", target.scope);
            return budget_stop(summary, &step);
        }
        // The truth read costs seconds per role; a quiet role can never
        // wake, so it runs only for a role that can.
        let deadline = Some(Instant::now() + Duration::from_secs_f64(wait_cap(left, truth_wait_s)));
        let holder = target.holder.clone();
        let (truth_map, measured) = truth(&[holder], deadline);
        if !measured {
            // Yields to the rest; triggers stay armed, so the next
            // rotation retries it first.
            summary
                .refused
                .push((target.scope.clone(), "truth-timeout".to_string()));
            continue;
        }
        summary.truth_reads += 1;
        let truth = truth_map
            .get(&target.holder)
            .cloned()
            .unwrap_or_else(TruthAnswer::no_evidence);
        if let Some(word) = holder_absent(&truth) {
            // Liveness refusals ride the summary, not the event stream.
            summary.refused.push((target.scope.clone(), word));
            summary.evaluated += 1;
            continue;
        }
        // A GONE holder is replaced, not woken: the dispatch bills the
        // respawn budget, not only the wake ledger.
        let holder_gone = truth.state == "unknown" && truth.reason == "not-found";
        // Seeds land only for a holder that is present.
        if pending_answer_seed {
            seed_answered_cursor(target);
        }
        if first_observation {
            if let Some(hash) = &fresh_board_hash {
                let rows = fresh_board_rows.as_deref().unwrap_or(&[]);
                store_board_hash(target, hash, rows);
            }
        }
        if holder_gone {
            let fields = crate::lead_ops::parse_manifest(&target.manifest);
            let (at_ceiling, ceiling) = crate::lead_ops::respawn_budget(&fields);
            if at_ceiling {
                emit(
                    "lead_wake_refused",
                    json!({
                        "scope": target.scope,
                        "refusal": "respawn-ceiling",
                        "reason": reason_word,
                        "holder": target.holder,
                    }),
                );
                let count = fields
                    .get("respawn_count")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0);
                ask(target, count, ceiling, SUCCESSOR_CEILING_MARKER);
                summary
                    .refused
                    .push((target.scope.clone(), "respawn-ceiling".to_string()));
                summary.evaluated += 1;
                continue;
            }
        }
        // Admit-and-bill in ONE lock: an answered escalation skips only the
        // debounce, never the ceiling.
        let admit = crate::lead_ops::admit_wake(
            &target.manifest,
            now,
            config.ceiling,
            if reason_word == "escalation_answered" {
                0
            } else {
                config.debounce_s
            },
            WAKE_LEDGER_KEEP,
        );
        let (refusal_word, window_count) = match admit {
            Ok(pair) => pair,
            Err(e) => {
                append_note(&mut summary, &format!("admit failed: {e}"));
                summary.evaluated += 1;
                continue;
            }
        };
        if !refusal_word.is_empty() {
            emit(
                "lead_wake_refused",
                json!({
                    "scope": target.scope,
                    "refusal": refusal_word,
                    "reason": reason_word,
                    "window_count": window_count,
                    "ceiling": config.ceiling,
                }),
            );
            if refusal_word == "ceiling" {
                ask(target, window_count as i64, config.ceiling, CEILING_MARKER);
            }
            summary
                .refused
                .push((target.scope.clone(), refusal_word.to_string()));
            summary.evaluated += 1;
            continue;
        }
        let spawned = match dispatch.as_deref_mut() {
            Some(dispatch) => {
                dispatch(
                    target,
                    &reason_word,
                    wake_address.as_deref(),
                    wake_detail.as_deref(),
                    holder_gone,
                );
                true
            }
            None => match dispatch_walk_detached(
                target,
                &reason_word,
                walk_binary(),
                wake_address.as_deref(),
                wake_detail.as_deref(),
                holder_gone,
            ) {
                Ok(()) => true,
                Err(refusal_word) => {
                    // A refused spawn still spent a wake bill; the feed must
                    // say why nothing launched, with the real refusal word.
                    emit(
                        "lead_wake_refused",
                        json!({
                            "scope": target.scope,
                            "refusal": refusal_word,
                            "reason": reason_word,
                            "window_count": window_count,
                            "ceiling": config.ceiling,
                        }),
                    );
                    summary
                        .refused
                        .push((target.scope.clone(), refusal_word.to_string()));
                    false
                }
            },
        };
        if holder_gone && spawned {
            // The new session does not exist yet; its trail is the walk's
            // journal.
            let old_session_id = manifest_field(&target.manifest, "harness_session_id");
            emit(
                "lead_spawned_successor",
                json!({
                    "scope": target.scope,
                    "holder": target.holder,
                    "old_session_id": old_session_id,
                    "trigger": reason_word,
                }),
            );
        }
        if let Some(hash) = &fresh_board_hash {
            if let Some(rows) = &fresh_board_rows {
                store_board_hash(target, hash, rows);
            }
        }
        if !answered_cursor_to_store.is_empty() {
            update_sidecar_fields(
                target,
                &[("answered_cursor", json!(answered_cursor_to_store.clone()))],
            );
        }
        emit(
            "lead_woken",
            json!({
                "scope": target.scope,
                "reason": reason_word,
                "address": wake_address,
                "successor": holder_gone,
                "window_count": window_count,
                "ceiling": config.ceiling,
            }),
        );
        summary.woke.push(json!({
            "scope": target.scope,
            "reason": reason_word,
            "address": wake_address,
            "successor": holder_gone,
        }));
        summary.acted += 1;
        summary.evaluated += 1;
    }
    summary
}

/// The daemon's production entry: load the registry, wire the native seams,
/// run the pass. A registry read fault is the named note, never "no leads".
pub fn run(home: &AgentsHome, config_cwd: &Path, config: &TriggerConfig) -> TriggerOutcome {
    let rows = crate::state::load_registry(&home.registry_json());
    match rows {
        Err(_) => TriggerOutcome {
            armed: true,
            note: "registry unreadable - no scope enumerated, nothing woken".to_string(),
            ..Default::default()
        },
        Ok(loaded) => {
            let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
            let emit = move |kind: &str, payload: Value| {
                if let Err(error) = emitter.emit(kind, &payload) {
                    eprintln!("lead-wake-triggers: emit failed: {error}");
                }
            };
            run_with(
                config,
                build_seams(home, config_cwd, loaded.entries, Box::new(emit)),
            )
        }
    }
}

/// The daemon's production seams: native truth batch, in-process bus read,
/// the `fno` projection door for the answer reads, the strict graph read,
/// the daemon emitter, and no tick budget (`None` seconds).
pub(crate) fn build_seams(
    home: &AgentsHome,
    config_cwd: &Path,
    rows: Vec<crate::state::RegistryEntry>,
    emit: Box<dyn Fn(&str, Value)>,
) -> Seams {
    let now = now_epoch();
    let questions_path = crate::provider_cap::questions_path(home);
    let truth: Arc<
        dyn Fn(&[String], Option<Instant>) -> (HashMap<String, TruthAnswer>, bool) + Send + Sync,
    > = Arc::new(|handles, deadline| {
        let (probes, outcome) =
            crate::truth_probe::family1_truth_probe_many_measured_within(handles, deadline);
        let answers = probes
            .iter()
            .map(|(handle, probe)| (handle.clone(), TruthAnswer::from_probe(probe)))
            .collect();
        (
            answers,
            outcome == crate::truth_probe::BatchOutcome::Measured,
        )
    });
    let unread: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|address: &str| {
        // An unreadable bus is no mail for this address, never an aborted
        // pass for every role. The segments reload per address, the same
        // per-address cost the retired Python door paid per shell.
        let live = crate::chats::bus_append_live_path();
        let Some(bus_dir) = live.parent().map(Path::to_path_buf) else {
            return false;
        };
        let (raw, _) = crate::announce::read_bus_segments_reported(&live);
        let msgs: Vec<Value> = raw
            .iter()
            .filter(|m| crate::chats::required_fields_present(&m.row))
            .map(|m| m.row.clone())
            .collect();
        crate::mail_control_drain::unread_count(&bus_dir, &msgs, address) > 0
    });
    let answered: Arc<dyn Fn() -> Vec<Value> + Send + Sync> = {
        let questions_path = questions_path.clone();
        Arc::new(move || {
            projection_rows(&questions_path, "--answered-questions", &json!({}))
                .ok()
                .and_then(|rows| serde_json::from_value::<Vec<Value>>(rows).ok())
                .unwrap_or_default()
        })
    };
    let next_answer: Arc<
        dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync,
    > = {
        let questions_path = questions_path.clone();
        Arc::new(move |addresses, records, cursor| {
            let query = json!({"addresses": addresses, "records": records, "cursor": cursor});
            match projection_rows(&questions_path, "--next-answer", &query) {
                Ok(rows) => {
                    let prompt = rows.get("prompt").and_then(Value::as_str).map(String::from);
                    let next_cursor = rows
                        .get("cursor")
                        .and_then(Value::as_str)
                        .unwrap_or(cursor)
                        .to_string();
                    (prompt, next_cursor)
                }
                Err(_) => (None, cursor.to_string()),
            }
        })
    };
    let cwd = config_cwd.to_path_buf();
    let scope_compile_cwd = cwd.clone();
    let entries: Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync> =
        Arc::new(move || crate::lead_ops::scope_rows(&cwd));
    let scope_read: Arc<dyn Fn(&str, &[Value]) -> ScopeRead + Send + Sync> =
        Arc::new(move |scope, rows| {
            match crate::lead_ops::scope_ids(scope, rows, &scope_compile_cwd) {
                Ok(ids) => {
                    let undelivered = crate::lead_ops::undelivered_of(&ids, rows);
                    ScopeRead {
                        ids: Some(ids.into_iter().collect()),
                        undelivered,
                    }
                }
                Err(_) => ScopeRead {
                    ids: None,
                    undelivered: 0,
                },
            }
        });
    Seams {
        now,
        now_iso: iso_stamp(now),
        registry_rows: rows,
        truth,
        unread,
        answered,
        next_answer,
        entries,
        scope_read,
        dispatch: None,
        ask: Box::new(|target, count, ceiling, marker| {
            ask_marker_question(target, count, ceiling, marker);
        }),
        emit,
        seconds_left: None,
        truth_wait_s: TRUTH_WAIT_S,
    }
}

/// One projection read over the `fno doctor event rows` door: query JSON on
/// stdin, `{"projection", "rows"}` on stdout. A failure is a named Err,
/// never an empty answer. The projection fold lives in the fno crate; the
/// daemon reaches it through this door, never a copy.
fn projection_rows(questions_path: &Path, mode: &str, query: &Value) -> Result<Value, String> {
    let bin = crate::scrape::fno_bin();
    let mut child = std::process::Command::new(&bin)
        .args(["doctor", "event", "rows", "--events"])
        .arg(questions_path)
        .arg(mode)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("projection door spawn failed: {e}"))?;
    use std::io::Write as _;
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(query.to_string().as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("projection door read failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "projection door refused: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let answer: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("projection door answered malformed JSON: {e}"))?;
    answer
        .get("rows")
        .cloned()
        .ok_or_else(|| "projection door answered without rows".to_string())
}

/// The durable ceiling question: one per scope per marker, deduped through
/// the shared already-asked fold over the machine's outstanding listing.
fn ask_marker_question(target: &Target, count: i64, ceiling: i64, marker: &str) {
    let (question, ask) = if marker == SUCCESSOR_CEILING_MARKER {
        (
            format!(
                "Holder {} of scope {} is gone, a trigger is live, and the respawn \
                 budget is spent ({count}/{ceiling}).",
                target.holder, target.scope
            ),
            format!(
                "role a fresh lead for {} or raise its respawn ceiling",
                target.scope
            ),
        )
    } else {
        (
            format!(
                "Scope {} is at its lead wake ceiling ({count}/{ceiling} in the \
                 rolling 24h) while a wake trigger is live.",
                target.scope
            ),
            format!(
                "raise lead.wake_ceiling or clear the trigger for {}",
                target.scope
            ),
        )
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let src = crate::outstanding::sources(&cwd);
    let existing = crate::outstanding::collect(&src, Ok(Vec::new()))
        .ok()
        .and_then(|o| {
            let needle = format!("[{marker}:{}]", target.scope);
            o.questions
                .iter()
                .find(|q| q.question.contains(&needle))
                .map(|q| q.id.clone())
        });
    if existing.is_some() {
        return;
    }
    let qid = format!("q-{}", crate::law_match::random_hex8());
    // The marker prefix is the dedupe key's other half: the next pass finds
    // the question by the same `[{marker}:{scope}]` needle it stores.
    let stored = format!("[{marker}:{}] {question}", target.scope);
    let envelope = json!({
        "ts": iso_stamp(now_epoch()),
        "type": "operator_question",
        "source": "daemon",
        "data": {
            "question_id": qid,
            "question": stored,
            "cwd": target.root.display().to_string(),
            "ask": ask.clone(),
        },
    });
    let req = crate::question_intake::IntakeRequest {
        question: stored,
        template: false,
        ask: Some(ask),
        options: Vec::new(),
        blocks: Vec::new(),
        node: None,
        subject: None,
        session_id: None,
        cwd: Some(target.root.display().to_string()),
        asker: None,
        laws: Vec::new(),
        storage_root: target.root.clone(),
        index_path: None,
        // This line only names the question-intake door's append target,
        // the space journal the Python leg supplied; intake does the write.
        journal_path: Some(crate::paths::space_dir(&target.root).join("events.jsonl")),
        // events-discipline:allow: the raw writes in this window are the
        // walk's wake log, never an event journal.
        display_name: None,
        render_cap: None,
        op: Some("append".to_string()),
        event: Some(envelope),
        images: Vec::new(),
        screens_dir: None,
    };
    let home = AgentsHome::from_env();
    let answer = crate::question_intake::run_intake(&req, &home);
    if answer.exit_code != 0 {
        eprintln!(
            "lead-wake-triggers: the ceiling question could not be raised: {:?}",
            answer.lines
        );
    }
}

/// The real walk spawn, detached in its own process group, the wake log as
/// its output. A manifest with no model pin writes the refusal to the log
/// and answers `Err("manifest-carries-no-model-pin")`; an unwritable log or
/// a failed spawn answers the matching refusal word - the bill was spent,
/// so the caller must say why nothing launched.
fn dispatch_walk_detached(
    target: &Target,
    reason: &str,
    binary: PathBuf,
    address: Option<&str>,
    detail: Option<&str>,
    successor: bool,
) -> Result<(), &'static str> {
    let fields = crate::lead_ops::parse_manifest(&target.manifest);
    let model = fields
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let log = target
        .manifest
        .with_file_name(format!("{}.md.wake.log", target.scope));
    if model.is_empty() {
        if let Ok(mut sink) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            use std::io::Write as _;
            let _ = writeln!(
                sink,
                "lead-wake refused {}: the role manifest carries no model pin; \
                 re-role pinned, never respawn on the account default.",
                target.scope
            );
        }
        return Err("manifest-carries-no-model-pin");
    }
    let argv = build_walk_argv(target, reason, &binary, address, detail, successor, &model);
    let Ok(sink) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    else {
        return Err("wake-log-unwritable");
    };
    let spawned = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&target.root)
        .stdout(sink)
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
    spawned.map(|_| ()).map_err(|_| "walk-spawn-failed")
}

/// The walk binary: this daemon's own executable, so a drift-retired
/// successor never launches from a stale path.
fn walk_binary() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno-agents"))
}

/// The wake-mode walk's argv: the address and the diff travel on the
/// command line (a fresh session can derive neither), and the model
/// repeats the role manifest's pin, so the respawned lead and every
/// successor it mints run the promoted model.
pub fn build_walk_argv(
    target: &Target,
    reason: &str,
    binary: &Path,
    address: Option<&str>,
    detail: Option<&str>,
    successor: bool,
    model: &str,
) -> Vec<String> {
    let mut argv = vec![
        binary.display().to_string(),
        "loop".to_string(),
        "run".to_string(),
        "--driver".to_string(),
        "lead".to_string(),
        "--scope".to_string(),
        target.scope.clone(),
        "--model".to_string(),
        model.to_string(),
        "--wake".to_string(),
        "--wake-reason".to_string(),
        reason.to_string(),
        "--wake-holder".to_string(),
        target.holder.clone(),
    ];
    if let Some(address) = address {
        argv.extend(["--wake-address".to_string(), address.to_string()]);
    }
    if let Some(detail) = detail {
        argv.extend(["--wake-detail".to_string(), detail.to_string()]);
    }
    if successor {
        argv.push("--wake-successor".to_string());
    }
    argv
}

// ---------------------------------------------------------------------------
// Pure pieces
// ---------------------------------------------------------------------------

/// The pass targets from the live registry rows, grouped per canonical
/// scope. Drops are named: two rows on one scope are conflicting holders; a
/// row with no cwd is an unregistered holder; a scope whose manifest is
/// missing is named with its path. First-appearance order throughout.
pub(crate) fn select_targets(rows: &[crate::state::RegistryEntry]) -> TargetSelection {
    let mut by_scope: Vec<(String, Vec<&crate::state::RegistryEntry>)> = Vec::new();
    for row in rows {
        let raw = row.role_scope.as_deref().unwrap_or("").trim();
        if raw.is_empty() || !crate::spawn_gate::status_is_liveish(&row.status) {
            continue;
        }
        let canon = crate::territory::canonical_scope(raw);
        if canon.is_empty() {
            continue;
        }
        match by_scope.iter_mut().find(|(s, _)| *s == canon) {
            Some(group) => group.1.push(row),
            None => by_scope.push((canon, vec![row])),
        }
    }
    let mut ok: Vec<Target> = Vec::new();
    let mut drops: Vec<String> = Vec::new();
    for (scope, group) in by_scope {
        if group.len() > 1 {
            let holders = group
                .iter()
                .map(|row| row.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            drops.push(format!("{scope}: conflicting holders {holders}"));
            continue;
        }
        let row = group[0];
        if row.name.is_empty() {
            drops.push(format!("{scope}: holderless role"));
            continue;
        }
        if row.cwd.is_empty() {
            drops.push(format!("{scope}: unregistered holder"));
            continue;
        }
        let root = PathBuf::from(&row.cwd);
        let manifest =
            match crate::lead_state::manifest_path(&crate::paths::space_dir(&root), &scope) {
                Ok(path) => path,
                Err(e) => {
                    drops.push(format!("{scope}: {e}"));
                    continue;
                }
            };
        if !manifest.is_file() {
            drops.push(format!(
                "{scope}: manifest missing at {}",
                manifest.display()
            ));
            continue;
        }
        ok.push(Target {
            holder: row.name.clone(),
            scope,
            root,
            manifest,
            short_id: row.short_id.clone(),
        });
    }
    TargetSelection { ok, drops }
}

pub(crate) struct TargetSelection {
    pub ok: Vec<Target>,
    pub drops: Vec<String>,
}

/// The refusal word for a holder that is NOT absent: only `done` and
/// `unknown`/`not-found` are absence; instrument failures fail closed.
pub(crate) fn holder_absent(truth: &TruthAnswer) -> Option<String> {
    if truth.state == "done" {
        return None;
    }
    if truth.state == "unknown" {
        if truth.reason == "not-found" {
            return None;
        }
        return Some(format!("unknown/{}", truth.reason));
    }
    Some(truth.state.clone())
}

/// The scope's board rows sorted, `None` when the scope compiled to no ids.
pub(crate) fn board_rows(ids: Option<&[String]>, entries: &[Value]) -> Option<Vec<[String; 4]>> {
    let ids: BTreeSet<&str> = ids?.iter().map(String::as_str).collect();
    let mut rows: Vec<[String; 4]> = entries
        .iter()
        .filter_map(|row| {
            let id = row.get("id").and_then(Value::as_str)?;
            if !ids.contains(id) {
                return None;
            }
            Some([
                id.to_string(),
                str_field(row, "status"),
                str_field(row, "_kanban_column"),
                str_field(row, "priority"),
            ])
        })
        .collect();
    rows.sort();
    Some(rows)
}

fn str_field(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The rows' digest: pipe-joined rows, newline-joined, sha256 hex.
pub fn hash_rows(rows: &[[String; 4]]) -> String {
    use sha2::Digest as _;
    let joined: Vec<String> = rows.iter().map(|row| row.join("|")).collect();
    let digest = sha2::Sha256::digest(joined.join("\n").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The added/changed/removed rows between two observations, as prompt text;
/// a removed row is named, not hidden. Capped at `cap` characters, naming
/// the cut.
pub(crate) fn render_board_diff(old: &[[String; 4]], new: &[[String; 4]], cap: usize) -> String {
    let old: BTreeMap<&str, &[String; 4]> = old.iter().map(|r| (r[0].as_str(), r)).collect();
    let new: BTreeMap<&str, &[String; 4]> = new.iter().map(|r| (r[0].as_str(), r)).collect();
    let mut lines: Vec<String> = Vec::new();
    for id in old.keys() {
        if !new.contains_key(id) {
            lines.push(format!("removed: {id} ({})", row_label(old[id])));
        }
    }
    for id in new.keys() {
        if !old.contains_key(id) {
            lines.push(format!("added: {id} ({})", row_label(new[id])));
        }
    }
    for id in new.keys() {
        if let Some(previous) = old.get(id) {
            if *previous != new[id] {
                lines.push(format!(
                    "changed: {id} {} -> {}",
                    row_label(previous),
                    row_label(new[id])
                ));
            }
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    let text = lines.join("\n");
    if text.chars().count() <= cap {
        return text;
    }
    let mut shown: Vec<String> = Vec::new();
    let mut used = 0usize;
    for line in &lines {
        if used + line.chars().count() + 1 > cap - 32 {
            break;
        }
        shown.push(line.clone());
        used += line.chars().count() + 1;
    }
    format!(
        "{}\n...(+{} more rows elided)",
        shown.join("\n"),
        lines.len() - shown.len()
    )
}

fn row_label(row: &[String; 4]) -> String {
    row[1..]
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("/")
}

/// `(wake?, hash+rows to store after a dispatch, diff, first_observation)`.
/// Pure: it never writes. An absent sidecar is a first observation - the
/// caller stores it only after the holder reads present; a changed hash
/// stores only after a dispatch; no rows is no signal.
pub(crate) fn board_trigger(
    target: &Target,
    rows: Option<&[[String; 4]]>,
    sidecar: &Map<String, Value>,
) -> (
    bool,
    Option<String>,
    Option<Vec<[String; 4]>>,
    Option<String>,
    bool,
) {
    let Some(rows) = rows else {
        return (false, None, None, None, false);
    };
    let fresh = hash_rows(rows);
    let stored = sidecar
        .get("board_hash")
        .and_then(Value::as_str)
        .unwrap_or("");
    let stored_rows: Option<Vec<[String; 4]>> = sidecar.get("board_rows").and_then(|v| {
        let list = v.as_array()?;
        let parsed: Option<Vec<[String; 4]>> = list
            .iter()
            .map(|row| {
                let parts = row.as_array()?;
                if parts.len() != 4 {
                    return None;
                }
                let mut out = [String::new(), String::new(), String::new(), String::new()];
                for (slot, part) in out.iter_mut().zip(parts) {
                    *slot = part.as_str()?.to_string();
                }
                Some(out)
            })
            .collect();
        parsed
    });
    let _ = target;
    if stored.is_empty() || stored_rows.is_none() {
        return (false, Some(fresh), Some(rows.to_vec()), None, true);
    }
    if stored == fresh {
        return (false, None, None, None, false);
    }
    let diff = render_board_diff(
        stored_rows.as_deref().unwrap_or(&[]),
        rows,
        MAX_DETAIL_CHARS,
    );
    (true, Some(fresh), Some(rows.to_vec()), Some(diff), false)
}

/// The timer backstop: an approximation of the polled triggers so a missed
/// event cannot strand a scope; a billed wake inside the window suppresses
/// it. `undelivered` is the goal's own count, done or superseded.
pub(crate) fn backstop_due(target: &Target, undelivered: i64, now: i64, backstop_s: i64) -> bool {
    let journal = crate::paths::space_dir(Path::new(&target.root)).join("events.jsonl");
    if crate::lead_ops::last_run_is_fresh(&journal, backstop_s, now) {
        return false;
    }
    if undelivered <= 0 {
        return false;
    }
    let stamps = crate::lead_ops::read_wakes(&target.manifest, now);
    if let Some(last) = stamps.last() {
        if now - last < backstop_s {
            return false;
        }
    }
    true
}

/// The matched address with undrained mail: the holder, its short id, and
/// every scope member, in sorted order.
pub(crate) fn mail_trigger(target: &Target, unread: &dyn Fn(&str) -> bool) -> Option<String> {
    let mut addresses: BTreeSet<String> = BTreeSet::new();
    if !target.holder.is_empty() {
        addresses.insert(target.holder.clone());
    }
    if !target.short_id.is_empty() {
        addresses.insert(target.short_id.clone());
    }
    for member in target.scope.split(',') {
        let member = member.trim();
        if !member.is_empty() {
            addresses.insert(member.to_string());
        }
    }
    addresses.into_iter().find(|address| unread(address))
}

/// `(prompt, matched_close_ts)` for the oldest unprocessed answer to the
/// holder's own question. Delivery mail addresses the full session id, so
/// the address set adds the manifest's id and its canonical short form.
fn escalation_trigger(
    next_answer: &Arc<dyn Fn(&[String], &[Value], &str) -> (Option<String>, String) + Send + Sync>,
    target: &Target,
    records: &[Value],
    cursor: &str,
) -> (Option<String>, String) {
    let full_id = manifest_field(&target.manifest, "harness_session_id");
    let mut set: BTreeSet<String> = BTreeSet::new();
    if !target.holder.is_empty() {
        set.insert(target.holder.clone());
    }
    if !target.short_id.is_empty() {
        set.insert(target.short_id.clone());
    }
    if !full_id.is_empty() {
        set.insert(full_id);
        set.insert(canonical_handle(&manifest_field(
            &target.manifest,
            "harness_session_id",
        )));
    }
    let addresses: Vec<String> = set.into_iter().collect();
    next_answer(&addresses, records, cursor)
}

/// The mailbox short form: the identity key's first eight characters.
pub(crate) fn canonical_handle(session_id: &str) -> String {
    let key: String = if session_id.starts_with("ses_") {
        session_id.to_string()
    } else {
        session_id.to_lowercase()
    };
    key.chars().take(8).collect()
}

fn manifest_field(manifest: &Path, key: &str) -> String {
    crate::lead_ops::parse_manifest(manifest)
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The manifest's created_at in the journal's ts shape: seeding the answer
/// cursor at the term's birth keeps a just-closed answer live.
pub(crate) fn birth_cursor(manifest: &Path) -> String {
    let raw = manifest_field(manifest, "created_at");
    match chrono::NaiveDateTime::parse_from_str(&raw, "%Y-%m-%dT%H:%M:%SZ") {
        // `+00:00`, the journal's isoformat spelling the cursor stores.
        Ok(stamp) => stamp
            .and_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        Err(_) => String::new(),
    }
}

fn read_sidecar(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn sidecar_of(target: &Target) -> PathBuf {
    target
        .manifest
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{}.wake.json", target.scope))
}

fn seed_answered_cursor(target: &Target) {
    let cursor = birth_cursor(&target.manifest);
    if cursor.is_empty() {
        return;
    }
    update_sidecar_fields(target, &[("answered_cursor", json!(cursor))]);
}

fn store_board_hash(target: &Target, digest: &str, rows: &[[String; 4]]) {
    let stored: Vec<Value> = rows
        .iter()
        .map(|row| json!([row[0], row[1], row[2], row[3]]))
        .collect();
    update_sidecar_fields(
        target,
        &[
            ("board_hash", json!(digest)),
            ("board_rows", Value::Array(stored)),
        ],
    );
}

fn update_sidecar_fields(target: &Target, fields: &[(&str, Value)]) {
    let mut map = Map::new();
    for (key, value) in fields {
        map.insert((*key).to_string(), value.clone());
    }
    if let Err(e) = crate::lead_ops::update_sidecar(&sidecar_of(target), &map) {
        eprintln!("lead-wake-triggers: sidecar update failed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Bounded reads and the budget
// ---------------------------------------------------------------------------

fn read_seconds_left(mut seconds_left: Option<&mut (dyn Fn() -> Option<f64> + '_)>) -> Option<f64> {
    seconds_left.as_deref_mut().and_then(|f| f())
}

fn budget_exhausted(seconds_left: Option<&mut (dyn Fn() -> Option<f64> + '_)>) -> bool {
    read_seconds_left(seconds_left).is_some_and(|left| left < STEP_FLOOR_S)
}

fn budget_stop(mut summary: TriggerOutcome, step: &str) -> TriggerOutcome {
    let note = format!(
        "budget spent after {} of {} roles, before {step}",
        summary.evaluated, summary.roles
    );
    append_note(&mut summary, &note);
    summary.budget_spent = true;
    summary
}

fn append_note(summary: &mut TriggerOutcome, note: &str) {
    if summary.note.is_empty() {
        summary.note = note.to_string();
    } else {
        summary.note = format!("{}; {note}", summary.note);
    }
}

/// The wait cap for one bounded read: the truth bound, shrunk to leave the
/// step floor when the pass is budgeted.
fn wait_cap(left: Option<f64>, truth_wait_s: f64) -> f64 {
    match left {
        None => truth_wait_s,
        Some(left) => truth_wait_s.min((left - STEP_FLOOR_S).max(0.0)),
    }
}

/// One bounded read: `(value, timed_out)`; the reader runs on past a
/// timeout, so a join would spend the bound.
fn bounded<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
    wait_s: f64,
) -> (Option<T>, bool) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs_f64(wait_s.max(0.0))) {
        Ok(value) => (Some(value), false),
        Err(_) => (None, true),
    }
}

/// The graph read on its own thread, so a slow store delays no setup read.
fn start_graph_read(
    entries: &Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync>,
) -> (mpsc::Receiver<GraphOutcome>, bool) {
    let entries = Arc::clone(entries);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = match entries() {
            Ok(rows) => GraphOutcome::Rows(rows),
            Err(e) => GraphOutcome::Failed(e),
        };
        let _ = tx.send(outcome);
    });
    (rx, false)
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn iso_stamp(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
