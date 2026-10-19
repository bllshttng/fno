//! The provider-lane axes of the spawn gate : the per-provider live
//! count, the lead share, the registry schema guard and the account quota
//! lock, ported from the Python gate (`cli/src/fno/agents/spawn_gate.py`)
//! that this gate replaces.
//!
//! Fail-closed is the contract here, unlike the global guards: the module
//! doc in `spawn_gate.rs` lets a RAM floor or a slot count fail OPEN because
//! the gate must never brick spawning, but an UNREADABLE LANE COUNT is a
//! refusal, never a zero — assuming an empty lane oversubscribes a shared
//! account, which is the harm the provider cap exists to prevent.

use std::collections::HashSet;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::agents_config;
use crate::claims::{self, ClaimState, PidProbe};
use crate::claude_roster::ClaudeRoster;
use crate::spawn_gate::status_is_liveish;
use crate::state::{load_registry, RegistryEntry};
use crate::AgentStatus;

/// How long a reactive provider-health entry may drive the quota lock
/// (mirrors runtime_state.PROVIDER_HEALTH_TTL_SECONDS and the bound
/// fallback_chain already applies to its own health reads).
const PROVIDER_HEALTH_TTL_SECONDS: f64 = 60.0 * 60.0;

// ponytail: fixed 5-hour hold matches the vendor window in the measured 429;
// upgrade is parsing the window length from the excerpt.
const UNKNOWN_RESET_HOLD_S: i64 = 5 * 3600;

/// Pane-probe wall-clock budget: the shared mux subprocess bound
/// (mux_spawn._MUX_SUBPROCESS_TIMEOUT_S).
const PANE_PROBE_BUDGET: Duration = Duration::from_secs(30);

/// The coerced per-provider lane caps. The legacy `max_lanes` spelling is
/// read only when `provider_limits` is absent; malformed input restores the
/// shared-account default, while an explicit empty table disables all caps.
pub(crate) fn provider_lane_caps(config_cwd: &Path) -> BTreeMap<String, Option<usize>> {
    let raw = agents_config::config_lookup(config_cwd, &["agents", "provider_limits"])
        .or_else(|| agents_config::config_lookup(config_cwd, &["agents", "max_lanes"]));
    match raw {
        Some(raw) => coerce_provider_lane_caps(&raw).unwrap_or_else(built_in_provider_lane_caps),
        None => built_in_provider_lane_caps(),
    }
}

pub(crate) fn provider_lanes_cap(config_cwd: &Path, provider: &str) -> Option<usize> {
    provider_lane_caps(config_cwd)
        .get(provider)
        .copied()
        .flatten()
}

fn built_in_provider_lane_caps() -> BTreeMap<String, Option<usize>> {
    BTreeMap::from([("zai".to_string(), Some(5))])
}

fn coerce_provider_lane_caps(raw: &toml::Value) -> Option<BTreeMap<String, Option<usize>>> {
    let table = raw.as_table()?;
    let mut caps = BTreeMap::new();
    for (provider, budget) in table {
        if !valid_provider_name(provider) {
            return None;
        }
        let lanes = if let Some(value) = budget.as_integer() {
            Some(positive_lane_cap(value)?)
        } else if let Some(dimensions) = budget.as_table() {
            if dimensions
                .keys()
                .any(|key| !matches!(key.as_str(), "lanes" | "subagents"))
            {
                return None;
            }
            let mut lanes = None;
            for (dimension, value) in dimensions {
                let value = positive_lane_cap(value.as_integer()?)?;
                if dimension == "lanes" {
                    lanes = Some(value);
                }
            }
            lanes
        } else {
            return None;
        };
        caps.insert(
            provider.clone(),
            lanes.or_else(|| (provider == "zai").then_some(5)),
        );
    }
    Some(caps)
}

fn positive_lane_cap(value: i64) -> Option<usize> {
    usize::try_from(value).ok().filter(|cap| *cap > 0)
}

fn valid_provider_name(provider: &str) -> bool {
    let mut chars = provider.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

/// Read counts from the spawn-gate owner for every capped provider. Schema
/// refusals and unreadable counts stay explicit so route-slot fails closed.
pub(crate) fn vendor_lane_readings(config_cwd: &Path, registry_path: Option<&Path>) -> Value {
    let caps = provider_lane_caps(config_cwd);
    let mut warnings = Vec::new();
    let (questions_raw, watched, schema_error) = match registry_path {
        Some(path) => {
            let schema_error = check_registry_schema(path, &mut warnings).err().map(|_| {
                warnings
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "spawn-gate registry schema refused".to_string())
            });
            let questions = schema_error
                .is_none()
                .then(|| read_questions_journal(path, &mut warnings));
            let watched = schema_error
                .is_none()
                .then(|| watched_ids(path, &mut warnings))
                .unwrap_or_default();
            (questions, watched, schema_error)
        }
        None => (
            None,
            BTreeMap::new(),
            Some("fno registry home unavailable".to_string()),
        ),
    };
    vendor_lane_readings_from_caps(&caps, |provider| {
        let Some(path) = registry_path else {
            return Err("fno registry home unavailable".to_string());
        };
        if let Some(error) = &schema_error {
            return Err(error.clone());
        }
        provider_live_count_with_questions(
            path,
            provider,
            questions_raw.as_deref().unwrap_or_default(),
            &watched,
            None,
            &mut warnings,
        )
        .map(|reading| reading.count)
    })
}

fn vendor_lane_readings_from_caps(
    caps: &BTreeMap<String, Option<usize>>,
    mut read_count: impl FnMut(&str) -> Result<usize, String>,
) -> Value {
    let mut vendor_caps = serde_json::Map::new();
    let mut vendor_counts = serde_json::Map::new();
    let mut vendor_count_errors = serde_json::Map::new();
    for (provider, cap) in caps {
        let Some(cap) = cap else { continue };
        vendor_caps.insert(provider.clone(), serde_json::json!(cap));
        match read_count(provider) {
            Ok(count) => {
                vendor_counts.insert(provider.clone(), serde_json::json!(count));
            }
            Err(error) => {
                vendor_count_errors.insert(provider.clone(), serde_json::json!(error));
            }
        }
    }
    serde_json::json!({
        "vendor_caps": vendor_caps,
        "vendor_counts": vendor_counts,
        "vendor_count_errors": vendor_count_errors,
    })
}

/// The `agents.provider_limits.<provider>.subagents` ceiling, or `None` when
/// the provider is uncapped. Mirrors `provider_lanes_cap`: a config that
/// never named a provider_limits table falls back to the built-in budget
/// table, exactly as Python's `provider_subagent_budget` fails open
/// (`config._BUILTIN_PROVIDER_BUDGETS`). The lead check-in reads it as the
/// blueprint-subagent ceiling; `None` reads as the default ceiling there.
pub(crate) fn provider_subagents_cap(config_cwd: &Path, provider: &str) -> Option<usize> {
    let subagents = match agents_config::config_lookup(config_cwd, &["agents", "provider_limits"]) {
        Some(table) => table
            .get(provider)
            .and_then(|budget| budget.get("subagents"))
            .and_then(|v| v.as_integer()),
        None => built_in_subagents(provider),
    };
    usize::try_from(subagents.unwrap_or(0))
        .ok()
        .filter(|subagents| *subagents >= 1)
}

/// The built-in budget table (`config._BUILTIN_PROVIDER_BUDGETS`): zai only.
fn built_in_subagents(provider: &str) -> Option<i64> {
    (provider == "zai").then_some(1)
}

// ---------------------------------------------------------------------------
// Pane liveness: one seam crossing, through the pane owner's own verb
// ---------------------------------------------------------------------------

/// Exact pane liveness for a registry row's mux ref: `Ok(true/false)` decided,
/// `Err(())` when the mux could not answer (the caller refuses — unreadable is
/// not absent). Shells `fno mux pane wait` the way `mux_spawn._mux_pane_alive`
/// does: exit 12 = the pane exited, 0/11 = alive; anything else is ambiguous,
/// and the authoritative pane listing decides. An EMPTY listing is
/// undecidable, never "gone": only a listing naming at least one OTHER pane
/// proves the server answered with real content.
pub(crate) fn mux_pane_alive(session: &str, pane_id: u64) -> Result<bool, ()> {
    let code = match pane_probe_code(session, pane_id) {
        Some(code) => code,
        None => return Err(()),
    };
    match code {
        12 => return Ok(false),
        0 | 11 => return Ok(true),
        _ => {}
    }
    // Ambiguous exit: ask the listing. A listing that cannot be read or names
    // no pane at all is undecidable (None), never a death.
    let out = match run_with_budget(
        Command::new(crate::scrape::fno_bin())
            .args(["mux", "pane", "ls", "--server", session, "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    ) {
        Some(out) => out,
        None => return Err(()),
    };
    if !out.status.success() {
        return Err(());
    }
    let rows: Value = match serde_json::from_slice(&out.stdout) {
        Ok(rows) => rows,
        Err(_) => return Err(()),
    };
    let rows = match rows.as_array() {
        Some(rows) if !rows.is_empty() => rows,
        _ => return Err(()),
    };
    Ok(!rows
        .iter()
        .any(|r| r.get("pane_id").and_then(Value::as_u64) == Some(pane_id)))
}

/// Run `fno mux pane wait` under the probe budget. `None` = the probe never
/// answered (spawn failure or deadline miss), which is undecidable.
fn pane_probe_code(session: &str, pane_id: u64) -> Option<i32> {
    let pane_id = pane_id.to_string();
    let mut cmd = Command::new(crate::scrape::fno_bin());
    cmd.args([
        "mux",
        "pane",
        "wait",
        "--server",
        session,
        &pane_id,
        "--timeout",
        "0",
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    run_with_budget(&mut cmd).map(|out| out.status.code().unwrap_or(-1))
}

/// Spawn a child, bound its whole run to `PANE_PROBE_BUDGET`, and reap it.
fn run_with_budget(cmd: &mut Command) -> Option<std::process::Output> {
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + PANE_PROBE_BUDGET;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Three-state pid liveness (the provider count refuses on undecidable)
// ---------------------------------------------------------------------------

/// `Ok(true)` alive, `Ok(false)` provably gone, `Err(())` undecidable — the
/// port of `spawn_gate._pid_alive`'s None: a denied inspection presents as an
/// undecided case, never as a decided death. When `recorded` carries the
/// row's `pid_start_time`, a live pid of a DIFFERENT incarnation (the pid was
/// recycled) reads as gone: the current start token is compared in the
/// registry's own units, as `daemon::pid_is_ours` does. An unreadable current
/// token against a recorded one is undecidable, never a silent count.
fn pid_liveness(pid: u32, recorded: Option<u64>) -> Result<bool, ()> {
    if pid <= 1 {
        return Ok(false);
    }
    match claims::probe_pid(pid as i32) {
        PidProbe::Created(_) => match recorded {
            None => Ok(true),
            Some(rec) => match crate::daemon::process_start_time(pid) {
                Some(now) => Ok(now == rec),
                None => Err(()),
            },
        },
        PidProbe::Absent => Ok(false),
        PidProbe::Refused => Err(()),
    }
}

/// One lane's count could not be proved: the probe's unknown verdict, with
/// the lane and the fault named.
#[derive(Debug)]
pub(crate) struct LaneFault {
    pub(crate) provider: String,
    pub(crate) error: String,
}

/// The requested bg short ids with a positive live roster marker. Fails closed
/// on an unreadable roster and on an undecidable incarnation
/// (`_provider_roster_live_short_ids`).
fn provider_roster_live_short_ids(wanted: &BTreeSet<String>) -> Result<BTreeSet<String>, String> {
    let roster =
        ClaudeRoster::load_default().map_err(|e| format!("claude roster unreadable: {e}"))?;
    let mut live = BTreeSet::new();
    let mut undecidable = BTreeSet::new();
    for worker in roster.workers_deduped() {
        let short = worker.short_id().to_string();
        if !wanted.contains(&short) {
            continue;
        }
        match worker.pid {
            None => {}
            Some(pid) => match pid_liveness(pid, None) {
                Ok(true) => {
                    live.insert(short);
                }
                Ok(false) => {}
                Err(()) => {
                    undecidable.insert(short);
                }
            },
        }
    }
    let unknown: Vec<String> = undecidable.difference(&live).cloned().collect();
    if !unknown.is_empty() {
        return Err(format!(
            "process incarnation unreadable for bg worker(s) {unknown:?}"
        ));
    }
    Ok(live)
}

// ---------------------------------------------------------------------------
// The provider count
// ---------------------------------------------------------------------------

/// A transcript quieter than this, on a row with an open operator question,
/// parks the row out of its provider lane count (the reaper's default quiet
/// grace).
const OPERATOR_WAIT_QUIET_S: u64 = 900;

/// Rows waiting on the operator: an open question they asked, and a transcript
/// quiet at least OPERATOR_WAIT_QUIET_S. Row name -> oldest open question id.
/// Only claude rows with a session id qualify; the transcript lookup is
/// claude-only. A question is open when an `operator_question` row carries its
/// id and no `operator_question_closed` row does. An unparsable line never
/// qualifies a row (the count's rule: an unreadable source never produces a
/// smaller number).
fn awaiting_operator(
    rows: &[&RegistryEntry],
    questions_raw: &str,
    transcript_age_s: impl Fn(&str) -> Option<u64>,
) -> BTreeMap<String, String> {
    let mut asks: Vec<(String, Option<String>, Option<String>)> = Vec::new(); // qid, session_id, asker
    let mut closed: HashSet<String> = HashSet::new();
    for line in questions_raw.lines() {
        if !line.contains("operator_question") {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = val.get("type").and_then(Value::as_str).unwrap_or("");
        let data = val
            .get("data")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let Some(qid) = data.get("question_id").and_then(Value::as_str) else {
            continue;
        };
        match kind {
            "operator_question" => asks.push((
                qid.to_string(),
                data.get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                data.get("asker")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )),
            "operator_question_closed" => {
                closed.insert(qid.to_string());
            }
            _ => {}
        }
    }

    let mut waiting: BTreeMap<String, String> = BTreeMap::new();
    for row in rows {
        if row.harness_name() != "claude" {
            continue;
        }
        let Some(sid) = row.harness_session_id.as_deref() else {
            continue;
        };
        if sid.is_empty() {
            continue;
        }
        let short = sid.get(..8).unwrap_or(sid).to_lowercase();
        // File order is journal order, so the first OPEN ask is the oldest:
        // closed asks are filtered before the find, or a closed first question
        // would hide a newer open one.
        let Some((qid, _, _)) = asks
            .iter()
            .filter(|(qid, _, _)| !closed.contains(qid))
            .find(|(_, q_session, q_asker)| match q_session {
                Some(s) => s.eq_ignore_ascii_case(sid),
                None => q_asker
                    .as_deref()
                    .is_some_and(|a| a.to_lowercase() == short),
            })
        else {
            continue;
        };
        // The transcript lookup scans every project dir, so it runs only for
        // a row that has an open ask.
        if transcript_age_s(sid).is_some_and(|age| age >= OPERATOR_WAIT_QUIET_S) {
            waiting.insert(row.name.clone(), qid.clone());
        }
    }
    waiting
}

/// The raw questions journal beside the agents home (`<fno_dir>/questions.jsonl`,
/// the path `needs.rs` `default_sources` reads). Never fails: a missing journal
/// means no questions, and any other read error pushes one warning and means no
/// questions, so waiting workers stay counted.
pub(crate) fn read_questions_journal(registry_path: &Path, warnings: &mut Vec<String>) -> String {
    match registry_path.parent().and_then(Path::parent) {
        Some(fno_dir) => {
            let path = fno_dir.join("questions.jsonl");
            match crate::event_store::journal_text_checked(
                &path,
                &crate::event_store::EventQuery::of_types(crate::needs::QUESTION_TYPES),
            ) {
                Ok(text) => text,
                Err(e) => {
                    warnings.push(format!(
                        "operator questions unreadable ({e}); waiting workers counted"
                    ));
                    String::new()
                }
            }
        }
        None => String::new(),
    }
}

/// Transcript quiet in seconds for a claude session id; `None` when the
/// transcript cannot be found or its age cannot be read.
fn transcript_age_s(sid: &str) -> Option<u64> {
    let path = crate::claude_drive::find_transcript(sid)?;
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    std::time::SystemTime::now()
        .duration_since(modified)
        .ok()
        .map(|d| d.as_secs())
}

/// IO wrapper over `awaiting_operator`: reads the questions journal and
/// measures transcript quiet through the claude transcript lookup. Never
/// fails: an unreadable journal reads as no questions, so waiting workers
/// stay counted.
pub(crate) fn read_awaiting_operator(
    registry_path: &Path,
    rows: &[&RegistryEntry],
    warnings: &mut Vec<String>,
) -> BTreeMap<String, String> {
    let raw = read_questions_journal(registry_path, warnings);
    awaiting_operator(rows, &raw, transcript_age_s)
}

/// One provider lane reading: the count the gate refuses on, the registry row
/// names it counted, the parked pairs it left out, and the slot claims it
/// counted. A claude row with an open operator question and a quiet
/// transcript holds its process and its slot but spends nothing on the lane,
/// so it stops counting against the provider cap.
#[derive(Debug)]
pub(crate) struct LaneReading {
    pub count: usize,
    pub counted: Vec<String>,
    pub parked: Vec<(String, String)>,
    /// The `(name, expires_at)` pairs of the slot claims the count included:
    /// the lane's held-by-claim share, reservations named.
    pub reserved: Vec<(String, i64)>,
    /// The pending launch records the count included: a dispatcher's record
    /// stands between the gate pass and the session report, where no row
    /// exists to count, so the record itself is the seat.
    pub launches: Vec<String>,
}

/// Count rows of ONE provider only when status and positive liveness agree
/// (the port of `spawn_gate.provider_live_count`). Every unreadable source is
/// an `Err`, never a zero.
pub(crate) fn provider_live_count(
    registry_path: &Path,
    provider: &str,
    redeemer: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<LaneReading, String> {
    let questions_raw = read_questions_journal(registry_path, warnings);
    let watched = watched_ids(registry_path, warnings);
    provider_live_count_with_questions(
        registry_path,
        provider,
        &questions_raw,
        &watched,
        redeemer,
        warnings,
    )
}

/// Registry rows currently parked on a `<watching>` wait: row identity ->
/// blocker. A worker that has pushed and waits on CI, a review, or the merge
/// slot holds no working seat - the settle and expiry arms own its resume.
/// An unreadable journal warns and means nobody parked (the count's rule: an
/// unreadable source never produces a smaller number).
pub(crate) fn watched_ids(
    registry_path: &Path,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, String> {
    const SEAT_WATCH_WINDOW_MS: i64 = 2 * 60 * 60 * 1000;
    let Some(home) = registry_path.parent().map(crate::paths::AgentsHome::at) else {
        return BTreeMap::new();
    };
    let evidence = match crate::watch_expiry::read_evidence_since(
        &home,
        crate::watch_expiry::millis_now(),
        SEAT_WATCH_WINDOW_MS,
    ) {
        Ok(evidence) => evidence,
        Err(error) => {
            warnings.push(format!(
                "watch idles unreadable ({error}); watched workers counted"
            ));
            return BTreeMap::new();
        }
    };
    crate::watch_expiry::watch_blockers(&evidence)
}

/// The same count over a pre-read journal, so a caller counting MANY providers
/// (the lanes probe) reads the journal once instead of once per provider.
pub(crate) fn provider_live_count_with_questions(
    registry_path: &Path,
    provider: &str,
    questions_raw: &str,
    watched: &BTreeMap<String, String>,
    redeemer: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<LaneReading, String> {
    let registry =
        load_registry(registry_path).map_err(|e| format!("fno registry unreadable: {e}"))?;
    let live_rows: Vec<&RegistryEntry> = registry
        .entries
        .iter()
        .filter(|e| status_is_liveish(&e.status))
        .collect();

    // Rows minted without a provider stamp get ONE warning per (harness,
    // origin) shape; a hand-started (operator-origin) row can never carry the
    // stamp, so it is not the defect the warning names.
    let mut unattributed: std::collections::BTreeMap<(String, String), usize> = Default::default();
    for row in &live_rows {
        if row.provider.as_deref().is_some_and(|p| !p.is_empty()) {
            continue;
        }
        if row.origin.as_deref() == Some("operator") {
            continue;
        }
        let shape = (
            (!row.harness_name().is_empty())
                .then(|| row.harness_name().to_string())
                .unwrap_or_else(|| "unknown".into()),
            row.origin.clone().unwrap_or_else(|| "unknown".into()),
        );
        *unattributed.entry(shape).or_default() += 1;
    }
    for ((harness, origin), count) in &unattributed {
        warnings.push(format!(
            "{count} live row(s) were minted without a provider stamp (harness={harness}, origin={origin})"
        ));
    }

    let candidates: Vec<&RegistryEntry> = live_rows
        .iter()
        .copied()
        .filter(|row| row.provider.as_deref() == Some(provider))
        .collect();

    let waiting = awaiting_operator(&candidates, questions_raw, transcript_age_s);

    // Watch parks: a row whose session carries a current seat-wait watch
    // (`<watching>` on ci, review, or the merge slot) is idle until the
    // daemon wakes it, so its seat counts as free.
    let watch_parked: BTreeMap<&str, &str> = candidates
        .iter()
        .filter_map(|row| {
            let id = crate::watch_expiry::registry_session_id(row)?;
            watched.get(id).map(|b| (row.name.as_str(), b.as_str()))
        })
        .collect();

    let bg_short_ids: BTreeSet<String> = candidates
        .iter()
        .filter(|row| row.pid.is_none() && row.harness_name() == "claude")
        .filter_map(|row| row.transport_short())
        .map(str::to_string)
        .collect();
    let bg_live = if bg_short_ids.is_empty() {
        BTreeSet::new()
    } else {
        provider_roster_live_short_ids(&bg_short_ids)?
    };

    let mut count = 0usize;
    let mut counted_names: Vec<String> = Vec::new();
    let mut parked: Vec<(String, String)> = Vec::new();

    for row in &candidates {
        if let Some(qid) = waiting.get(row.name.as_str()) {
            parked.push((row.name.clone(), qid.clone()));
            continue;
        }
        if let Some(blocker) = watch_parked.get(row.name.as_str()) {
            parked.push((row.name.clone(), format!("watch:{blocker}")));
            continue;
        }
        if let Some(pid) = row.pid {
            if row.pid_start_time.is_none() {
                // A pid without its incarnation token: a decided death skips,
                // and the pane probe settles everything else. An undecidable
                // pid does NOT fault here - the pane gets the chance first,
                // exactly as the Python counter ordered it.
                if pid_liveness(pid, None) == Ok(false) {
                    continue;
                }
                match pane_state(row)? {
                    Some(true) => {
                        count += 1;
                        counted_names.push(row.name.clone());
                    }
                    Some(false) => {}
                    None => {
                        return Err(format!(
                            "process incarnation token missing for {}",
                            row.name
                        ))
                    }
                }
                continue;
            }
            // With a recorded start time, pid reuse fails closed.
            if pid_liveness(pid, row.pid_start_time)
                .map_err(|_| format!("process incarnation unreadable for {}", row.name))?
            {
                count += 1;
                counted_names.push(row.name.clone());
            }
            continue;
        }
        match pane_state(row)? {
            Some(true) => {
                count += 1;
                counted_names.push(row.name.clone());
                continue;
            }
            Some(false) => continue,
            None => {}
        }
        if row.mux.is_some() {
            // Unreadable is not absent: the cap refuses before the bg fallback.
            return Err(format!("pane liveness unreadable for {}", row.name));
        }
        if let Some(short) = row.transport_short() {
            if bg_live.contains(short) {
                count += 1;
                counted_names.push(row.name.clone());
            }
        }
    }
    // A parked row's own worker reservation must not count it back: the
    // claims dedup sees both lists.
    let mut claim_seen = counted_names.clone();
    claim_seen.extend(parked.iter().map(|(n, _)| n.clone()));
    let (claim_count, reserved) =
        provider_live_slot_claims(provider, &claim_seen, redeemer, warnings)?;
    count += claim_count;
    let launches = pending_launch_records(registry_path, provider);
    count += launches.len();
    Ok(LaneReading {
        count,
        counted: counted_names,
        parked,
        reserved,
        launches,
    })
}

/// The pending dispatch records holding `provider`'s lane (the change-7
/// reservation rule): a burst of dispatches cannot all pass on one free
/// slot, because the first unclaimed record already holds it. A missing
/// record dir is the no-dispatch-yet case, not a fault - the same posture
/// the claims share takes.
fn pending_launch_records(registry_path: &Path, provider: &str) -> Vec<String> {
    let Some(home) = registry_path.parent().map(crate::paths::AgentsHome::at) else {
        return Vec::new();
    };
    crate::launch_record::pending_for_provider(home.root(), provider, crate::claims::now_ms())
}

/// Pane liveness for one row: `Some(bool)` decided, `None` when the row
/// carries no pane ref, `Err` when the probe crashed (fail closed).
fn pane_state(row: &RegistryEntry) -> Result<Option<bool>, String> {
    match &row.mux {
        Some(mux) => mux_pane_alive(&mux.session, mux.pane_id)
            .map(Some)
            .map_err(|()| format!("pane liveness unreadable for {}", row.name)),
        None => Ok(None),
    }
}

/// Provider-tagged headless reservations not represented by rows
/// (`_provider_live_slot_claims`). A Suspect reservation (dead pid inside its
/// TTL) counts as live, as `live_worker_slot_claims` counts it. A corrupted
/// one refuses. Returns the count beside the `(name, expires_at)` pairs it
/// counted, so a refusal can name who holds the lane. `redeemer` skips that
/// one name when its claim carries the `reserved_by` key: a reserved spawn is
/// not charged for its own reservation, and a live worker's plain slot claim
/// is never skipped by borrowing its name.
pub(crate) fn provider_live_slot_claims(
    provider: &str,
    counted_names: &[String],
    redeemer: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<(usize, Vec<(String, i64)>), String> {
    let root = match claims::global_claims_root() {
        Some(root) => root,
        None => return Ok((0, Vec::new())),
    };
    let dir = match claims::claims_dir_for(Some(&root)) {
        Some(dir) => dir,
        None => return Ok((0, Vec::new())),
    };
    let keys = match crate::claim_store::records_in(&dir, Some("worker:"), true) {
        Ok(records) => records.into_iter().map(|r| r.key),
        Err(_) => return Ok((0, Vec::new())),
    };
    let counted: HashSet<&str> = counted_names.iter().map(String::as_str).collect();
    let mut count = 0usize;
    let mut reserved: Vec<(String, i64)> = Vec::new();
    for key in keys {
        let name = key.strip_prefix("worker:").unwrap_or(&key);
        if counted.contains(name) {
            continue;
        }
        let (state, record) = claims::status(&key, Some(&root));
        match state {
            ClaimState::Free | ClaimState::Stale => continue,
            ClaimState::Corrupted => return Err(format!("worker reservation {key} is corrupted")),
            _ => {}
        }
        let is_reservation = record
            .as_ref()
            .is_some_and(|rec| rec.metadata.get("reserved_by").is_some());
        if redeemer == Some(name) && is_reservation {
            continue;
        }
        let model_provider = record.as_ref().and_then(|rec| {
            rec.metadata
                .get("model_provider")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let Some(model_provider) = model_provider else {
            warnings.push(format!(
                "live worker reservation {key} was minted without model_provider; skipping"
            ));
            continue;
        };
        if model_provider.is_empty()
            || model_provider == "__uncapped__"
            || model_provider != provider
        {
            continue;
        }
        match state {
            ClaimState::Live | ClaimState::Suspect => {
                count += 1;
                reserved.push((
                    name.to_string(),
                    record.as_ref().and_then(|rec| rec.expires_at).unwrap_or(0),
                ));
            }
            _ => {}
        }
    }
    Ok((count, reserved))
}

// ---------------------------------------------------------------------------
// Lead share
// ---------------------------------------------------------------------------

/// One share reading over the registry: the teamed sessions, the share, the
/// caller's held rows and the unattributed bucket (`share_reading` +
/// `org.teamed_sessions`). Every count is `None` when the registry is
/// unreadable — there is nothing to enforce and no zero to fail open on.
pub(crate) struct ShareReading {
    pub leads: Option<usize>,
    pub share: Option<usize>,
    pub held: Option<usize>,
    pub held_rows: Option<Vec<String>>,
    pub unattributed_rows: Option<Vec<String>>,
}

pub(crate) fn share_reading(
    registry_path: &Path,
    cap: usize,
    caller: Option<&str>,
    terminal_runs: Option<&Path>,
) -> ShareReading {
    let registry = match load_registry(registry_path) {
        Ok(registry) => registry,
        Err(_) => {
            return ShareReading {
                leads: None,
                share: None,
                held: None,
                held_rows: None,
                unattributed_rows: None,
            }
        }
    };
    let mut teamed: HashSet<String> = HashSet::new();
    let mut held_rows: Vec<String> = Vec::new();
    let mut unattributed: Vec<String> = Vec::new();
    let terminal = terminal_runs.map(crate::terminal_run::read_keys);
    for e in &registry.entries {
        // A team counts until it is cleared or its row is permanently dead.
        // A reboot leaves every teamed row reading exited while the team
        // still stands, and dropping those teams read leads=0 and a share
        // of 1 for everyone.
        if let Some(session) = e.harness_session_id.as_deref() {
            if e.role_level.is_some()
                && !session.is_empty()
                && e.status != AgentStatus::PermanentDead
            {
                teamed.insert(session.to_string());
            }
        }
        if !status_is_liveish(&e.status) || e.role_level.is_some() {
            continue;
        }
        // A row whose run reported a releasing terminal reason is a parked
        // finished worker, not active work: it stops charging its lead's
        // share while it stays alive for late review mail. A row
        // without a harness session id can never match a receipt and keeps
        // counting, and an unreadable receipt dir reads as empty (no skips,
        // today's counting).
        if let Some(set) = &terminal {
            if e.harness_session_id
                .as_deref()
                .is_some_and(|sid| set.contains(sid))
            {
                continue;
            }
        }
        match e.spawned_by_session.as_deref() {
            Some(spawner) if Some(spawner) == caller => held_rows.push(e.name.clone()),
            Some(_) => {}
            None => unattributed.push(e.name.clone()),
        }
    }
    let leads = teamed.len();
    // The Python `_lead_share` divisor expression (`teamed | {caller if
    // caller in teamed}`) is a set union that can never grow the set, so the
    // divisor is exactly the team count. The caller folds in only when
    // itself teamed - i.e. never as an extra vote (LD2).
    let divisor = leads;
    let share = if divisor == 0 {
        1
    } else {
        (cap / divisor).max(1)
    };
    ShareReading {
        leads: Some(leads),
        share: Some(share),
        held: Some(held_rows.len()),
        held_rows: Some(held_rows),
        unattributed_rows: Some(unattributed),
    }
}

/// Return the caller's row only when team settlement confirms it will be vacated.
pub(crate) fn succession_replaces(
    live: &[RegistryEntry],
    caller: Option<&str>,
    scope: &str,
) -> Result<String, &'static str> {
    let caller = caller
        .filter(|session| !session.is_empty())
        .ok_or("no_caller")?;
    let mut rows = live.iter().filter(|row| {
        row.harness_session_id.as_deref() == Some(caller)
            || row.cc_session_id.as_deref() == Some(caller)
    });
    let row = rows.next().ok_or("caller_not_live")?;
    if rows.next().is_some() {
        return Err("caller_ambiguous");
    }
    let rows = serde_json::to_value(live).map_err(|_| "settle_unreadable")?;
    // Occupancy probe only: an empty proposal skips the name contract and
    // slot_probe keeps the settle from drawing, so a gate retry never burns
    // a name. The successor's name settles on the spawn path.
    let answer = crate::team_settle::resolve(&serde_json::json!({
        "scope": scope,
        "rows": rows,
        "succession": true,
        "proposed_name": "",
        "slot_probe": true,
        "caller": {"kind": "agent", "name": row.name.clone()},
    }))
    .map_err(|_| "settle_unreadable")?;
    let vacates_caller = answer
        .get("vacate")
        .and_then(Value::as_array)
        .is_some_and(|vacate| vacate.len() == 1 && vacate[0].as_str() == Some(row.name.as_str()));
    if answer.get("outcome").and_then(Value::as_str) == Some("succeeded") && vacates_caller {
        Ok(row.name.clone())
    } else {
        Err("caller_not_sole_holder")
    }
}

// ---------------------------------------------------------------------------
// Registry schema guard
// ---------------------------------------------------------------------------

/// Refuse a spawn only when the shared registry's writer floor is ahead of
/// this binary. Additive schemas remain readable; an unreadable or missing
/// file skips, as the Python guard skips.
pub(crate) fn check_registry_schema(
    registry_path: &Path,
    warnings: &mut Vec<String>,
) -> Result<(), crate::spawn_gate::Refusal> {
    use crate::spawn_gate::{Refusal, EXIT_REGISTRY_SCHEMA};
    let Ok(doc) = crate::registry_store::read(registry_path) else {
        return Ok(()); // fresh machine / unreadable: skip
    };
    let Some(on_disk) = doc.get("schema_version").and_then(Value::as_u64) else {
        return Ok(());
    };
    let understood = crate::state::REGISTRY_SCHEMA_VERSION as u64;
    let min_writer = doc
        .get("min_writer_version")
        .and_then(Value::as_u64)
        .unwrap_or(on_disk);
    if min_writer <= understood {
        return Ok(());
    }
    let writer_rev = doc
        .get("writer_rev")
        .and_then(Value::as_str)
        .unwrap_or("unstamped");
    let reader_rev = env!("FNO_AGENTS_GIT_REV");
    warnings.push(format!(
        "spawn-gate: shared agent registry at {} was written by {writer_rev} with \
         schema_version={on_disk} and min_writer_version={min_writer}; this reader is \
         {reader_rev} and understands schema_version={understood}. Refusing to spawn; \
         upgrade this fno with fno doctor update.",
        registry_path.display(),
    ));
    let data_dir = registry_path.parent().unwrap_or_else(|| Path::new("."));
    let latch_path = data_dir.join("version-skew.last");
    let latch_key = format!("{writer_rev}:{min_writer}:{understood}");
    if !matches!(std::fs::read_to_string(&latch_path), Ok(stored) if stored == latch_key) {
        let emitted = crate::events::EventEmitter::new(data_dir.join("events.jsonl"), "rust")
            .emit(
                "version_skew",
                &serde_json::json!({
                    "on_disk": on_disk,
                    "min_writer": min_writer,
                    "understood": understood,
                    "writer_rev": writer_rev,
                    "reader_rev": reader_rev,
                }),
            )
            .is_ok();
        if emitted {
            let _ = std::fs::write(latch_path, latch_key);
        }
    }
    Err(Refusal::with_receipt(
        EXIT_REGISTRY_SCHEMA,
        serde_json::json!({
            "status": "refused",
            "reason": "registry_schema",
            "registry_path": registry_path.to_string_lossy(),
            "on_disk": on_disk,
            "min_writer": min_writer,
            "understood": understood,
            "writer_rev": writer_rev,
            "reader_rev": reader_rev,
            "remedy": "fno doctor update",
        }),
    ))
}

// ---------------------------------------------------------------------------
// Account quota lock
// ---------------------------------------------------------------------------

/// A vendor quota window on the caller-named account: refuse exit 78 with
/// reason `provider_quota_lock` while the account's `rate_limited_until` is in
/// the future. NOT machine busy-ness, so `--force` never buys past it. An
/// unreadable state file reads as unlocked, exactly as the Python reader's
/// missing-disk arm reads (`is_in_cooldown`).
pub(crate) fn check_account_quota_lock(
    config_cwd: &Path,
    account: &str,
    warnings: &mut Vec<String>,
) -> Result<(), crate::spawn_gate::Refusal> {
    use crate::spawn_gate::{Refusal, EXIT_PROVIDER_CAP};
    if account.is_empty() || account == "default" {
        return Ok(());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let Some(resets_at) = account_locked_until(&runtime_state_payload(config_cwd), account, now)
    else {
        return Ok(());
    };
    let when = chrono_like_iso(resets_at);
    warnings.push(format!(
        "spawn-gate: account {account} is rate-limited until {when}; refusing; no worker launched"
    ));
    Err(Refusal::with_receipt(
        EXIT_PROVIDER_CAP,
        serde_json::json!({
            "status": "refused",
            "reason": "provider_quota_lock",
            "account": account,
            "resets_at": resets_at,
        }),
    ))
}

/// The auth wall beside the quota lock: a spawn naming an account whose
/// config dir cannot log in is refused before launch, so the node is not
/// counted as dispatched while nobody works on it. Claude-only: codex and
/// opencode status verbs read local state and cannot see a server-side
/// expiry, so a probe built on them could only ever say yes. The rules and
/// their order live in [`check_account_login_with`]; this wrapper binds the
/// production reader and probe so tests can inject both.
pub(crate) fn check_account_login(
    route_provider: Option<&str>,
    account: &str,
    warnings: &mut Vec<String>,
) -> Result<(), crate::spawn_gate::Refusal> {
    check_account_login_with(
        route_provider,
        account,
        crate::reentry::shell_account_binding,
        crate::claude_login::probe_config_dir,
        crate::claude_login::record_verdict_ambient,
        warnings,
    )
}

/// The decision body, split for tests: `binding`, `probe` and `record` are
/// injected, so
/// a test never shells out. Rules, in order:
/// 1. no account or `default`: admit, nothing is called (the quota lock's rule).
/// 2. a route to any provider other than anthropic: admit, nothing is called
///    - a routed worker authenticates with the route's key, not the slot's login.
/// 3. an unreadable binding: admit; the Python seam's own resolver owns that
///    refusal and runs before the gate.
/// 4. a binding with no config dir (an api-key lane): admit, nothing to probe.
/// 5. a readable config dir: probe it. Logged in: admit. A logged-out verdict
///    refuses with the quota lock's exit code and a receipt naming the
///    account, the dir and the login command. An INCONCLUSIVE probe (timeout,
///    unparseable output, a non-auth error) pushes a note and admits - a
///    silent fail-open would hide a probe broken by a future claude flag
///    rename, so the note is the lane's one honesty signal.
/// A DECIDED verdict (logged in or out) also goes to `record` before the
/// result returns, so launch-time picking sees the refusal without paying
/// for another probe; an inconclusive one records nothing.
pub(crate) fn check_account_login_with(
    route_provider: Option<&str>,
    account: &str,
    binding: impl Fn(&str) -> Result<Option<String>, String>,
    probe: impl Fn(&Path) -> crate::claude_login::Login,
    record: impl Fn(&str, &Path, &crate::claude_login::Login),
    warnings: &mut Vec<String>,
) -> Result<(), crate::spawn_gate::Refusal> {
    use crate::claude_login::Login;
    use crate::spawn_gate::{Refusal, EXIT_PROVIDER_CAP};
    if account.is_empty() || account == "default" {
        return Ok(());
    }
    if route_provider.is_some_and(|p| p != "anthropic") {
        return Ok(());
    }
    let Ok(Some(dir)) = binding(account) else {
        // An Err reads admit, same as an api-key lane (rule 3/4).
        return Ok(());
    };
    let dir = PathBuf::from(dir);
    match probe(&dir) {
        Login::LoggedIn => {
            record(account, &dir, &Login::LoggedIn);
            Ok(())
        }
        Login::Unknown(why) => {
            warnings.push(format!(
                "spawn-gate note: account {account} login probe inconclusive ({why}); not refusing on it"
            ));
            Ok(())
        }
        Login::LoggedOut(detail) => {
            let remedy = crate::claude_login::login_command(&dir);
            record(account, &dir, &Login::LoggedOut(detail.clone()));
            warnings.push(format!(
                "spawn-gate: account {account} cannot log in ({detail}); refusing; no worker launched; run: {remedy}"
            ));
            Err(Refusal::with_receipt(
                EXIT_PROVIDER_CAP,
                serde_json::json!({
                    "status": "refused",
                    "reason": "account_not_logged_in",
                    "account": account,
                    "config_dir": dir,
                    "detail": detail,
                    "remedy": remedy,
                }),
            ))
        }
    }
}

/// The same wall on the ROUTE axis: a route-keyed spawn (`--provider zai`,
/// no `--account`) never reaches the account check above, so the lane
/// snapshot the daemon persists every PROVIDER_CAP_INTERVAL_S, armed or
/// not, is the provider-keyed lock it refuses on. A missing or unreadable
/// snapshot reads unlocked, exactly as the unreadable runtime-state arm
/// does; a reset at or before now reads unlocked (the lane is returning).
pub(crate) fn check_lane_quota_lock(
    home_root: &Path,
    provider: &str,
    warnings: &mut Vec<String>,
) -> Result<(), crate::spawn_gate::Refusal> {
    use crate::spawn_gate::{Refusal, EXIT_PROVIDER_CAP};
    if provider.is_empty() {
        return Ok(());
    }
    let home = crate::paths::AgentsHome::at(home_root.to_path_buf());
    let Some(snapshot) = crate::provider_cap::read_persisted_snapshot(&home) else {
        return Ok(());
    };
    let provider_lanes: Vec<_> = snapshot
        .lanes
        .iter()
        .filter(|lane| lane.provider == provider)
        .collect();
    if provider_lanes.is_empty() {
        warnings.push(format!(
            "spawn-gate note: provider lane {provider} quota unmeasured (no lane in snapshot); not refusing on it"
        ));
    } else if provider_lanes.iter().all(|lane| lane.state == "unmeasured") {
        warnings.push(format!(
            "spawn-gate note: provider lane {provider} quota unmeasured (no member measured); not refusing on it"
        ));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let now_epoch = now as i64;
    let Some(lane) = snapshot.lanes.iter().find(|l| {
        if l.provider != provider || l.state != "open" || l.reset_passed_epoch.is_some() {
            return false;
        }
        if l.reset_epoch.map(|r| r as f64 > now).unwrap_or(false) {
            return true;
        }
        l.reset_epoch.is_none()
            && l.members
                .iter()
                .filter(|m| m.capped)
                .filter_map(|m| m.newest_assistant.as_deref())
                .filter_map(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .map(|ts| ts.timestamp())
                .max()
                .is_some_and(|ts| now_epoch >= ts && now_epoch - ts < UNKNOWN_RESET_HOLD_S)
    }) else {
        return Ok(());
    };
    let reset_unknown = lane.reset_epoch.is_none();
    if reset_unknown {
        let target = lane
            .missing_reset_timezone
            .first()
            .map(String::as_str)
            .unwrap_or(&lane.account);
        warnings.push(format!(
            "spawn-gate: provider lane {} has an unknown reset; refusing; set reset_timezone on the [[accounts.records]] entry for {target}; no worker launched",
            lane.lane
        ));
    } else {
        let when = chrono_like_iso(lane.reset_epoch.unwrap() as f64);
        warnings.push(format!(
            "spawn-gate: provider lane {} is rate-limited until {when}; refusing; no worker launched",
            lane.lane
        ));
    }
    Err(Refusal::with_receipt(
        EXIT_PROVIDER_CAP,
        serde_json::json!({
            "status": "refused",
            "reason": "provider_quota_lock",
            "provider": provider,
            "lane": lane.lane,
            "resets_at": lane.reset_epoch.map(|r| r as f64),
            "reset_unknown": reset_unknown,
            "missing_reset_timezone": lane.missing_reset_timezone,
        }),
    ))
}

fn runtime_state_payload(config_cwd: &Path) -> Value {
    crate::route_capacity::runtime_state_payload(config_cwd)
}

/// The account's binding rate-limit deadline, when its health entry is fresh
/// (the shared record-level TTL) and the lock is in the future. Reuses the
/// fallback-chain's health parse instead of a second one.
pub(crate) fn account_locked_until(raw: &Value, account: &str, now: f64) -> Option<f64> {
    let health = crate::fallback_chain::parse_provider_health(raw).remove(account)?;
    let rlu = health.get("rate_limited_until").and_then(Value::as_f64)?;
    match health.get("last_error_at").and_then(Value::as_f64) {
        Some(at) if at < now - PROVIDER_HEALTH_TTL_SECONDS => return None,
        _ => {}
    }
    (rlu > now).then_some(rlu)
}

/// Epoch seconds as a UTC ISO-8601 string for the refusal prose; the receipt
/// keeps the epoch number the Python receipt kept.
fn chrono_like_iso(epoch_s: f64) -> String {
    chrono::DateTime::from_timestamp(epoch_s as i64, 0)
        .map(|dt| dt.to_string())
        .unwrap_or_else(|| "unknown (no reset was readable)".to_string())
}

/// The lead-share axis: one lead holding more than its share of the
/// effective cap refuses its own spawn, naming the rows it can stop. Lives
/// beside `share_reading`, the count it refuses on.
pub(crate) fn check_lead_share(
    registry_path: &Path,
    learned: &crate::capacity::Effective,
    caller_session: Option<&str>,
    terminal_runs: Option<&Path>,
    _axes_read: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), crate::spawn_gate::Refusal> {
    let cap = learned.cap;
    let Some(caller) = caller_session.filter(|c| !c.is_empty()) else {
        return Ok(());
    };
    let reading = share_reading(registry_path, cap, Some(caller), terminal_runs);
    let (Some(leads), Some(share), Some(held)) = (reading.leads, reading.share, reading.held)
    else {
        // An unreadable registry leaves every count unknown; nothing to
        // enforce and no zero to fail open on.
        return Ok(());
    };
    if held < share {
        return Ok(());
    }
    let mut msg = format!(
        "spawn-gate: lead {} holds {held} of {} across {leads} leads (share {share}); \
         refusing to spawn -- waiting cannot help while your own workers hold the share \
         (--force to bypass)",
        &caller[..caller.len().min(8)],
        learned.clause()
    );
    // The held names read before the unattributed bucket: they are the rows
    // the caller can stop, where the bucket names nobody.
    msg.push_str(&crate::spawn_gate::held_rows_suffix(
        reading.held_rows.as_ref(),
    ));
    if let Some(rows) = reading.unattributed_rows.filter(|r| !r.is_empty()) {
        let shown: Vec<String> = rows.iter().take(5).cloned().collect();
        msg.push_str(&format!(
            "; {} live row(s) name nobody and sit in the unattributed bucket ({}{})",
            rows.len(),
            shown.join(", "),
            if rows.len() > 5 { "..." } else { "" }
        ));
    }
    eprintln!("{msg}");
    learned.eprint_slot_hint();
    Err(
        crate::spawn_gate::Refusal::code(crate::spawn_gate::EXIT_LEAD_SHARE)
            .ev("reason", serde_json::json!("lead_share"))
            .ev("lead", serde_json::json!(caller))
            .ev("held", serde_json::json!(held))
            .ev("share", serde_json::json!(share))
            .ev("max_live", serde_json::json!(cap))
            .ev("leads", serde_json::json!(leads))
            .ev(
                "held_rows",
                serde_json::json!(reading.held_rows.clone().unwrap_or_default()),
            ),
    )
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn write_registry(path: &Path, entries: &[String]) {
        std::fs::write(
            path,
            format!(
                r#"{{"schema_version":{},"entries":[{}]}}"#,
                crate::state::REGISTRY_SCHEMA_VERSION,
                entries.join(",")
            ),
        )
        .unwrap();
    }

    fn live_row(name: &str, provider: &str, pid: Option<u32>) -> String {
        let fields = pid
            .map(|p| {
                let start = crate::daemon::process_start_time(p).unwrap_or(0);
                format!(r#","pid":{p},"pid_start_time":{start}"#)
            })
            .unwrap_or_default();
        format!(
            r#"{{"name":"{name}","provider":"{provider}","cwd":"/tmp","status":"live","created_at":"2026-01-01T00:00:00Z"{fields}}}"#
        )
    }

    /// A claude worker row with a session id and no pid (the bg shape).
    fn claude_row_json(name: &str, sid: &str) -> String {
        format!(
            r#"{{"name":"{name}","harness":"claude","provider":"zai","cwd":"/tmp","status":"live","created_at":"2026-01-01T00:00:00Z","harness_session_id":"{sid}"}}"#
        )
    }

    /// A spawned worker row: live, carries its harness session id and the
    /// lead session that spawned it (the lead-share held-row shape).
    fn spawned_row(name: &str, sid: &str, spawner: &str) -> String {
        format!(
            r#"{{"name":"{name}","harness":"claude","provider":"zai","cwd":"/tmp","status":"live","created_at":"2026-01-01T00:00:00Z","harness_session_id":"{sid}","spawned_by_session":"{spawner}"}}"#
        )
    }

    /// The finished-worker counting rule: a live caller-spawned row whose harness
    /// session id carries a terminal-run receipt stops counting toward
    /// lead_share on the next gate read, and counts again once the receipt
    /// clears (the loop went live again). A missing receipt dir reads as no
    /// receipts: today's counting, the fail-safe direction.
    #[test]
    fn a_terminal_receipt_row_stops_counting_toward_lead_share() {
        let _guard = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-lanes-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = dir.join("registry.json");
        let sid = "c79bce5b-1682-4a8c-97f4-d6254cf18112";
        write_registry(&reg, &[spawned_row("t-w-done", sid, "lead-1")]);

        let home = crate::paths::AgentsHome::at(&dir);
        let runs = home.terminal_runs_dir();

        // No receipt: the row counts (today's refusal fires unchanged).
        let reading = share_reading(&reg, 3, Some("lead-1"), Some(&runs));
        assert_eq!(reading.held, Some(1), "no receipt: the row counts");

        // DonePRGreen receipt dropped: the same row stops counting.
        crate::terminal_run::write_receipt(&home, sid, "DonePRGreen").unwrap();
        let reading = share_reading(&reg, 3, Some("lead-1"), Some(&runs));
        assert_eq!(reading.held, Some(0), "receipt: the finished row skips");
        assert!(
            reading
                .held_rows
                .unwrap_or_default()
                .iter()
                .all(|n| n != "t-w-done"),
            "the skipped row is not named as held"
        );

        // Receipt cleared (the loop went live again): the row counts again.
        crate::terminal_run::remove_receipt(&home, sid);
        let reading = share_reading(&reg, 3, Some("lead-1"), Some(&runs));
        assert_eq!(
            reading.held,
            Some(1),
            "cleared receipt: the row counts again"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The skip is keyed on the receipt, not the caller: another lead's
    /// finished row stays in ITS lead's count, and an unreadable dir reads
    /// as no receipts (count everything).
    #[test]
    fn the_receipt_skip_is_keyed_on_the_row_not_the_caller() {
        let _guard = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-lanes-share2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = dir.join("registry.json");
        let sid = "c79bce5b-1682-4a8c-97f4-d6254cf18112";
        write_registry(
            &reg,
            &[
                spawned_row("t-w-lead2", sid, "lead-2"),
                spawned_row("t-w-lead1", "aaaaaaaa-bb-cc-dd-eeeeeeeeeeee", "lead-1"),
            ],
        );
        let home = crate::paths::AgentsHome::at(&dir);
        let runs = home.terminal_runs_dir();
        crate::terminal_run::write_receipt(&home, sid, "DonePRGreen").unwrap();

        // lead-2's own finished row skips; lead-1's live row still counts.
        let reading = share_reading(&reg, 3, Some("lead-2"), Some(&runs));
        assert_eq!(reading.held, Some(0), "lead-2's finished row skips");
        let reading = share_reading(&reg, 3, Some("lead-1"), Some(&runs));
        assert_eq!(reading.held, Some(1), "lead-1's working row still counts");

        // A nonexistent dir reads as no receipts: lead-2's row counts.
        let reading = share_reading(&reg, 3, Some("lead-2"), Some(&dir.join("nope")));
        assert_eq!(reading.held, Some(1), "missing dir: count everything");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AC1-HP's counting rule: live rows of the provider count, dead pids and
    /// other providers do not.
    #[test]
    fn provider_count_counts_live_rows_of_the_provider_only() {
        let _guard = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-lanes-count-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FNO_CLAIMS_ROOT", dir.join("claims-root"));
        let reg = dir.join("registry.json");
        write_registry(
            &reg,
            &[
                live_row("a", "zai", Some(std::process::id())),
                live_row("b", "zai", Some(std::process::id())),
                // dead pid: provably gone, uncounted
                live_row("c", "zai", Some(4_194_321)),
                // other provider: not this lane
                live_row("d", "codex", Some(std::process::id())),
            ],
        );
        let mut warnings = Vec::new();
        let reading = provider_live_count(&reg, "zai", None, &mut warnings).unwrap();
        assert_eq!(reading.count, 2, "two live zai rows");
        assert_eq!(reading.counted, vec!["a".to_string(), "b".to_string()]);
        assert!(
            reading.parked.is_empty(),
            "no parked rows in the plain fixture"
        );
        assert!(
            reading.reserved.is_empty(),
            "no slot claims in the plain fixture"
        );
        std::env::remove_var("FNO_CLAIMS_ROOT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dispatch reservation rule: a pending record the dispatcher wrote
    /// before exec holds the provider's lane through the exec-to-adopt gap,
    /// and the reading names it. Another provider's record, a provider-less
    /// one, and a record past its claim window hold nothing.
    #[test]
    fn a_pending_dispatch_record_holds_the_lane() {
        let _guard = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-lanes-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FNO_CLAIMS_ROOT", dir.join("claims-root"));
        let reg = dir.join("registry.json");
        write_registry(&reg, &[]);
        let now = crate::claims::now_ms();
        let launch = |name: &str, provider: Option<&str>| crate::launch_record::Launch {
            harness: "claude".into(),
            name: Some(name.into()),
            node: None,
            slot: None,
            provider: provider.map(str::to_string),
            cwd: "/tmp".into(),
        };
        crate::launch_record::write_record(
            &dir,
            launch("w-burst", Some("zai")),
            "lead-1",
            None,
            "/tmp",
            now,
        )
        .unwrap();
        crate::launch_record::write_record(
            &dir,
            launch("w-other-lane", Some("codex")),
            "lead-1",
            None,
            "/tmp",
            now,
        )
        .unwrap();
        crate::launch_record::write_record(
            &dir,
            launch("w-typed-line", None),
            "lead-1",
            None,
            "/tmp",
            now,
        )
        .unwrap();
        crate::launch_record::write_record(
            &dir,
            launch("w-lapsed", Some("zai")),
            "lead-1",
            None,
            "/tmp",
            now - crate::launch_record::WINDOW_MS - 1_000,
        )
        .unwrap();

        let mut warnings = Vec::new();
        let reading = provider_live_count(&reg, "zai", None, &mut warnings).unwrap();
        assert_eq!(
            reading.count, 1,
            "only the in-window zai record holds: {reading:?}"
        );
        assert_eq!(reading.launches, vec!["w-burst".to_string()]);

        let other = provider_live_count(&reg, "codex", None, &mut warnings).unwrap();
        assert_eq!(other.count, 1);
        assert_eq!(other.launches, vec!["w-other-lane".to_string()]);
        std::env::remove_var("FNO_CLAIMS_ROOT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AC1-HP at the helper: an open question plus a quiet transcript maps the
    /// row to its question id; a row with no question stays out even when its
    /// transcript is just as quiet.

    /// AC1-ACTIVE: a transcript quiet only 60 s never parks a row.

    /// AC1-CLOSED: a closed question does not park its row.

    /// The asker arm: a question carrying only the 8-character asker short id
    /// still binds to its row.

    /// Stages the watch-park home: agents home, events journal, claims root.
    fn stage_watch_home(dir: &Path) -> (PathBuf, impl Fn(&str, &str, i64) -> String) {
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::env::set_var("FNO_CLAIMS_ROOT", dir.join("claims-root"));
        let reg = dir.join("agents").join("registry.json");
        let watch_row = move |session: &str, blocker: &str, expires_at_ms: i64| {
            serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "type": "loop_check_watch_idle",
                "source": "hook",
                "data": {
                    "session_id": session,
                    "harness": "claude",
                    "node": "x-wt",
                    "pr": 42,
                    "blocker": blocker,
                    "expires_at_ms": expires_at_ms
                }
            })
            .to_string()
        };
        (reg, watch_row)
    }

    /// A worker waiting on a ci watch parks; a working row still counts and
    /// the parked row's own reservation is deduped, not counted back.
    #[test]
    fn provider_count_parks_a_worker_waiting_on_ci_watch() {
        let _guard = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-lanes-watch-{}", std::process::id()));
        let (reg, watch_row) = stage_watch_home(&dir);
        let a = "aaaaaaaa-0000-0000-0000-00000000000a";
        let expires = chrono::Utc::now().timestamp_millis() + 600_000;
        std::fs::write(
            dir.join("events.jsonl"),
            format!("{}\n", watch_row(a, "ci", expires)),
        )
        .unwrap();
        seed_worker_a_claim(&dir);
        let me = std::process::id();
        write_registry(
            &reg,
            &[claude_row_json("a", a), live_row("good", "zai", Some(me))],
        );
        let mut warnings = Vec::new();
        let reading = provider_live_count(&reg, "zai", None, &mut warnings).unwrap();
        assert_eq!(reading.count, 1, "the watch-parked row frees its seat");
        assert_eq!(reading.counted, vec!["good".to_string()]);
        assert_eq!(
            reading.parked,
            vec![("a".to_string(), "watch:ci".to_string())]
        );
        assert!(
            reading.reserved.is_empty(),
            "the parked row's own claim is deduped, not counted and not named"
        );
        std::env::remove_var("FNO_CLAIMS_ROOT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Seeds a live zai worker:a reservation under the active claims root.
    fn seed_worker_a_claim(dir: &std::path::Path) {
        let host = claims::hostname();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let lock = dir
            .join("claims-root")
            .join(".fno")
            .join("claims")
            .join(format!("{}.lock", claims::encode_key("worker:a")));
        crate::claim_store::seed_yaml_at_path(&lock, &format!("schema_version: {}\nkey: worker:a\nholder: h\nacquired_at: {now}\npid: {}\nhost: {host}\nmetadata:\n  model_provider: zai\n", claims::SCHEMA_VERSION, std::process::id()));
    }

    /// AC1-ERR: an unreadable registry refuses — never a zero that reads as
    /// an empty lane.
    #[test]
    fn provider_count_refuses_on_an_unreadable_registry() {
        let _guard = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-lanes-badreg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = dir.join("registry.json");
        std::fs::write(&reg, "{ not json").unwrap();
        let mut warnings = Vec::new();
        let err = provider_live_count(&reg, "zai", None, &mut warnings).unwrap_err();
        assert!(err.contains("fno registry unreadable"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reserved spawn is not charged for its own reservation (AC1-EDGE /
    /// AC2-HP), but a live worker's plain slot claim (no `reserved_by`) is
    /// never skipped by a spawn borrowing its name (AC2-ERR).
    #[test]
    fn provider_count_skips_the_redeemers_reservation_only() {
        let _guard = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-lanes-redeem-{}", std::process::id()));
        let root = dir.join("claims-root");
        let claims_dir = root.join(".fno").join("claims");
        std::env::set_var("FNO_CLAIMS_ROOT", &root);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let host = claims::hostname();
        for (key, holder, reserved) in [
            ("worker:t-reserved-x-4444", "lead-1", true),
            ("worker:t-live-worker", "live-holder", false),
        ] {
            let reserved_line = reserved
                .then_some("  reserved_by: lead-1\n")
                .unwrap_or_default();
            let lock = claims_dir.join(format!("{}.lock", claims::encode_key(key)));
            crate::claim_store::seed_yaml_at_path(&lock, &format!(
                    "schema_version: {}\nkey: {key}\nholder: {holder}\nacquired_at: {now}\nexpires_at: {}\npid: {}\nhost: {host}\nmetadata:\n  model_provider: zai\n{reserved_line}",
                    claims::SCHEMA_VERSION,
                    now + 600_000,
                    std::process::id()
                ));
        }
        let mut warnings = Vec::new();
        let (n, _) = provider_live_slot_claims("zai", &[], None, &mut warnings).unwrap();
        assert_eq!(n, 2, "both claims count with no redeemer");
        let (n, _) =
            provider_live_slot_claims("zai", &[], Some("t-reserved-x-4444"), &mut warnings)
                .unwrap();
        assert_eq!(n, 1, "the redeemer skips its own reservation");
        let (n, _) =
            provider_live_slot_claims("zai", &[], Some("t-live-worker"), &mut warnings).unwrap();
        assert_eq!(n, 2, "a live worker's claim is never skipped by name");
        std::env::remove_var("FNO_CLAIMS_ROOT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Additive ahead schemas pass; a floor-ahead schema refuses with a
    /// head-pinned receipt and one event per distinct writer/floor/reader.
    #[test]
    fn registry_schema_floor_controls_forward_read_and_latches_skew_event() {
        let dir = std::env::temp_dir().join(format!("fno-lanes-schema-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A registry path's parent owns its table, so each document gets its own dir.
        let put = |sub: &str, doc: String| {
            let home = dir.join(sub);
            std::fs::create_dir_all(&home).unwrap();
            let reg = home.join("registry.json");
            std::fs::write(&reg, doc).unwrap();
            reg
        };
        let reg = put(
            "additive",
            format!(
                r#"{{"schema_version":{},"min_writer_version":{},"writer_rev":"writer-a","entries":[]}}"#,
                crate::state::REGISTRY_SCHEMA_VERSION + 1,
                crate::state::REGISTRY_SCHEMA_VERSION,
            ),
        );
        let mut warnings = Vec::new();
        assert!(check_registry_schema(&reg, &mut warnings).is_ok());
        assert!(warnings.is_empty());

        let min_writer = crate::state::REGISTRY_SCHEMA_VERSION + 1;
        let reg = put(
            "floor",
            format!(
                r#"{{"schema_version":{},"min_writer_version":{min_writer},"writer_rev":"writer-a","entries":[]}}"#,
                crate::state::REGISTRY_SCHEMA_VERSION + 1
            ),
        );
        let err = check_registry_schema(&reg, &mut warnings).unwrap_err();
        assert_eq!(err.exit_code, crate::spawn_gate::EXIT_REGISTRY_SCHEMA);
        let receipt = err.receipt.unwrap();
        assert_eq!(receipt["reason"], "registry_schema");
        assert_eq!(
            receipt["on_disk"],
            crate::state::REGISTRY_SCHEMA_VERSION + 1
        );
        assert_eq!(receipt["understood"], crate::state::REGISTRY_SCHEMA_VERSION);
        assert_eq!(receipt["min_writer"], min_writer);
        assert_eq!(receipt["writer_rev"], "writer-a");
        assert_eq!(receipt["reader_rev"], env!("FNO_AGENTS_GIT_REV"));
        assert_eq!(receipt["remedy"], "fno doctor update");
        assert!(warnings[0].contains("writer-a"));
        assert!(warnings[0].contains(env!("FNO_AGENTS_GIT_REV")));
        assert!(warnings[0].contains("fno doctor update"));

        let err = check_registry_schema(&reg, &mut warnings).unwrap_err();
        let receipt = err.receipt.unwrap();
        assert_eq!(receipt["writer_rev"], "writer-a");
        assert_eq!(receipt["reader_rev"], env!("FNO_AGENTS_GIT_REV"));
        assert_eq!(receipt["remedy"], "fno doctor update");
        let journal = dir.join("floor/events.jsonl");
        let event_rows = crate::events::committed_journal_text(&journal)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["type"] == "version_skew")
            .count();
        assert_eq!(event_rows, 1);
        let event = crate::events::committed_journal_text(&journal)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|event| event["type"] == "version_skew")
            .unwrap();
        assert_eq!(event["source"], "rust");

        let latch = dir.join("floor/version-skew.last");
        std::fs::remove_file(&latch).unwrap();
        std::fs::create_dir(&latch).unwrap();
        assert!(check_registry_schema(&reg, &mut warnings).is_err());

        let reg = put(
            "huge",
            r#"{"schema_version":4294967296,"min_writer_version":4294967296}"#.to_string(),
        );
        assert!(check_registry_schema(&reg, &mut warnings).is_err());

        let reg = put(
            "current",
            format!(
                r#"{{"schema_version":{},"min_writer_version":{},"entries":[]}}"#,
                crate::state::REGISTRY_SCHEMA_VERSION,
                crate::state::REGISTRY_SCHEMA_VERSION
            ),
        );
        warnings.clear();
        assert!(check_registry_schema(&reg, &mut warnings).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The quota lock: a future rate_limited_until on the named account
    /// refuses exit 78 provider_quota_lock; another account is untouched; an
    /// unreadable state file reads as unlocked.
    #[test]
    fn quota_lock_refuses_only_the_locked_account() {
        let _guard = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-lanes-quota-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("provider-runtime-state.json");
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            + 600.0;
        std::fs::write(
            &state,
            format!(
                r#"{{"provider_health":{{"acct-a":{{"rate_limited_until":{future},"last_error_at":{}}}}}}}"#,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64()
            ),
        )
        .unwrap();
        std::env::set_var("FNO_RUNTIME_STATE_PATH", &state);

        let mut warnings = Vec::new();
        let err = check_account_quota_lock(&dir, "acct-a", &mut warnings).unwrap_err();
        assert_eq!(err.exit_code, crate::spawn_gate::EXIT_PROVIDER_CAP);
        assert_eq!(
            err.receipt.as_ref().unwrap()["reason"],
            "provider_quota_lock"
        );
        assert_eq!(err.receipt.as_ref().unwrap()["account"], "acct-a");

        warnings.clear();
        assert!(check_account_quota_lock(&dir, "acct-b", &mut warnings).is_ok());
        assert!(check_account_quota_lock(&dir, "default", &mut warnings).is_ok());

        std::env::set_var("FNO_RUNTIME_STATE_PATH", dir.join("missing.json"));
        warnings.clear();
        assert!(check_account_quota_lock(&dir, "acct-a", &mut warnings).is_ok());

        std::env::remove_var("FNO_RUNTIME_STATE_PATH");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The route lane lock: a fresh open lane with a future reset refuses
    /// exit 78 provider_quota_lock naming the lane; another provider, a
    /// passed reset, and a missing snapshot all read unlocked.
    #[test]
    fn quota_rows() {
        let dir = std::env::temp_dir().join(format!("fno-lanes-lanequota-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("provider-cap")).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let lane_json = |reset: i64| {
            format!(
                r#"{{"lanes":[{{"lane":"zai:default","provider":"zai","account":"default","reset_epoch":{reset},"reset_passed_epoch":null,"missing_reset_timezone":[],"state":"open","members":[]}}],"measured_at":"probe","measured_at_epoch":{now}}}"#
            )
        };
        std::fs::write(
            dir.join("provider-cap").join("snapshot.json"),
            lane_json(now + 600),
        )
        .unwrap();

        let mut warnings = Vec::new();
        let err = check_lane_quota_lock(&dir, "zai", &mut warnings).unwrap_err();
        assert_eq!(err.exit_code, crate::spawn_gate::EXIT_PROVIDER_CAP);
        assert_eq!(
            err.receipt.as_ref().unwrap()["reason"],
            "provider_quota_lock"
        );
        assert_eq!(err.receipt.as_ref().unwrap()["lane"], "zai:default");
        assert_eq!(
            err.receipt.as_ref().unwrap()["resets_at"].as_f64(),
            Some((now + 600) as f64)
        );

        warnings.clear();
        assert!(check_lane_quota_lock(&dir, "anthropic", &mut warnings).is_ok());

        std::fs::write(
            dir.join("provider-cap").join("snapshot.json"),
            lane_json(now - 60),
        )
        .unwrap();
        warnings.clear();
        assert!(check_lane_quota_lock(&dir, "zai", &mut warnings).is_ok());

        warnings.clear();
        assert!(check_lane_quota_lock(&dir.join("absent"), "zai", &mut warnings).is_ok());
        let _ = std::fs::remove_dir_all(&dir);

        let dir = std::env::temp_dir().join(format!("fno-lanes-unmeasured-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("provider-cap")).unwrap();
        std::fs::write(
            dir.join("provider-cap").join("snapshot.json"),
            serde_json::json!({
                "lanes": [{
                    "lane": "openai:default",
                    "provider": "openai",
                    "account": "default",
                    "reset_epoch": null,
                    "reset_passed_epoch": null,
                    "missing_reset_timezone": [],
                    "state": "unmeasured",
                    "members": []
                }],
                "measured_at": "probe",
                "measured_at_epoch": 1_000_000_000
            })
            .to_string(),
        )
        .unwrap();

        let mut warnings = Vec::new();
        assert!(check_lane_quota_lock(&dir, "openai", &mut warnings).is_ok());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("spawn-gate note:"));
        assert!(warnings[0].contains("provider lane openai quota unmeasured"));
        assert!(warnings[0].contains("no member measured"));
        let _ = std::fs::remove_dir_all(&dir);

        let dir = std::env::temp_dir().join(format!("fno-lanes-unknown-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("provider-cap")).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let snapshot = |newest: i64, reset_passed_epoch: Option<i64>| {
            serde_json::json!({
                "lanes": [{
                    "lane": "zai:default",
                    "provider": "zai",
                    "account": "default",
                    "reset_epoch": null,
                    "reset_passed_epoch": reset_passed_epoch,
                    "missing_reset_timezone": ["default"],
                    "state": "open",
                    "members": [{
                        "name": "w-1",
                        "session_id": null,
                        "harness": "claude",
                        "provider": "zai",
                        "account": "default",
                        "node": null,
                        "cwd": null,
                        "capped": true,
                        "cap_unknown": null,
                        "newest_assistant": chrono::DateTime::from_timestamp(newest, 0).unwrap().to_rfc3339(),
                        "held": null,
                        "excerpt": "API Error: 429"
                    }]
                }],
                "measured_at": "probe",
                "measured_at_epoch": now
            })
        };
        std::fs::write(
            dir.join("provider-cap").join("snapshot.json"),
            snapshot(now - 600, None).to_string(),
        )
        .unwrap();

        let mut warnings = Vec::new();
        let err = check_lane_quota_lock(&dir, "zai", &mut warnings).unwrap_err();
        assert_eq!(err.exit_code, crate::spawn_gate::EXIT_PROVIDER_CAP);
        assert_eq!(err.receipt.as_ref().unwrap()["reset_unknown"], true);
        assert!(err.receipt.as_ref().unwrap()["resets_at"].is_null());
        assert_eq!(
            err.receipt.as_ref().unwrap()["missing_reset_timezone"],
            serde_json::json!(["default"])
        );
        assert!(warnings[0].contains("set reset_timezone"));

        std::fs::write(
            dir.join("provider-cap").join("snapshot.json"),
            snapshot(now - 6 * 3600, None).to_string(),
        )
        .unwrap();
        warnings.clear();
        assert!(check_lane_quota_lock(&dir, "zai", &mut warnings).is_ok());

        std::fs::write(
            dir.join("provider-cap").join("snapshot.json"),
            snapshot(now - 600, Some(now - 60)).to_string(),
        )
        .unwrap();
        warnings.clear();
        assert!(check_lane_quota_lock(&dir, "zai", &mut warnings).is_ok());
        let _ = std::fs::remove_dir_all(&dir);

        let caps = std::collections::BTreeMap::from([
            ("zai".to_string(), Some(2)),
            ("openai".to_string(), None),
            ("anthropic".to_string(), Some(3)),
        ]);
        let readings = vendor_lane_readings_from_caps(&caps, |provider| match provider {
            "zai" => Ok(2),
            "anthropic" => Err("registry unreadable".to_string()),
            _ => unreachable!("uncapped provider must not be counted"),
        });

        assert_eq!(
            readings["vendor_caps"],
            serde_json::json!({"zai": 2, "anthropic": 3})
        );
        assert_eq!(readings["vendor_counts"], serde_json::json!({"zai": 2}));
        assert_eq!(
            readings["vendor_count_errors"],
            serde_json::json!({"anthropic": "registry unreadable"})
        );
    }

    #[test]
    fn provider_lane_caps_coerces_the_documented_config_table() {
        let _g = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("fno-provider-lane-caps-{}", std::process::id()));
        let fnodir = dir.join(".fno");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&fnodir).unwrap();
        let config_path = fnodir.join("config.toml");
        let prior_config = std::env::var_os("FNO_CONFIG");
        std::env::set_var("FNO_CONFIG", &config_path);

        let expected = |entries: &[(&str, Option<usize>)]| {
            entries
                .iter()
                .map(|(provider, cap)| ((*provider).to_string(), *cap))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let cases = [
            (
                "[agents.provider_limits]\nzai = 5\n",
                expected(&[("zai", Some(5))]),
            ),
            (
                "[agents.provider_limits.zai]\nsubagents = 2\n",
                expected(&[("zai", Some(5))]),
            ),
            (
                "[agents.provider_limits.openai]\nsubagents = 1\n",
                expected(&[("openai", None)]),
            ),
            (
                "[agents]\nmax_lanes = { openai = 3 }\n",
                expected(&[("openai", Some(3))]),
            ),
            (
                "[agents]\nmax_lanes = { openai = 3 }\n[agents.provider_limits]\nzai = 2\n",
                expected(&[("zai", Some(2))]),
            ),
            ("[agents.provider_limits]\n", expected(&[])),
            ("[agents]\nmax_live = 2\n", expected(&[("zai", Some(5))])),
        ];
        for (config, caps) in cases {
            std::fs::write(&config_path, config).unwrap();
            assert_eq!(provider_lane_caps(&dir), caps, "config: {config}");
        }

        for malformed in [
            "[agents.provider_limits]\nzai = 0\n",
            "[agents.provider_limits]\nzai = true\n",
            "[agents.provider_limits]\nzai = 2.5\n",
            "[agents.provider_limits]\nZai = 3\n",
            "[agents.provider_limits.zai]\nlanes = 2\nburst = 1\n",
        ] {
            std::fs::write(&config_path, malformed).unwrap();
            assert_eq!(
                provider_lane_caps(&dir),
                expected(&[("zai", Some(5))]),
                "malformed config: {malformed}"
            );
        }

        match prior_config {
            Some(v) => std::env::set_var("FNO_CONFIG", v),
            None => std::env::remove_var("FNO_CONFIG"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- check_account_login_with: one test per rule ----

    use crate::claude_login::Login;

    /// A binding/probe pair that counts every call, so a rule that must
    /// short-circuit proves it called nothing.
    fn login_lane_fakes(
        binding_result: Result<Option<String>, String>,
        probe_verdict: Login,
    ) -> (
        impl Fn(&str) -> Result<Option<String>, String>,
        impl Fn(&Path) -> Login,
        std::rc::Rc<std::cell::Cell<usize>>,
    ) {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let binding_calls = calls.clone();
        let probe_calls = calls.clone();
        let binding = move |_id: &str| {
            binding_calls.set(binding_calls.get() + 1);
            binding_result.clone()
        };
        let probe = move |_dir: &Path| {
            probe_calls.set(probe_calls.get() + 1);
            probe_verdict.clone()
        };
        (binding, probe, calls)
    }

    /// The recorder most rules pass: a decision must be free to record
    /// nothing when the rule never reached a verdict.
    fn no_record(_account: &str, _dir: &Path, _verdict: &Login) {}

    /// Rule 1: no account or `default` admits and calls nothing.
    #[test]
    fn login_lane_rows() {
        for account in ["", "default"] {
            let (binding, probe, calls) = login_lane_fakes(Ok(None), Login::LoggedIn);
            let mut warnings = Vec::new();
            assert!(check_account_login_with(
                None,
                account,
                binding,
                probe,
                no_record,
                &mut warnings
            )
            .is_ok());
            assert_eq!(calls.get(), 0, "account {account:?} must call nothing");
            assert!(warnings.is_empty());
        }

        let (binding, probe, calls) = login_lane_fakes(Ok(None), Login::LoggedIn);
        let mut warnings = Vec::new();
        assert!(check_account_login_with(
            Some("zai"),
            "makers",
            binding,
            probe,
            no_record,
            &mut warnings
        )
        .is_ok());
        assert_eq!(calls.get(), 0);
        assert!(warnings.is_empty());

        let (binding, probe, calls) =
            login_lane_fakes(Err("no such account".to_string()), Login::LoggedIn);
        let mut warnings = Vec::new();
        assert!(
            check_account_login_with(None, "makers", binding, probe, no_record, &mut warnings)
                .is_ok()
        );
        assert_eq!(calls.get(), 1, "binding once, probe never");
        assert!(warnings.is_empty());

        let (binding, probe, calls) = login_lane_fakes(Ok(None), Login::LoggedIn);
        let mut warnings = Vec::new();
        assert!(
            check_account_login_with(None, "makers", binding, probe, no_record, &mut warnings)
                .is_ok()
        );
        assert_eq!(calls.get(), 1, "binding once, probe never");
        assert!(warnings.is_empty());

        let (binding, probe, _calls) = login_lane_fakes(
            Ok(Some("/tmp/acct".to_string())),
            Login::LoggedOut("Login expired".to_string()),
        );
        let mut warnings = Vec::new();
        let err =
            check_account_login_with(None, "makers", binding, probe, no_record, &mut warnings)
                .expect_err("a logged-out account must refuse");
        assert_eq!(err.exit_code, crate::spawn_gate::EXIT_PROVIDER_CAP);
        let receipt = err.receipt.expect("refusal carries the receipt");
        assert_eq!(receipt["reason"], "account_not_logged_in");
        assert_eq!(receipt["account"], "makers");
        assert_eq!(receipt["config_dir"], "/tmp/acct");
        assert_eq!(receipt["detail"], "Login expired");
        assert_eq!(
            receipt["remedy"],
            "CLAUDE_CONFIG_DIR=/tmp/acct claude /login"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("no worker launched") && w.contains("claude /login")),
            "{warnings:?}"
        );

        let (binding, probe, _calls) = login_lane_fakes(
            Ok(Some("/tmp/acct".to_string())),
            Login::Unknown("probe timed out after 20s".to_string()),
        );
        let mut warnings = Vec::new();
        assert!(
            check_account_login_with(None, "makers", binding, probe, no_record, &mut warnings)
                .is_ok()
        );
        assert_eq!(
            warnings.len(),
            1,
            "exactly the inconclusive note: {warnings:?}"
        );
        assert!(warnings[0].contains("inconclusive"), "{warnings:?}");

        let (binding, probe, _calls) =
            login_lane_fakes(Ok(Some("/tmp/acct".to_string())), Login::LoggedIn);
        let mut warnings = Vec::new();
        assert!(
            check_account_login_with(None, "makers", binding, probe, no_record, &mut warnings)
                .is_ok()
        );
        assert!(warnings.is_empty());
    }
}
