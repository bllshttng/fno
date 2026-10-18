//! The learned capacity state: one JSON file per machine under
//! `<agents home>/capacity/`, written by the machine_watch tick, read by the
//! spawn gate, the probe, the lead beat, and the mux meter. The user's
//! `agents.max_live` stays the ceiling; the loop only LOWERS an effective cap
//! (AIMD: a sustained run-queue breach cuts 25%, sustained calm decays +1 per
//! window) and decays it back. A missing or unreadable state file lowers
//! nothing: every reader falls back to the ceiling and says so.

use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::machine_sample::MachineSample;
use crate::paths::AgentsHome;

/// Run queue per core above which a breach counts (the runaway band).
pub const DEFAULT_CUT_LOAD_PER_CORE: f64 = 4.0;
/// Sustained breach before a cut fires (the runaway hold).
pub const DEFAULT_CUT_HOLD_SECONDS: u64 = 600;
/// Each cut multiplies the effective cap by `1 - fraction`.
pub const DEFAULT_CUT_FRACTION: f64 = 0.25;
/// Run queue per core below which the box counts as calm.
pub const DEFAULT_RAISE_LOAD_PER_CORE: f64 = 2.0;
/// Sustained calm before one +1 decay step.
pub const DEFAULT_RAISE_HOLD_SECONDS: u64 = 900;
/// The learned cap never drops below this unless the user sets a higher floor.
pub const DEFAULT_FLOOR: u32 = 1;
/// The reason an older first run stamped on a cap it guessed from the machine
/// budget. The reader treats it as no measurement.
const SEED_REASON: &str = "seeded from machine budget";
/// What every soft-cap refusal names to hold the learned cap up. The spawn
/// door's `--floor` writes the key.
/// What every soft refusal says about --force.
const FORCE_TAIL: &str =
    "For one spawn the user ordered: --force. It passes every soft limit and never the RAM floor.";
pub const MAX_LIVE_REMEDY: &str = "raise agents.max_live in ~/.fno/config.toml";
pub const FLOOR_REMEDY: &str = "fno agents spawn --floor N";

/// `[agents.capacity]` thresholds, defaults mirroring the runaway arm's band.
#[derive(Debug, Clone, Copy)]
pub struct CapacityThresholds {
    pub cut_per_core: f64,
    pub cut_hold_seconds: u64,
    pub cut_fraction: f64,
    pub raise_per_core: f64,
    pub raise_hold_seconds: u64,
    /// `[agents.capacity] floor`: the user's lower bound on the learned cap.
    pub floor: u32,
}

impl Default for CapacityThresholds {
    fn default() -> Self {
        Self {
            cut_per_core: DEFAULT_CUT_LOAD_PER_CORE,
            cut_hold_seconds: DEFAULT_CUT_HOLD_SECONDS,
            cut_fraction: DEFAULT_CUT_FRACTION,
            raise_per_core: DEFAULT_RAISE_LOAD_PER_CORE,
            raise_hold_seconds: DEFAULT_RAISE_HOLD_SECONDS,
            floor: DEFAULT_FLOOR,
        }
    }
}

/// The `[agents.capacity]` overrides, read through the same candidate chain
/// every agents key reads. A malformed or absent key keeps its default.
pub fn thresholds(cwd: &std::path::Path) -> CapacityThresholds {
    let read = |key: &str| crate::agents_config::config_lookup(cwd, &["agents", "capacity", key]);
    let float = |key: &str, default: f64| {
        read(key)
            .and_then(|v| v.as_float())
            .filter(|v| *v > 0.0)
            .unwrap_or(default)
    };
    let uint = |key: &str, default: u64| {
        read(key)
            .and_then(|v| v.as_integer())
            .map(|v| v as u64)
            .filter(|v| *v > 0)
            .unwrap_or(default)
    };
    CapacityThresholds {
        cut_per_core: float("cut_load_per_core", DEFAULT_CUT_LOAD_PER_CORE),
        cut_hold_seconds: uint("cut_hold_seconds", DEFAULT_CUT_HOLD_SECONDS),
        cut_fraction: read("cut_fraction")
            .and_then(|v| v.as_float())
            .filter(|v| *v > 0.0 && *v < 1.0)
            .unwrap_or(DEFAULT_CUT_FRACTION),
        raise_per_core: float("raise_load_per_core", DEFAULT_RAISE_LOAD_PER_CORE),
        raise_hold_seconds: uint("raise_hold_seconds", DEFAULT_RAISE_HOLD_SECONDS),
        floor: read("floor")
            .and_then(|v| v.as_integer())
            .filter(|v| *v > 0)
            .map_or(DEFAULT_FLOOR, |v| v.min(u32::MAX as i64) as u32),
    }
}

/// The one memory figure: the vm_stat compressor-occupied share of physical
/// RAM, written by the tick and printed by the meter and the org overlay so
/// the two can never disagree. macmon keeps CPU and watts only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryFigure {
    pub used_fraction: f64,
    pub total_gb: f64,
    pub compressor_occupied_gb: f64,
    pub compressor_stored_gb: f64,
}

/// One provider quota window folded from the ledger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowRow {
    pub window: String,
    pub used_tokens: u64,
    /// When the window refills: a 429-derived lock when one is armed, else
    /// the window boundary.
    pub reset_epoch: i64,
}

/// One `[[accounts.records]]` row folded into the state. `show = false`
/// keeps the row out of every gauge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountRow {
    pub account: String,
    pub provider: String,
    pub billing: String,
    pub show: bool,
    pub windows: Vec<WindowRow>,
    /// Subscription only: the account's own token ceiling, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_tokens: Option<u64>,
    /// Metered only: calendar-month (UTC) spend in dollars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_usd: Option<f64>,
}

/// The persisted state. `over_since`/`calm_since` carry the AIMD streaks
/// across daemon restarts; `slots_learned` stays absent until the resources
/// phase teaches the learned heavy-job slot count.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapacityState {
    pub ceiling: u32,
    pub effective: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots_learned: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over_since: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calm_since: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryFigure>,
    #[serde(default)]
    pub accounts: Vec<AccountRow>,
    /// The tick's own live registry count, so the gauge never walks the
    /// registry with a second reader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers_live: Option<u64>,
    pub updated_epoch: i64,
}

/// The cap answer the gate reads. `known = false` means no usable state: the
/// cap IS the ceiling and every rendering says so.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Effective {
    pub cap: usize,
    pub ceiling: usize,
    pub known: bool,
    pub reason: Option<String>,
    pub since: Option<i64>,
    /// Set when the user's floor holds the cap above what the loop learned.
    pub held: Option<FloorHold>,
    /// Set when this pass burst past `agents.max_live` on machine room.
    pub burst: bool,
}

/// The floor as configured and the learned cap it overrode.
#[derive(Debug, Clone, PartialEq)]
pub struct FloorHold {
    pub floor: usize,
    pub learned: usize,
}

impl Effective {
    /// The sentence fragment every refusal and receipt shares, so a display
    /// can never quote a cap the gate did not read.
    pub fn clause(&self) -> String {
        if self.burst {
            return format!(
                "burst cap {} past agents.max_live {} (CPU and RAM have room)",
                self.cap, self.ceiling
            );
        }
        if let Some(held) = self.held.as_ref().filter(|h| h.learned >= self.cap) {
            return format!(
                "effective cap {} of {} (at agents.capacity.floor {})",
                self.cap, self.ceiling, held.floor
            );
        }
        if let Some(held) = &self.held {
            return format!(
                "effective cap {} of {} (held at agents.capacity.floor {}, learned {})",
                self.cap, self.ceiling, held.floor, held.learned
            );
        }
        match self.known {
            true => match (&self.reason, self.since) {
                (Some(reason), Some(since)) => format!(
                    "effective cap {} of {} ({} since {})",
                    self.cap,
                    self.ceiling,
                    reason,
                    render_clock(since)
                ),
                _ => format!("effective cap {} of {}", self.cap, self.ceiling),
            },
            false => format!(
                "max_live {} (effective cap unknown, using ceiling)",
                self.ceiling
            ),
        }
    }

    /// The cap the loop learned, with the floor stripped off.
    fn learned_cap(&self) -> usize {
        self.held.as_ref().map_or(self.cap, |h| h.learned)
    }

    /// The line every soft-cap refusal prints so the reader knows the number
    /// is not theirs and how to lift it. `None` at the ceiling: no lift exists.
    pub fn override_hint(&self) -> Option<String> {
        (self.cap < self.ceiling).then(|| {
            format!(
                "spawn-gate: the learned cap is {}; the user's agents.max_live is {}. To hold \
                 the cap at N or above: {FLOOR_REMEDY}. {FORCE_TAIL}",
                self.learned_cap(),
                self.ceiling
            )
        })
    }

    /// The hint for a refusal on the slot axis: below the ceiling the floor
    /// hint, at it the step that raises `agents.max_live`.
    pub fn slot_hint(&self) -> String {
        self.override_hint().unwrap_or_else(|| {
            format!(
                "spawn-gate: the cap is the user's agents.max_live, {}. To raise it: \
                 {MAX_LIVE_REMEDY}. {FORCE_TAIL} Past it, one spawn a minute admits while the \
                 CPU hold and the RAM floor read room.",
                self.ceiling
            )
        })
    }

    pub fn eprint_slot_hint(&self) {
        eprintln!("{}", self.slot_hint());
    }

    /// Prints [`Self::override_hint`] on stderr when there is one.
    pub fn eprint_hint(&self) {
        if let Some(hint) = self.override_hint() {
            eprintln!("{hint}");
        }
    }

    /// Adds the keys a slot refusal receipt carries beside its `max_live`:
    /// the learned cap, the ceiling and, below the ceiling, the remedy.
    pub fn annotate(&self, receipt: &mut Value) {
        receipt["learned_cap"] = serde_json::json!(self.learned_cap());
        receipt["ceiling"] = serde_json::json!(self.ceiling);
        if self.cap < self.ceiling {
            receipt["remedy"] = serde_json::json!(FLOOR_REMEDY);
        } else if receipt["axis"] != "fleet_cpu_share" {
            receipt["remedy"] = serde_json::json!(MAX_LIVE_REMEDY);
        }
    }
}

/// The remedy a refused native launch line shows. A native line has no
/// `--force`, so a slot refusal (its receipt carries `ceiling`) also names the
/// spawn form that does.
pub fn native_remedy(remedy: &str, receipt: Option<&Value>) -> String {
    match receipt.is_some_and(|r| r.get("ceiling").is_some()) {
        true => format!(
            "{remedy}. For one launch the user ordered: fno agents spawn NAME PAYLOAD --force"
        ),
        false => remedy.to_string(),
    }
}

/// [`effective`] against the ambient home, and the ceiling when the process
/// has none: the gate reads this, so an undeclared-root test process takes
/// the ceiling fallback instead of panicking on the home resolution.
pub fn effective_from_env(ceiling: usize, floor: usize) -> Effective {
    match crate::paths::AgentsHome::from_env_opt() {
        Some(home) => effective(&home, ceiling, floor),
        None => Effective {
            cap: ceiling,
            ceiling,
            ..Default::default()
        },
    }
}

/// [`effective_from_env`] with the ceiling and the floor read from the config
/// at `cwd`: the one-line reader for callers that hold no ceiling already.
pub fn effective_for(cwd: &std::path::Path) -> Effective {
    effective_from_env(
        crate::agents_config::max_live(cwd) as usize,
        thresholds(cwd).floor as usize,
    )
}

/// The state file: per machine, keyed by the stable platform id when the box
/// has one (a hostname moves under a roaming laptop; the platform id does
/// not), hostname as the fallback.
pub fn state_path(home: &AgentsHome) -> PathBuf {
    let key = crate::claims::machine_id();
    let key = if key.is_empty() {
        crate::claims::hostname()
    } else {
        key
    };
    let key = if key.is_empty() {
        "unknown-host".to_string()
    } else {
        key
    };
    home.root().join("capacity").join(format!("{key}.json"))
}

/// The persisted state, `None` when missing or unreadable. A reader never
/// repairs a broken file: the next tick's write does.
pub fn read_state(home: &AgentsHome) -> Option<CapacityState> {
    let raw = std::fs::read_to_string(state_path(home)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The learned cap when the loop measured it below the ceiling. A seed guess,
/// or a cap that sat at an older and lower ceiling, is no measurement: it
/// follows the ceiling, so a raised `agents.max_live` binds at once.
fn measured_cut(state: &CapacityState) -> Option<u32> {
    (state.effective < state.ceiling && state.reason.as_deref() != Some(SEED_REASON))
        .then_some(state.effective)
}

/// The cap answer for this machine: the learned effective cap raised to the
/// user's floor and clamped under the given ceiling, or the ceiling itself
/// when no usable state exists. The reader applies the floor itself so a
/// floor set between two ticks binds at once.
pub fn effective(home: &AgentsHome, ceiling: usize, floor: usize) -> Effective {
    let ceiling = ceiling.max(1);
    match read_state(home) {
        Some(state) if state.effective >= 1 => {
            let learned = measured_cut(&state).map_or(ceiling, |cut| (cut as usize).min(ceiling));
            let floor_cap = floor.min(ceiling);
            let cap = learned.max(floor_cap);
            let known = cap < ceiling || state.reason.is_some();
            Effective {
                cap,
                ceiling,
                known,
                reason: state.reason,
                since: state.since,
                // A tick and a cut both write the floor into the state, so a
                // cap sitting on a user floor is held even when learned equals it.
                held: (floor > DEFAULT_FLOOR as usize && cap == floor_cap)
                    .then_some(FloorHold { floor, learned }),
                burst: false,
            }
        }
        _ => Effective {
            cap: ceiling,
            ceiling,
            ..Default::default()
        },
    }
}

/// Local wall clock for a `since` stamp, "21:40" style.
fn render_clock(epoch: i64) -> String {
    Local
        .timestamp_opt(epoch, 0)
        .single()
        .map(|t: DateTime<Local>| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// One AIMD step from a tick's sample. Seeds the state at the ceiling on
/// first run, folds the memory figure and the account windows, and
/// persists the result. `Ok(None)` when the sample carries no run queue (the
/// one signal the loop reads) and there is nothing to refresh.
pub fn feed(
    sample: &MachineSample,
    cwd: &std::path::Path,
    home: &AgentsHome,
    now_epoch: i64,
) -> io::Result<CapacityState> {
    let ceiling = crate::agents_config::max_live(cwd);
    let thresholds = thresholds(cwd);
    let mut state = read_state(home).unwrap_or_else(|| seed(ceiling, now_epoch));
    if measured_cut(&state).is_none() {
        state.effective = ceiling;
        state.reason = None;
        state.since = None;
    }
    state.ceiling = ceiling;
    if let Some(next) = step(&state, sample, &thresholds, now_epoch) {
        state = next;
    }
    // The file the mux meter reads agrees with the reader within one tick.
    state.effective = state.effective.max(thresholds.floor.min(ceiling));
    state.memory = memory_figure(sample);
    state.workers_live = sample.live_rows;
    state.accounts = fold_accounts(cwd, home, ledger_rows(cwd, home), now_epoch);
    state.updated_epoch = now_epoch;
    write_state(home, &state)?;
    Ok(state)
}

/// First run: the cap starts at the ceiling. Only a measured breach lowers it.
fn seed(ceiling: u32, now_epoch: i64) -> CapacityState {
    CapacityState {
        ceiling,
        effective: ceiling.max(1),
        reason: None,
        since: None,
        slots_learned: None,
        over_since: None,
        calm_since: None,
        memory: None,
        accounts: Vec::new(),
        workers_live: None,
        updated_epoch: now_epoch,
    }
}

/// The verdict step: breach held long enough cuts 25% (never under the
/// floor, and no cut at all once the cap sits on it), calm held
/// long enough adds 1 (never over the ceiling), the middle band resets both
/// streaks. The streak bookkeeping is ALWAYS updated in the returned state
/// (a pre-threshold sample arms the clock; discarding it would restart the
/// hold every tick and AIMD would never fire from natural ticks); the cap
/// itself only moves on a threshold event. `None` when the sample carries no
/// run queue, the one signal the loop reads.
fn step(
    state: &CapacityState,
    sample: &MachineSample,
    t: &CapacityThresholds,
    now_epoch: i64,
) -> Option<CapacityState> {
    let per_core = sample
        .runnable
        .zip(sample.cores)
        .filter(|(_, cores)| *cores > 0.0)
        .map(|(runnable, cores)| runnable as f64 / cores)?;
    let mut next = state.clone();
    if per_core > t.cut_per_core {
        let over = state.over_since.unwrap_or(now_epoch);
        next.over_since = Some(over);
        next.calm_since = None;
        if now_epoch - over >= t.cut_hold_seconds as i64 && state.effective > t.floor {
            let cut = ((state.effective as f64) * (1.0 - t.cut_fraction)).floor() as u32;
            next.effective = cut.max(t.floor);
            next.reason = Some("CPU-bound".into());
            next.since = Some(now_epoch);
            next.over_since = None;
        }
        return Some(next);
    }
    if per_core < t.raise_per_core {
        let calm = state.calm_since.unwrap_or(now_epoch);
        next.calm_since = Some(calm);
        next.over_since = None;
        if now_epoch - calm >= t.raise_hold_seconds as i64 && state.effective < state.ceiling {
            next.effective = state.effective + 1;
            next.reason = None;
            next.since = None;
            next.calm_since = None;
        }
        return Some(next);
    }
    next.over_since = None;
    next.calm_since = None;
    Some(next)
}

pub(crate) fn memory_figure(sample: &MachineSample) -> Option<MemoryFigure> {
    let total = sample.total_mem_gb?;
    let occupied = sample.compressor_gb?;
    if total <= 0.0 {
        return None;
    }
    Some(MemoryFigure {
        used_fraction: (occupied / total).clamp(0.0, 1.0),
        total_gb: total,
        compressor_occupied_gb: occupied,
        compressor_stored_gb: sample.compressed_gb.unwrap_or(0.0),
    })
}

/// The global ledger (`~/.fno/ledger.json` by default,
/// `config.paths.ledger_json` when the install moves it), `[]` when missing
/// or unreadable: the fold answers from whatever the ledger holds and never
/// invents tokens.
fn ledger_rows(cwd: &std::path::Path, home: &AgentsHome) -> Vec<Value> {
    let configured = crate::agents_config::config_lookup(cwd, &["paths", "ledger_json"])
        .and_then(|v| v.as_str().map(PathBuf::from));
    let path = configured.unwrap_or_else(|| {
        home.root()
            .parent()
            .map(|dir| dir.join("ledger.json"))
            .unwrap_or_else(|| PathBuf::from("ledger.json"))
    });
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v.get("entries").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

/// One `[[accounts.records]]` row the fold can use. The billing fields are
/// optional config: a record that names none folds nothing. `route_model` is
/// the model side of the route (`glm-5.3` from `zai/glm-5.3`), the key the
/// ledger's own model ids match against.
#[derive(Debug, Clone)]
pub(crate) struct AccountRecord {
    pub id: String,
    pub provider: String,
    pub route_model: String,
    pub billing: String,
    pub show: bool,
    pub windows: Vec<String>,
    pub limit_tokens: Option<u64>,
    pub reset_timezone: Option<String>,
}

/// `[[accounts.records]]` through the same candidate chain
/// `record_reset_timezones` reads, so one config file is one truth. The
/// chain is walked highest priority first and the first record with an id
/// wins, the same first-hit-wins rule every config key follows.
pub(crate) fn account_records(cwd: &std::path::Path) -> Vec<AccountRecord> {
    let paths = crate::agents_config::config_candidates(cwd);
    let mut out: Vec<AccountRecord> = Vec::new();
    for path in &paths {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(table) = raw.parse::<toml::Table>() else {
            continue;
        };
        let Some(records) = table
            .get("accounts")
            .and_then(|v| v.get("records"))
            .and_then(toml::Value::as_array)
        else {
            continue;
        };
        for rec in records {
            let Some(id) = rec.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if out.iter().any(|known| known.id == id) {
                continue;
            }
            // A record may name its route (provider/model) or skip it: an
            // OAuth entry identifies its account without the model axis. A
            // route-less record folds no tokens (no provider to match), but
            // it stays in the state so the config is never silently dropped.
            let (provider, route_model) = match rec.get("route").and_then(|v| v.as_str()) {
                Some(route) => match route.split_once('/') {
                    Some((p, m)) if !p.is_empty() => (p, m),
                    _ => continue,
                },
                None => ("", ""),
            };
            let billing = rec
                .get("billing")
                .and_then(|v| v.as_str())
                .unwrap_or("subscription");
            if billing != "subscription" && billing != "metered" {
                continue;
            }
            let windows: Vec<String> = rec
                .get("windows")
                .and_then(toml::Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|v| v.as_str())
                        .filter(|w| *w == "5h" || *w == "weekly")
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            out.push(AccountRecord {
                id: id.to_string(),
                provider: provider.to_string(),
                route_model: route_model.to_string(),
                billing: billing.to_string(),
                show: rec
                    .get("show")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(true),
                windows: match billing {
                    "subscription" => windows,
                    _ => Vec::new(),
                },
                limit_tokens: rec
                    .get("limit_tokens")
                    .and_then(toml::Value::as_integer)
                    .map(|v| v as u64),
                reset_timezone: rec
                    .get("reset_timezone")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            });
        }
    }
    out
}

/// Tokens per provider per window, folded from the ledger's own totals. The
/// ledger carries no account axis, so several records sharing one provider
/// each show the provider fold, never an invented split.
pub(crate) fn fold_windows(
    records: &[AccountRecord],
    ledger: &[Value],
    snapshot: Option<&crate::provider_cap::CapSnapshot>,
    now_epoch: i64,
) -> Vec<AccountRow> {
    records
        .iter()
        .map(|rec| {
            let rows = match rec.billing.as_str() {
                "subscription" => rec
                    .windows
                    .iter()
                    .map(|window| {
                        let zone = rec.reset_timezone.as_deref();
                        let since = window_end(window, zone, now_epoch) - window_len(window);
                        WindowRow {
                            window: window.clone(),
                            used_tokens: ledger_tokens(ledger, rec, since),
                            // A 429 lock beats a computed boundary: the
                            // provider already said when the lane refills.
                            reset_epoch: snapshot_reset(snapshot, &rec.provider)
                                .unwrap_or_else(|| window_end(window, zone, now_epoch)),
                        }
                    })
                    .collect(),
                _ => Vec::new(),
            };
            AccountRow {
                account: rec.id.clone(),
                provider: rec.provider.clone(),
                billing: rec.billing.clone(),
                show: rec.show,
                windows: rows,
                limit_tokens: rec.limit_tokens,
                spend_usd: match rec.billing.as_str() {
                    "metered" => Some(ledger_spend(ledger, rec, now_epoch)),
                    _ => None,
                },
            }
        })
        .collect()
}

/// A 429-derived lock beats a computed boundary: the provider already said
/// when the lane refills.
fn snapshot_reset(
    snapshot: Option<&crate::provider_cap::CapSnapshot>,
    provider: &str,
) -> Option<i64> {
    snapshot.and_then(|snap| {
        snap.lanes.iter().find_map(|lane| {
            (lane.provider == provider)
                .then_some(lane.reset_epoch)
                .flatten()
        })
    })
}

/// Window end: the 5h block boundary, or next Monday 00:00 in the record's
/// reset timezone (UTC when the record names none or an unknown zone).
fn window_end(window: &str, zone: Option<&str>, now_epoch: i64) -> i64 {
    match window {
        "5h" => {
            let period = 5 * 3600;
            let start = now_epoch.div_euclid(period) * period;
            start + period
        }
        "weekly" => {
            use chrono::Datelike;
            let zone = zone
                .and_then(|name| name.parse::<chrono_tz::Tz>().ok())
                .unwrap_or(chrono_tz::UTC);
            let now = Utc
                .timestamp_opt(now_epoch, 0)
                .single()
                .unwrap_or_else(Utc::now)
                .with_timezone(&zone);
            let add_days = 7 - now.weekday().num_days_from_monday();
            now.date_naive()
                .and_hms_opt(0, 0, 0)
                .and_then(|midnight| midnight.and_local_timezone(zone).single())
                .map(|midnight| midnight + chrono::Duration::days(add_days as i64))
                .map(|reset| reset.timestamp())
                .unwrap_or_else(|| now_epoch + 7 * 3600)
        }
        _ => now_epoch + 3600,
    }
}

fn window_len(window: &str) -> i64 {
    match window {
        "5h" => 5 * 3600,
        "weekly" => 7 * 24 * 3600,
        _ => 3600,
    }
}

/// Ledger tokens whose provider matches, summed per window row. The ledger
/// has no account axis; the model's route prefix is the provider.
/// The ledger row against the record. Rows that name their provider
/// (`provider` / `provider_id`) match on it exactly - two accounts sharing a
/// model prefix then never attribute each other's rows. Rows without a
/// provider field fall to the model id: the BARE id (`glm-5.3-flash[1m]`,
/// `claude-opus-5-5`) against the route's model side, by prefix (`glm-5.3`
/// matches `glm-5.3-flash[1m]`); a model carrying the provider prefix
/// (`zai/glm-5.3-flash`) strips it first.
fn row_matches_record(row: &Value, rec: &AccountRecord) -> bool {
    for key in ["provider", "provider_id"] {
        if let Some(provider) = row.get(key).and_then(Value::as_str) {
            if !provider.is_empty() {
                return provider == rec.provider;
            }
        }
    }
    let Some(model) = row.get("model").and_then(Value::as_str) else {
        return false;
    };
    let bare = model.split_once('/').map(|(_, m)| m).unwrap_or(model);
    !rec.route_model.is_empty() && bare.starts_with(&rec.route_model)
}

fn ledger_tokens(ledger: &[Value], rec: &AccountRecord, since_epoch: i64) -> u64 {
    ledger
        .iter()
        .filter(|row| row_matches_record(row, rec))
        .filter(|row| row_started_after(row, since_epoch))
        .filter_map(|row| row.get("tokens_total").and_then(Value::as_u64))
        .sum()
}

fn ledger_spend(ledger: &[Value], rec: &AccountRecord, now_epoch: i64) -> f64 {
    ledger
        .iter()
        .filter(|row| row_matches_record(row, rec))
        .filter(|row| row_started_after(row, month_start(now_epoch)))
        .filter_map(|row| row.get("cost_usd").and_then(Value::as_f64))
        .sum()
}

/// Rows whose work started inside the current calendar month (UTC): the
/// widest window any gauge shows.
fn month_start(now_epoch: i64) -> i64 {
    use chrono::Datelike;
    let now: DateTime<Utc> = Utc.timestamp_opt(now_epoch, 0).single().unwrap_or_default();
    now.date_naive()
        .with_day(1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0)
}

fn row_started_after(row: &Value, since_epoch: i64) -> bool {
    match row
        .get("started")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
    {
        Some(ts) => ts.timestamp() >= since_epoch,
        None => true,
    }
}

fn fold_accounts(
    cwd: &std::path::Path,
    home: &AgentsHome,
    ledger: Vec<Value>,
    now_epoch: i64,
) -> Vec<AccountRow> {
    let records = account_records(cwd);
    if records.is_empty() {
        return Vec::new();
    }
    let snapshot = crate::provider_cap::read_persisted_snapshot(home);
    fold_windows(&records, &ledger, snapshot.as_ref(), now_epoch)
}

/// The write the tick owns: tmp file, rename, the same pattern the provider
/// cap snapshot persists with.
pub(crate) fn write_state(home: &AgentsHome, state: &CapacityState) -> io::Result<()> {
    let path = state_path(home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(state)?)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_sample::MachineSample;

    fn sample(runnable: Option<u64>, cores: f64) -> MachineSample {
        let mut sample = MachineSample::default();
        sample.runnable = runnable;
        sample.cores = Some(cores);
        sample
    }

    fn tmp_home(tag: &str) -> AgentsHome {
        let dir = std::env::temp_dir().join(format!("fno-capacity-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        AgentsHome::at(dir)
    }

    /// The ceiling comes from CONFIG, not the test: a runner without an
    /// agents.max_live in scope takes DEFAULT_MAX_LIVE, and every assert in
    /// the AIMD test would read against the wrong ceiling. Pin 23 through
    /// FNO_CONFIG under the shared env lock for the test's duration.
    fn pin_ceiling_23() -> (std::sync::MutexGuard<'static, ()>, std::path::PathBuf) {
        let lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-capacity-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "[agents]\nmax_live = 23\n").unwrap();
        std::env::set_var("FNO_CONFIG", dir.join("config.toml"));
        (lock, dir)
    }

    fn drop_ceiling_pin(guard: std::sync::MutexGuard<'static, ()>, dir: &std::path::Path) {
        std::env::remove_var("FNO_CONFIG");
        let _ = std::fs::remove_dir_all(dir);
        drop(guard);
    }

    #[test]
    fn sustained_breach_cuts_and_the_clause_names_both_numbers() {
        let (env_lock, cfg_dir) = pin_ceiling_23();
        let cwd = std::env::temp_dir();
        let now = 1_800_000_000;
        // Natural ticks: the FIRST overload sample arms the clock without
        // cutting, the state file carries it, and the tick past the hold
        // window fires the cut - no pre-seeded timestamps.
        let natural = tmp_home("natural");
        let hot = sample(Some(60), 12.0); // 5 per core, over the 4.0 band
        let armed = feed(&hot, &cwd, &natural, now).unwrap();
        assert_eq!(armed.effective, 23, "the arming tick does not cut");
        assert_eq!(armed.over_since, Some(now), "the breach clock is persisted");
        let cut = feed(&hot, &cwd, &natural, now + 600).unwrap();
        assert_eq!(cut.effective, 17, "a 25% cut of 23 floors to 17");
        assert_eq!(cut.reason.as_deref(), Some("CPU-bound"));
        let _ = std::fs::remove_dir_all(natural.root());

        let home = tmp_home("cut");
        // Pre-learned cap at the ceiling, breach held the full hold window.
        let state = CapacityState {
            ceiling: 23,
            effective: 23,
            reason: None,
            since: None,
            slots_learned: None,
            over_since: Some(now - 600),
            calm_since: None,
            memory: None,
            accounts: Vec::new(),
            workers_live: None,
            updated_epoch: now - 300,
        };
        write_state(&home, &state).unwrap();
        let cwd = std::env::temp_dir();
        let hot = sample(Some(60), 12.0); // 5 per core, over the 4.0 band
        let fed = feed(&hot, &cwd, &home, now).unwrap();
        assert_eq!(fed.effective, 17, "a 25% cut of 23 floors to 17");
        assert_eq!(fed.reason.as_deref(), Some("CPU-bound"));
        let learned = effective(&home, 23, 1);
        assert_eq!(learned.cap, 17);
        assert!(
            learned.clause().contains("effective cap 17 of 23"),
            "{}",
            learned.clause()
        );
        assert!(learned.clause().contains("CPU-bound since"));

        // A machine with no state at all takes the ceiling and says so.
        let bare = tmp_home("fallback");
        let learned = effective(&bare, 23, 1);
        assert_eq!(learned.cap, 23);
        assert!(!learned.known);
        assert_eq!(
            learned.clause(),
            "max_live 23 (effective cap unknown, using ceiling)"
        );
        let _ = std::fs::remove_dir_all(bare.root());
        let _ = std::fs::remove_dir_all(home.root());

        // The decay leg: calm held the full window adds one, a raised cap
        // carries no cut reason, and the ceiling stops the walk.
        let home = tmp_home("decay");
        let now = 1_800_000_000;
        let state = CapacityState {
            ceiling: 23,
            effective: 17,
            reason: Some("CPU-bound".into()),
            since: Some(now - 3600),
            slots_learned: None,
            over_since: None,
            calm_since: Some(now - 900),
            memory: None,
            accounts: Vec::new(),
            workers_live: None,
            updated_epoch: now - 300,
        };
        write_state(&home, &state).unwrap();
        let cwd = std::env::temp_dir();
        let fed = feed(&sample(Some(6), 12.0), &cwd, &home, now).unwrap();
        assert_eq!(fed.effective, 18);
        assert!(fed.reason.is_none(), "a raised cap carries no cut reason");
        // At the ceiling a further calm tick stops, never exceeds.
        let mut at_ceiling = fed.clone();
        at_ceiling.effective = 23;
        at_ceiling.calm_since = Some(now - 900);
        write_state(&home, &at_ceiling).unwrap();
        let fed = feed(&sample(Some(6), 12.0), &cwd, &home, now + 300).unwrap();
        assert_eq!(fed.effective, 23);
        let _ = std::fs::remove_dir_all(home.root());
        drop_ceiling_pin(env_lock, &cfg_dir);
    }

    #[test]
    fn the_floor_binds_the_reader_and_the_cut() {
        let now = 1_800_000_000;
        let learned_five = CapacityState {
            ceiling: 24,
            effective: 5,
            reason: Some("CPU-bound".into()),
            since: Some(now - 60),
            slots_learned: None,
            over_since: None,
            calm_since: None,
            memory: None,
            accounts: Vec::new(),
            workers_live: None,
            updated_epoch: now - 300,
        };
        let home = tmp_home("floor");
        write_state(&home, &learned_five).unwrap();
        // The reader raises the cap before any tick and says why.
        let held = effective(&home, 24, 8);
        assert_eq!((held.cap, held.known), (8, true));
        assert_eq!(
            held.clause(),
            "effective cap 8 of 24 (held at agents.capacity.floor 8, learned 5)"
        );
        // A floor over the ceiling clamps to it; one under the learned cap
        // changes nothing.
        assert_eq!(effective(&home, 24, 40).cap, 24);
        let plain = effective(&home, 24, 1);
        assert_eq!(plain.cap, 5);
        assert!(plain.held.is_none());
        // A native line has no --force, so its slot refusal names the spawn form.
        let mut receipt = serde_json::json!({"reason": "cap"});
        plain.annotate(&mut receipt);
        let native = native_remedy(FLOOR_REMEDY, Some(&receipt));
        assert!(
            native.ends_with("fno agents spawn NAME PAYLOAD --force"),
            "{native}"
        );
        assert_eq!(native_remedy(FLOOR_REMEDY, None), FLOOR_REMEDY);
        let _ = std::fs::remove_dir_all(home.root());

        // A cut stops at the floor and never fires once the cap sits on it.
        let t = CapacityThresholds {
            floor: 8,
            ..Default::default()
        };
        let hot = sample(Some(60), 12.0);
        let mut ten = learned_five.clone();
        ten.effective = 10;
        ten.over_since = Some(now - 600);
        let cut = step(&ten, &hot, &t, now).unwrap();
        assert_eq!(cut.effective, 8, "a 25% cut of 10 is 7; the floor holds 8");
        let mut on_floor = ten.clone();
        on_floor.effective = 8;
        assert_eq!(step(&on_floor, &hot, &t, now).unwrap().effective, 8);
        // With no floor key the cut still clamps at 1.
        let mut two = ten.clone();
        two.effective = 2;
        let bare = CapacityThresholds::default();
        assert_eq!(step(&two, &hot, &bare, now).unwrap().effective, 1);

        // After the cut is persisted the state's learned cap equals the floor.
        // The reader still names the floor, so a tick never hides it.
        let home = tmp_home("floor-tick");
        write_state(&home, &cut).unwrap();
        let after = effective(&home, 24, 8);
        assert_eq!(after.cap, 8);
        assert_eq!(
            after.clause(),
            "effective cap 8 of 24 (at agents.capacity.floor 8)"
        );
        let _ = std::fs::remove_dir_all(home.root());

        // At the ceiling a refusal still names a step: agents.max_live and --force.
        let at_ceiling = Effective {
            cap: 24,
            ceiling: 24,
            ..Default::default()
        };
        let hint = at_ceiling.slot_hint();
        assert!(
            hint.contains("agents.max_live") && hint.contains("--force"),
            "{hint}"
        );
        let mut receipt = serde_json::json!({"axis": "max_live"});
        at_ceiling.annotate(&mut receipt);
        assert_eq!(receipt["remedy"], MAX_LIVE_REMEDY);

        // max_live 32 with no floor reads 32: a first run seeds at the
        // ceiling, and neither an old seed guess nor a cap that sat at an
        // older ceiling of 24 holds the cap at 24.
        assert_eq!(seed(32, now).effective, 32);
        let home = tmp_home("raised-ceiling");
        let mut at_old = learned_five.clone();
        (at_old.effective, at_old.reason) = (24, None);
        write_state(&home, &at_old).unwrap();
        assert_eq!(effective(&home, 32, 1).cap, 32);
        (at_old.ceiling, at_old.reason) = (32, Some(SEED_REASON.into()));
        write_state(&home, &at_old).unwrap();
        assert_eq!(effective(&home, 32, 1).cap, 32);
        let _ = std::fs::remove_dir_all(home.root());
    }

    #[test]
    fn subscription_record_folds_one_row_per_window() {
        let now = 1_772_064_000; // a Monday 00:00 UTC
                                 // The ledger's real shape: BARE model ids, no provider prefix.
        let ledger = vec![
            serde_json::json!({
                "model": "glm-5.3-flash[1m]",
                "tokens_total": 171_079_597u64,
                "cost_usd": 5.42,
                "started": "2026-02-26T00:10:00+00:00",
            }),
            serde_json::json!({
                "model": "zai/glm-5.3-air",
                "tokens_total": 1_000u64,
                "cost_usd": 0.01,
                "started": "2026-02-26T10:00:00+00:00",
            }),
        ];
        let records = vec![AccountRecord {
            id: "zai-sub".into(),
            provider: "zai".into(),
            route_model: "glm-5.3".into(),
            billing: "subscription".into(),
            show: true,
            windows: vec!["5h".into(), "weekly".into()],
            limit_tokens: None,
            reset_timezone: Some("Asia/Singapore".into()),
        }];
        let rows = fold_windows(&records, &ledger, None, now);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].windows.len(), 2, "one row per window");
        assert_eq!(rows[0].windows[0].window, "5h");
        assert_eq!(rows[0].windows[0].used_tokens, 171_080_597);
        assert!(
            rows[0].windows[0].reset_epoch > now,
            "a 5h window resets in the future"
        );
        assert_eq!(rows[0].windows[1].window, "weekly");
        assert!(rows[0].windows[1].reset_epoch > now);

        // A hidden metered record stays in the state, flagged out of every
        // gauge, with spend and no window.
        let hidden = AccountRecord {
            id: "zai-api".into(),
            provider: "zai".into(),
            route_model: "glm-5.3".into(),
            billing: "metered".into(),
            show: false,
            windows: Vec::new(),
            limit_tokens: None,
            reset_timezone: None,
        };
        let rows = fold_windows(&[hidden], &ledger, None, now);
        assert!(
            rows[0].windows.is_empty(),
            "metered accounts show no window"
        );
        assert_eq!(rows[0].billing, "metered");
        assert!(!rows[0].show);
        assert!(rows[0].spend_usd.is_some());
    }
}
