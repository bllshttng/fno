//! The fleet machine-footprint reading: what CPU and process cost the fleet
//! puts on this machine right now, and whether a spawn is admissible under
//! it.
//!
//! The port of `cli/src/fno/footprint.py` (pure bucketing over one process
//! table) and `cli/src/fno/doctor_footprint.py` (attribution, admission,
//! payload), whose Python legs this file's landing deletes. One engine
//! serves three readers that must never disagree: the spawn gate's CPU axis
//! (in process, the `fno-footprint-cause` probe subprocess is gone), the
//! `census --cause` payload the gate parses, and `fno doctor
//! footprint`/`lanes` through the census op's `--footprint`/`--lanes` arms.
//!
//! Attribution fails closed in exactly the deleted leg's directions: a live
//! row no route can resolve is a NAMED gap that widens the fleet share to an
//! interval (the gate reads the upper bound, never headroom), and an
//! unreadable instrument refuses with `cpu_instrument`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use crate::census::ProcRow;
use crate::paths::AgentsHome;

/// The sustained-CPU floor: a one-core box still gets a meaningful bar.
pub(crate) const SUSTAINED_CPU_FLOOR_CORES: f64 = 0.25;
/// The sustained threshold is this fraction of measured capacity.
pub(crate) const SUSTAINED_CPU_CAPACITY_FRACTION: f64 = 0.1;
/// The roster explains the daemon itself beside the live rows.
pub(crate) const DAEMON_ALLOWANCE: u64 = 1;
/// Exit codes. Capacity keeps 3 (readers depend on it); the leak alarm owns
/// 5 so `echo $?` answers WHICH alarm fired.
pub(crate) const EXIT_CAPACITY_OVER: i32 = 3;
pub(crate) const EXIT_LEAK: i32 = 5;
pub(crate) const EXIT_UNAVAILABLE: i32 = 4;

/// argv[0] basenames that are a test runner on their own.
const TEST_RUNNER_NAMES: &[&str] = &["pytest", "py.test"];
/// fno binaries whose own process is fleet-attributable.
const FNO_BINARIES: &[&str] = &[
    "fno",
    "fno-py",
    "fno-agents",
    "fno-agents-daemon",
    "fno-agents-worker",
];
/// argv[1] tokens the Claude Code background daemon gives its pre-warm pool.
const CLAUDE_SPARE_POOL_ARGS: &[&str] = &["bg-spare", "bg-pty-host", "--bg-spare", "--bg-pty-host"];

/// Registry statuses that can hold a live process (`spawn_gate.LIVE_STATUSES`).
const LIVE_STATUSES: &[&str] = &["spawning", "ready", "idle", "busy", "live", "restarting"];

// ---------------------------------------------------------------------------
// Capacity and threshold
// ---------------------------------------------------------------------------

/// The CPU denominator: what `std::thread::available_parallelism` answers
/// (affinity- and quota-aware on Linux, host count on macOS). The Rust gate
/// already decided this is the right denominator; the deleted Python leg's
/// min() of the same candidates converges to it.
pub fn cpu_capacity_cores() -> u64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u64)
        .unwrap_or(1)
        .max(1)
}

/// `agents.footprint_sustained_cpu_cores`, when set above zero.
fn sustained_override(cwd: &Path) -> Option<f64> {
    crate::agents_config::resolve_agents_value(cwd, "footprint_sustained_cpu_cores")
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
}

/// The sustained-CPU bar: the override when pinned, else a fraction of
/// measured capacity with a floor.
pub fn sustained_cpu_threshold(capacity_cores: f64, cwd: &Path) -> f64 {
    if let Some(over) = sustained_override(cwd) {
        return over;
    }
    SUSTAINED_CPU_FLOOR_CORES.max(SUSTAINED_CPU_CAPACITY_FRACTION * capacity_cores)
}

// ---------------------------------------------------------------------------
// Row classification (footprint.py, decision for decision)
// ---------------------------------------------------------------------------

fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// True when a process's OWN program is a test runner. argv[0] plus the
/// first non-flag arguments, never a substring scan: a leaked keeper whose
/// socket path sits under `pytest-of-<user>` is not a test.
pub(crate) fn is_test_runner(command: &str) -> bool {
    let argv: Vec<&str> = command.split_whitespace().collect();
    let Some(program) = argv.first() else {
        return false;
    };
    let program = basename(program);
    let rest: Vec<&str> = argv.into_iter().skip(1).collect();
    if TEST_RUNNER_NAMES.contains(&program) {
        return true;
    }
    if program.starts_with("python") && rest.len() >= 2 && rest[0] == "-m" && rest[1] == "pytest" {
        return true;
    }
    let positional: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    if program == "cargo" {
        return matches!(positional.first(), Some(&"test") | Some(&"nextest"));
    }
    if program == "fno" || program == "fno-py" {
        return positional.first() == Some(&"test")
            || (positional.len() >= 2 && positional[0] == "doctor" && positional[1] == "test");
    }
    false
}

/// True for a Claude Code background-daemon pre-warm process: matched on
/// argv[0] plus argv[1], never the whole command line.
pub(crate) fn is_claude_spare_pool(command: &str) -> bool {
    let argv: Vec<&str> = command.split_whitespace().collect();
    if argv.len() < 2 {
        return false;
    }
    basename(argv[0]) == "claude" && CLAUDE_SPARE_POOL_ARGS.contains(&argv[1])
}

/// True when a pool-shaped argv runs a claimed session: Claude Code removes
/// the claim socket (the token after the last `--bg-spare`) when a session
/// takes the spare.
pub(crate) fn spare_claimed(command: &str) -> bool {
    let argv: Vec<&str> = command.split_whitespace().collect();
    let marks: Vec<usize> = argv
        .iter()
        .enumerate()
        .filter(|(_, t)| **t == "bg-spare" || **t == "--bg-spare")
        .map(|(i, _)| i)
        .collect();
    let Some(last) = marks.last() else {
        return false;
    };
    let Some(sock) = argv.get(last + 1) else {
        return false;
    };
    sock.ends_with(".claim.sock") && !Path::new(sock).exists()
}

fn attributed_command(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let Some(first) = tokens.first() else {
        return false;
    };
    let name = basename(first);
    if FNO_BINARIES.contains(&name) {
        return true;
    }
    name.starts_with("python") && tokens[1..].iter().any(|t| *t == "fno-py")
}

/// The `worktrees/<name>` tail a cluster of commands shares - the one line
/// that explains a spike ("that tree's test suite is running").
fn worktree_of(command: &str) -> Option<String> {
    let abs = command.find("worktrees/")?;
    let bytes = command.as_bytes();
    let mut start = abs;
    while start > 0 && is_name_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = abs + "worktrees/".len();
    while end < bytes.len() && is_name_byte(bytes[end]) {
        end += 1;
    }
    Some(command[start..end].to_string())
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'~' | b'.' | b'/' | b'-')
}

pub(crate) fn top_consumers(sustained: &[(f64, String)], n: usize) -> Vec<Value> {
    let mut programs: BTreeMap<String, (u64, f64, BTreeMap<String, u64>)> = BTreeMap::new();
    for (cpu_percent, command) in sustained {
        let name = command
            .split_whitespace()
            .next()
            .map(basename)
            .unwrap_or(command)
            .to_string();
        let entry = programs.entry(name.clone()).or_default();
        entry.0 += 1;
        entry.1 += cpu_percent;
        if let Some(tree) = worktree_of(command) {
            *entry.2.entry(tree).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(String, u64, f64, Option<(String, u64)>)> = programs
        .into_iter()
        .map(|(name, e)| {
            let top_tree = e.2.into_iter().max_by_key(|(_, c)| *c);
            (name, e.0, e.1, top_tree)
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.cmp(&a.1))
            .then(a.0.cmp(&b.0))
    });
    ranked
        .into_iter()
        .take(n)
        .map(|(name, procs, cpu, tree)| {
            let mut row = json!({
                "name": name,
                "procs": procs,
                "cpu_pct": (cpu * 10.0).round() / 10.0,
            });
            match tree {
                Some((tree_name, tree_procs)) => {
                    row["worktree"] = json!(tree_name);
                    row["worktree_procs"] = json!(tree_procs);
                }
                None => {
                    row["worktree"] = Value::Null;
                    row["worktree_procs"] = json!(0);
                }
            }
            row
        })
        .collect()
}

/// One masked unparsed-row sample; kept for payload shape parity.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct UnparsedSample {
    pub row: u64,
    pub pid: Option<u64>,
    pub reason: String,
    pub masked: String,
}

/// One snapshot split into sustained and startup-cost buckets.
#[derive(Debug, Clone, Default)]
pub(crate) struct Footprint {
    pub sustained_cpu_cores: f64,
    pub descendant_cpu_cores: f64,
    pub fleet_cpu_cores: f64,
    pub descendant_process_count: u64,
    pub direct_process_count: u64,
    pub transient_call_count: u64,
    pub process_count: u64,
    pub measured_cpu_cores: f64,
    pub top: Vec<(f64, String)>,
    pub unparsed_lines: u64,
    pub unparsed_samples: Vec<UnparsedSample>,
    pub attribution_gap: Option<String>,
    pub test_process_count: u64,
    pub spare_pool_process_count: u64,
    pub spare_pool_cpu_cores: f64,
    pub machine_process_count: u64,
    pub runnable_count: u64,
    pub memory: Option<Value>,
}

/// The CPU axis's decision, computed once and read by both gates (LD1).
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct Admission {
    pub verdict: String,
    pub axis: String,
    pub reason: String,
    pub share_low: f64,
    pub share_high: f64,
    pub bound: String,
    pub fleet_cores: f64,
    pub machine_cores: f64,
    pub capacity_cores: f64,
    pub ceiling: f64,
    pub gap: Option<String>,
    pub top_holder: Option<String>,
}

/// The reading plus the sccache fields the table fetch carries.
#[derive(Debug, Clone, Default)]
pub(crate) struct CauseReading {
    pub footprint: Footprint,
    pub sccache_server_pid: Option<u32>,
    pub sccache_restarts_1h: Option<u64>,
}

/// One parent-chain walker for exclusion and attribution (the deleted leg's
/// chain_reaches, cache entries are chain-complete).
fn chain_reaches(
    start: u32,
    marks: &BTreeSet<u32>,
    by_pid: &BTreeMap<u32, &ProcRow>,
    cache: &mut BTreeMap<u32, bool>,
) -> bool {
    if let Some(cached) = cache.get(&start) {
        return *cached;
    }
    let mut path: Vec<u32> = Vec::new();
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    let mut current = Some(start);
    let mut result = false;
    while let Some(pid) = current {
        if pid == 0 || seen.contains(&pid) {
            break;
        }
        if let Some(cached) = cache.get(&pid) {
            result = *cached;
            break;
        }
        seen.insert(pid);
        path.push(pid);
        if marks.contains(&pid) {
            result = true;
            break;
        }
        current = by_pid.get(&pid).map(|row| row.ppid);
    }
    for pid in path {
        cache.insert(pid, result);
    }
    result
}

/// Bucket one process table into the footprint. `attributed_roots` name the
/// live worker roots the registry proved; `excluded` keeps the reader's own
/// process out; shared serves count toward the fleet but not the leak
/// threshold.
pub(crate) fn parse_footprint(
    table: &[ProcRow],
    excluded_root_pids: &BTreeSet<u32>,
    attributed_roots: &BTreeSet<u32>,
    threshold_excluded_roots: &BTreeSet<u32>,
    sustained_floor_seconds: u64,
) -> Footprint {
    let by_pid: BTreeMap<u32, &ProcRow> = table.iter().map(|r| (r.pid, r)).collect();
    let direct: BTreeMap<u32, bool> = table
        .iter()
        .map(|r| {
            (
                r.pid,
                attributed_roots.contains(&r.pid) || attributed_command(&r.command),
            )
        })
        .collect();
    let attributed_marks: BTreeSet<u32> = direct
        .iter()
        .filter(|(_, d)| **d)
        .map(|(pid, _)| *pid)
        .collect();
    let mut out = Footprint {
        machine_process_count: table.len() as u64,
        ..Default::default()
    };
    let mut sustained: Vec<(f64, String)> = Vec::new();
    let mut excluded_cache: BTreeMap<u32, bool> = BTreeMap::new();
    let mut attributed_cache: BTreeMap<u32, bool> = BTreeMap::new();
    let mut owned_cache: BTreeMap<u32, bool> = BTreeMap::new();
    for row in table {
        let excluded = chain_reaches(row.pid, excluded_root_pids, &by_pid, &mut excluded_cache);
        if !excluded {
            out.measured_cpu_cores += row.cpu_pct / 100.0;
            if is_test_runner(&row.command) {
                out.test_process_count += 1;
            }
            if is_claude_spare_pool(&row.command)
                && !spare_claimed(&row.command)
                && !chain_reaches(row.pid, attributed_roots, &by_pid, &mut owned_cache)
            {
                out.spare_pool_process_count += 1;
                out.spare_pool_cpu_cores += row.cpu_pct / 100.0;
            }
        }
        let attributed = if excluded {
            false
        } else {
            chain_reaches(row.pid, &attributed_marks, &by_pid, &mut attributed_cache)
        };
        if !attributed {
            continue;
        }
        out.process_count += 1;
        let direct_row = direct.get(&row.pid).copied().unwrap_or(false);
        if !direct_row {
            out.descendant_process_count += 1;
            out.descendant_cpu_cores += row.cpu_pct / 100.0;
            if row.cpu_pct != 0.0 {
                sustained.push((row.cpu_pct, row.command.clone()));
            }
        } else if attributed_roots.contains(&row.pid) {
            if !threshold_excluded_roots.contains(&row.pid) {
                out.direct_process_count += 1;
            }
            out.sustained_cpu_cores += row.cpu_pct / 100.0;
            sustained.push((row.cpu_pct, row.command.clone()));
        } else if row.elapsed_s < sustained_floor_seconds {
            out.direct_process_count += 1;
            out.transient_call_count += 1;
        } else {
            out.direct_process_count += 1;
            out.sustained_cpu_cores += row.cpu_pct / 100.0;
            sustained.push((row.cpu_pct, row.command.clone()));
        }
    }
    sustained.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    out.top = sustained;
    out.fleet_cpu_cores = out.sustained_cpu_cores + out.descendant_cpu_cores;
    out
}

/// The single largest fleet program, short enough for a refusal line. The
/// tree is named only when it accounts for every proc in the cluster.
pub(crate) fn top_holder(reading: &Footprint) -> Option<String> {
    let ranked = top_consumers(&reading.top, 1);
    let first = ranked.first()?;
    let procs = first["procs"].as_u64()?;
    let tree = first["worktree"].as_str();
    let tree_procs = first["worktree_procs"].as_u64().unwrap_or(0);
    let cpu_pct = first["cpu_pct"].as_f64().unwrap_or(0.0);
    let full = tree.is_some_and(|t| !t.is_empty()) && tree_procs == procs && procs >= 2;
    let name = first["name"].as_str().unwrap_or("?");
    let base = format!("{} {procs} procs {:.2} cores", name, cpu_pct / 100.0);
    match (tree, full) {
        (Some(t), true) => Some(format!("{base} in {t}")),
        _ => Some(base),
    }
}

fn gap_row_count(gap: &str) -> Option<u64> {
    let head: String = gap.chars().take_while(|c| c.is_ascii_digit()).collect();
    if head.is_empty() {
        return None;
    }
    head.parse::<u64>().ok()
}

/// The `(41.1%[, up to ...])` bracket shared by reasons and the line.
pub(crate) fn share_segment(
    share_low: f64,
    share_high: f64,
    bound: &str,
    gap: Option<&str>,
) -> String {
    let mut seg = format!("{:.1}%", share_low * 100.0);
    if bound == "upper" {
        seg += &match gap.and_then(gap_row_count) {
            Some(rows) => format!(
                ", up to {:.1}% with {rows} rows unattributed",
                share_high * 100.0
            ),
            None => format!(", up to {:.1}% with rows unattributed", share_high * 100.0),
        };
    }
    seg
}

/// The one CPU-axis decider, consumed by both gates and every readout
/// (LD1/LD3). The fleet's attributed share decides; a gap widens it to an
/// interval bounded above by the machine's measured CPU. Pure.
pub(crate) fn cpu_admission(
    reading: &Footprint,
    capacity_cores: f64,
    share_ceiling: f64,
) -> Admission {
    let holder = top_holder(reading);
    let holder_clause = holder
        .as_deref()
        .map(|h| format!("; top holder {h}"))
        .unwrap_or_default();
    let share_low = if capacity_cores > 0.0 {
        reading.fleet_cpu_cores / capacity_cores
    } else {
        0.0
    };
    let gap = reading.attribution_gap.clone();
    let machine_share = if capacity_cores > 0.0 {
        reading.measured_cpu_cores / capacity_cores
    } else {
        0.0
    };
    let share_high = if gap.is_some() {
        share_low.max(machine_share)
    } else {
        share_low
    };
    let bound = if gap.is_some() { "upper" } else { "exact" };
    let ceil_pct = share_ceiling * 100.0;
    let seg = share_segment(share_low, share_high, bound, gap.as_deref());
    let mut a = Admission {
        verdict: String::new(),
        axis: "fleet_cpu_share".to_string(),
        reason: String::new(),
        share_low,
        share_high,
        bound: bound.to_string(),
        fleet_cores: reading.fleet_cpu_cores,
        machine_cores: reading.measured_cpu_cores,
        capacity_cores,
        ceiling: share_ceiling,
        gap: gap.clone(),
        top_holder: holder,
    };
    if share_high <= share_ceiling {
        a.verdict = "admit".to_string();
        a.reason = format!(
            "fleet CPU {:.3} of {capacity_cores:.2} cores ({seg}) against max_fleet_cpu_share {ceil_pct:.1}%; admitting",
            reading.fleet_cpu_cores
        );
    } else if share_low > share_ceiling {
        a.verdict = "hold".to_string();
        a.reason = format!(
            "spawn held: the fleet holds {:.2}/{capacity_cores:.2} cores ({:.1}%) over max_fleet_cpu_share {ceil_pct:.1}%{}; waiting for the fleet's own work to drain (--no-wait to fail fast, --force to bypass)",
            reading.fleet_cpu_cores, share_low * 100.0, holder_clause
        );
    } else {
        a.verdict = "undecidable".to_string();
        a.reason = format!(
            "spawn-gate: the fleet's CPU share cannot be decided: attributed {:.1}% of capacity, up to {:.1}% with rows unattributed, and max_fleet_cpu_share {ceil_pct:.1}% falls inside that band; {}{}; close the attribution gap or lower foreign load (--force to bypass)",
            share_low * 100.0, share_high * 100.0, gap.unwrap_or_default(), holder_clause
        );
    }
    a
}

/// ``clean`` | ``unexplained`` | ``unknown``: a LEAK detector, never a
/// capacity number.
pub(crate) fn leak_verdict(direct: u64, threshold: Option<u64>) -> &'static str {
    match threshold {
        None => "unknown",
        Some(t) => {
            if direct > t {
                "unexplained"
            } else {
                "clean"
            }
        }
    }
}

/// A registry row flattened to the fields attribution reads.
#[derive(Debug, Clone)]
struct Row {
    name: String,
    harness: String,
    short_id: String,
    harness_session_id: String,
    node: Option<String>,
    status: String,
    pid: Option<u32>,
    pid_start_time: Option<u64>,
    exited_at: Option<String>,
    mux_session: Option<String>,
    mux_pane_id: Option<u64>,
}

impl Row {
    fn from_value(v: &Value) -> Option<Row> {
        Some(Row {
            name: v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            harness: v
                .get("harness")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            short_id: v
                .get("short_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            harness_session_id: v
                .get("harness_session_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            node: v
                .get("node")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            status: v.get("status").and_then(Value::as_str)?.to_string(),
            pid: v.get("pid").and_then(Value::as_u64).map(|p| p as u32),
            pid_start_time: v.get("pid_start_time").and_then(Value::as_u64),
            exited_at: v
                .get("exited_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            mux_session: v
                .get("mux")
                .and_then(|m| m.get("session"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            mux_pane_id: v
                .get("mux")
                .and_then(|m| m.get("pane_id"))
                .and_then(Value::as_u64),
        })
    }

    fn is_live(&self) -> bool {
        LIVE_STATUSES.contains(&self.status.as_str())
    }
}

/// Process liveness with the pid guard: reject an out-of-range pid outright
/// (it is the value that turns a signal into a broadcast), then the zombie
/// flag, then the start-token compare.
pub(crate) fn pid_alive(pid: u32, recorded_start: Option<u64>) -> Option<bool> {
    if pid <= 1 || pid > 0x7FFF_FFFF {
        return Some(false);
    }
    let Some((current_start, zombie)) = crate::process_probe::process_bsd(pid) else {
        return None;
    };
    if zombie {
        return Some(false);
    }
    match recorded_start {
        Some(recorded) => Some(current_start == recorded),
        None => Some(true),
    }
}

/// True only when the pid runs under a start time we never recorded:
/// recycling must be proven, never guessed.
fn pid_recycled(pid: u32, recorded_start: Option<u64>) -> bool {
    let Some(recorded) = recorded_start else {
        return false;
    };
    crate::process_probe::process_start_time(pid)
        .map(|current| current != recorded)
        .unwrap_or(false)
}

/// Root liveness. pid 1 stays local to this reader on purpose: the shared
/// guard rejects pid <= 1 for every caller, and widening it would change
/// liveness semantics fleet-wide; a pid-1 root is proven by token alone.
fn root_pid_is_live(pid: u32, pid_start: Option<u64>) -> Option<bool> {
    if pid == 1 {
        let current = crate::process_probe::process_start_time(pid)?;
        return Some(match pid_start {
            None => true,
            Some(token) => current == token,
        });
    }
    pid_alive(pid, pid_start)
}

/// The 8-hex key the rv farm and roster join on.
fn transport_join_key(short_id: &str, session_id: &str) -> String {
    let is_8hex = short_id.len() == 8
        && short_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if is_8hex || session_id.is_empty() {
        return short_id.to_string();
    }
    session_id.split('-').next().unwrap_or("").to_string()
}

/// The rendezvous socket farm root (FNO_CC_DAEMON_RV_ROOT overrides it).
fn rv_socket_root() -> PathBuf {
    match std::env::var("FNO_CC_DAEMON_RV_ROOT") {
        Ok(over) if !over.is_empty() => PathBuf::from(over),
        _ => PathBuf::from(format!("/tmp/cc-daemon-{}", unsafe { libc::getuid() })),
    }
}

/// Parse `lsof -F pfn` output into `{socket stem: pid}`; first pid wins.
fn pid_map_from_lsof(text: &str) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    let mut pid: Option<u32> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.parse().ok();
        } else if let Some(rest) = line.strip_prefix('n') {
            if let Some(pid) = pid {
                let stem = Path::new(rest)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !stem.is_empty() {
                    out.entry(stem).or_insert(pid);
                }
            }
        }
    }
    out
}

/// 8-hex claude jobId -> pid of the process that IS that bg session. Best
/// effort: a missing farm, a dead lsof, or a timeout all mean an empty map,
/// which undercounts; readers that gate on cost treat empty as unknown.
fn bg_socket_pid_map(deadline: Option<Instant>) -> BTreeMap<String, u32> {
    let base = rv_socket_root();
    let mut socks: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.flatten() {
            let rv = entry.path().join("rv");
            if let Ok(live) = std::fs::read_dir(&rv) {
                for sock in live.flatten() {
                    if sock.path().extension().and_then(|e| e.to_str()) == Some("sock") {
                        socks.push(sock.path());
                    }
                }
            }
        }
    }
    if socks.is_empty() || deadline.is_some_and(|d| Instant::now() >= d) {
        return BTreeMap::new();
    }
    socks.sort();
    let mut argv: Vec<String> = vec!["-F".into(), "pfn".into(), "--".into()];
    argv.extend(socks.iter().map(|p| p.display().to_string()));
    let Ok(out) = std::process::Command::new("lsof").args(&argv).output() else {
        return BTreeMap::new();
    };
    pid_map_from_lsof(&String::from_utf8_lossy(&out.stdout))
}

/// The claude daemon roster as `{8-hex jobId: host pid}`, or None when
/// unreadable. A missing file reads as an empty map.
fn roster_pid_map() -> Option<BTreeMap<String, Option<u32>>> {
    let base = match std::env::var("FNO_CLAUDE_DAEMON_DIR") {
        Ok(over) if !over.is_empty() => PathBuf::from(over),
        _ => home_dir().join(".claude").join("daemon"),
    };
    let raw = match std::fs::read_to_string(base.join("roster.json")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(BTreeMap::new()),
        Err(_) => return None,
    };
    let value: Value = serde_json::from_str(&raw).ok()?;
    let workers = value.get("workers")?.as_object()?;
    let mut out = BTreeMap::new();
    for w in workers.values() {
        let Some(session) = w.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        let pid = w.get("pid").and_then(Value::as_u64).map(|p| p as u32);
        out.insert(session.split('-').next().unwrap_or("").to_string(), pid);
    }
    Some(out)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The codex session id a rollout file holds, from the rollout's own
/// session_meta record (the filename UUID is not always the session id).
fn codex_rollout_session_id(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = vec![0u8; 64 * 1024];
    let read = file.read(&mut head).ok()?;
    let text = String::from_utf8_lossy(&head[..read]);
    for line in text.lines() {
        let value: Value = serde_json::from_str(line).ok()?;
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            return value
                .pointer("/payload/id")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    None
}

/// codex session id -> pid of the process holding that thread's rollout.
/// The rollout fd IS the identity; the walk stays inside codex-named
/// processes. Best effort: an unreadable walk means an empty map.
fn codex_rollout_pid_map(
    wanted: &BTreeSet<String>,
    deadline: Option<Instant>,
) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    if wanted.is_empty() || deadline.is_some_and(|d| Instant::now() >= d) {
        return out;
    }
    let table = crate::census::process_table().0;
    let codex_pids: Vec<String> = table
        .iter()
        .filter(|r| r.command.to_lowercase().contains("codex"))
        .map(|r| r.pid.to_string())
        .collect();
    if codex_pids.is_empty() {
        return out;
    }
    let mut argv: Vec<String> = vec!["-F".into(), "pfn".into()];
    for pid in &codex_pids {
        argv.push("-p".into());
        argv.push(pid.clone());
    }
    let Ok(run) = std::process::Command::new("lsof").args(&argv).output() else {
        return out;
    };
    let mut pid: Option<u32> = None;
    for line in String::from_utf8_lossy(&run.stdout).lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.parse().ok();
        } else if let Some(rest) = line.strip_prefix('n') {
            let Some(holder) = pid else {
                continue;
            };
            let name = Path::new(rest)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) {
                continue;
            }
            let Some(sid) = codex_rollout_session_id(Path::new(rest)) else {
                continue;
            };
            if wanted.contains(&sid) {
                out.entry(sid).or_insert(holder);
            }
        }
    }
    out
}

/// The `worker:<name>` claim state, or None when the store cannot answer.
/// free/stale positively report no live holder; an unreadable store proves
/// nothing.
fn claim_witness(name: &str) -> Option<String> {
    let key = format!("worker:{name}");
    let (state, _) = crate::claims::status(&key, None);
    Some(state.as_str().to_string())
}

/// Exact pane liveness through the front door's pane wait (exit codes are
/// the contract), falling back to the authoritative enumeration. `None` =
/// the mux could not answer.
fn mux_pane_alive(session: &str, pane_id: u64) -> Option<bool> {
    let fno = front_door_bin()?;
    let code = std::process::Command::new(&fno)
        .args([
            "mux",
            "pane",
            "wait",
            "--server",
            session,
            &pane_id.to_string(),
            "--timeout",
            "0",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?
        .code()
        .unwrap_or(-1);
    if code == 12 {
        // _WAIT_EXITED: the pane is gone.
        return Some(false);
    }
    if code == 0 || code == 11 {
        return Some(true);
    }
    let out = std::process::Command::new(&fno)
        .args(["mux", "pane", "ls", "--session", session, "--json"])
        .output()
        .ok()?;
    let rows: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let present = rows
        .iter()
        .any(|r| r.get("pane_id").and_then(Value::as_u64) == Some(pane_id));
    Some(!present)
}

fn front_door_bin() -> Option<PathBuf> {
    std::env::var_os("FNO_FRONT_DOOR")
        .map(PathBuf::from)
        .or_else(|| which("fno"))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|c| c.is_file())
}

/// Positive transcript evidence this pidless row is advancing: the roster's
/// own ADVANCING verdict, a working/watching reading inside the stale
/// attention window. An unreadable probe proves nothing.
fn row_is_advancing(row: &Row) -> bool {
    let handle = if !row.harness_session_id.is_empty() {
        row.harness_session_id.clone()
    } else if !row.short_id.is_empty() {
        row.short_id.clone()
    } else {
        row.name.clone()
    };
    let Some(probe) = crate::truth_probe::family1_truth_probe(&handle) else {
        return false;
    };
    if probe
        .reachability
        .as_deref()
        .map_or(true, |r| r == "unreachable")
    {
        return false;
    }
    match probe.state.as_str() {
        "working" | "watching" => probe
            .last_activity_age_s
            .map(|age| age < crate::daemon::STALE_ATTENTION_S)
            .unwrap_or(false),
        _ => false,
    }
}

/// Whether an unrouted live row's cost is plausibly real and unattributed.
/// Fail closed: only a POSITIVE dead answer drops the row.
fn unrouted_row_costs_fleet(row: &Row, deadline: Option<Instant>) -> bool {
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return true;
    }
    if let (Some(session), Some(pane_id)) = (&row.mux_session, row.mux_pane_id) {
        return mux_pane_alive(session, pane_id).is_none();
    }
    match claim_witness(&row.name) {
        Some(word) => !(word == "free" || word == "stale"),
        None => true,
    }
}

/// Only the exit-transition stamp (`exited_at`) counts; a stamp of T covers
/// [T, T+1), so a snapshot inside that window cannot rule out a transition.
fn terminal_row_changed_after_snapshot(row: &Row, snapshot_at: f64) -> bool {
    let Some(stamp) = &row.exited_at else {
        return false;
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(stamp) else {
        return true;
    };
    let ts = parsed.with_timezone(&Utc);
    let resolution = if ts.timestamp_subsec_nanos() == 0 {
        1.0
    } else {
        0.0
    };
    ts.timestamp() as f64 + resolution > snapshot_at
}

/// The shared codex app-server's live root and its verdict: `live`,
/// `absent` (every oracle cleanly names no live process) or `unreadable`.
fn codex_app_server_serve(snapshot_pids: &BTreeSet<u32>) -> (BTreeSet<u32>, String) {
    let daemons = codex_home().join("app-server-daemon");
    let oracles: Vec<(PathBuf, Vec<&str>)> = vec![
        (
            daemons.join("fno-harness-daemon.json"),
            vec![
                "processStartToken",
                "process_start_time",
                "startTime",
                "processStartTime",
            ],
        ),
        (daemons.join("app-server.pid"), vec![]),
    ];
    let mut all_absent = true;
    for (path, token_keys) in oracles {
        let record: Option<Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        let pid = record
            .as_ref()
            .and_then(|r| r.get("pid"))
            .and_then(Value::as_u64)
            .map(|p| p as u32);
        let token = record.as_ref().and_then(|r| {
            token_keys
                .iter()
                .find_map(|key| r.get(key).and_then(Value::as_u64).filter(|v| *v > 0))
        });
        let valid = match pid {
            Some(p) if p > 0 => token_keys.is_empty() || token.is_some(),
            _ => false,
        };
        if !valid {
            all_absent = false;
            continue;
        }
        let pid = pid.unwrap_or(0);
        match root_pid_is_live(pid, token) {
            None => return (BTreeSet::new(), "unreadable".to_string()),
            Some(true) => return (BTreeSet::from([pid]), "live".to_string()),
            Some(false) => {
                if snapshot_pids.contains(&pid) {
                    all_absent = false;
                }
            }
        }
    }
    (
        BTreeSet::new(),
        if all_absent {
            "absent".to_string()
        } else {
            "unreadable".to_string()
        },
    )
}

fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME") {
        Some(home) => PathBuf::from(home),
        None => home_dir().join(".codex"),
    }
}

/// The confirmed pid of the detached shared opencode serve, or a named gap.
fn live_shared_serve_root_pids(
    snapshot_pids: &BTreeSet<u32>,
) -> Result<(BTreeSet<u32>, Option<String>), String> {
    let home = AgentsHome::from_env();
    let path = home.root().join("opencode-serve.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return Ok((BTreeSet::new(), None)),
    };
    let Ok(record) = serde_json::from_str::<Value>(&text) else {
        return Err(
            "shared opencode serve root unattributed: opencode-serve.json is not a mapping"
                .to_string(),
        );
    };
    if !record.is_object() {
        return Err(
            "shared opencode serve root unattributed: opencode-serve.json is not a mapping"
                .to_string(),
        );
    }
    let pid = record.get("pid").and_then(Value::as_u64);
    let pid_start = record.get("pid_start").and_then(Value::as_u64);
    let (Some(pid), Some(start)) = (pid, pid_start) else {
        return Err("shared opencode serve root unattributed: opencode-serve.json carries no usable pid and pid_start pair".to_string());
    };
    if pid == 0 || start == 0 {
        return Err("shared opencode serve root unattributed: opencode-serve.json carries no usable pid and pid_start pair".to_string());
    }
    match root_pid_is_live(pid as u32, Some(start)) {
        None => Err(format!(
            "shared opencode serve root unattributed: pid {pid} start time could not be read"
        )),
        Some(true) => Ok((BTreeSet::from([pid as u32]), None)),
        Some(false) => {
            if snapshot_pids.contains(&(pid as u32)) && !pid_recycled(pid as u32, Some(start)) {
                Err(format!(
                    "shared opencode serve root unattributed: pid {pid} is dead, sits in the ps snapshot, and recycling is unproven"
                ))
            } else {
                Ok((BTreeSet::new(), None))
            }
        }
    }
}

/// Name the bg-socket route that can resolve this pidless row, or None.
fn pidless_route(row: &Row) -> Option<&'static str> {
    if !row.short_id.is_empty() {
        return Some("bg-socket");
    }
    if row.harness == "claude" && !row.harness_session_id.is_empty() {
        return Some("bg-socket");
    }
    None
}

/// The shared daemon hosting this row's work, or None. A table, so the next
/// harness with a shared daemon plugs in beside codex.
fn shared_daemon_label(row: &Row) -> Option<&'static str> {
    match row.harness.as_str() {
        "codex" => Some("codex-app-server"),
        _ => None,
    }
}

/// The row's name as the error text reads it; the registry's absent name
/// reads as "?" (the deleted leg's getattr default).
fn name_or(row: &Row) -> &str {
    if row.name.is_empty() {
        "?"
    } else {
        &row.name
    }
}

/// Positively live worker pids that may have detached children, plus the
/// named attribution gap. `Err` refuses the whole reading; `Ok`'s gap text
/// names what the share above it undercounts.
fn live_root_pids(
    deadline: Option<Instant>,
    snapshot_pids: &BTreeSet<u32>,
    snapshot_at: f64,
    serves: &BTreeMap<String, String>,
) -> Result<(BTreeSet<u32>, Option<String>), String> {
    let home = AgentsHome::from_env();
    let raw_rows = crate::whoami::registry_rows(&home);
    let rows: Vec<Row> = raw_rows.iter().filter_map(Row::from_value).collect();
    let mut recycled_idx: Vec<usize> = Vec::new();
    let mut dead_rows: Vec<String> = Vec::new();
    let mut gap_rows: Vec<String> = Vec::new();
    let mut roots: BTreeSet<u32> = BTreeSet::new();
    for row in rows.iter() {
        if row.is_live() {
            continue;
        }
        if row.pid.is_none() && terminal_row_changed_after_snapshot(row, snapshot_at) {
            return Err(format!(
                "worker root liveness unavailable: terminal row {} exited inside the ps snapshot's second, so a later transition cannot be ruled out",
                name_or(row)
            ));
        }
        let Some(pid) = row.pid else {
            continue;
        };
        if !snapshot_pids.contains(&pid) {
            continue;
        }
        let Some(start) = row.pid_start_time else {
            return Err(format!(
                "worker root liveness unavailable: terminal row {} sits in the ps snapshot and carries no pid start token",
                name_or(row)
            ));
        };
        match root_pid_is_live(pid, Some(start)) {
            Some(true) => {
                roots.insert(pid);
            }
            _ => {
                if !pid_recycled(pid, Some(start)) {
                    return Err(format!(
                        "worker root liveness unavailable: terminal row {} pid {pid} is not live and recycling is unproven",
                        name_or(row)
                    ));
                }
            }
        }
    }
    for (idx, row) in rows.iter().enumerate() {
        if !row.is_live() {
            continue;
        }
        let (Some(pid), Some(start)) = (row.pid, row.pid_start_time) else {
            continue;
        };
        match root_pid_is_live(pid, Some(start)) {
            None => {
                return Err(format!(
                    "worker root liveness unavailable: pid {pid} start time could not be read for row {}",
                    name_or(row)
                ));
            }
            Some(true) => {
                roots.insert(pid);
            }
            Some(false) => {
                if snapshot_pids.contains(&pid) && !pid_recycled(pid, Some(start)) {
                    return Err(format!(
                        "worker root liveness unavailable: row {} pid {pid} is dead, sits in the ps snapshot, and recycling is unproven",
                        name_or(row)
                    ));
                }
                recycled_idx.push(idx);
            }
        }
    }
    let pidless: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.is_live() && (r.pid.is_none() || r.pid_start_time.is_none()))
        .map(|(i, _)| i)
        .collect();
    let daemon_live: Vec<usize> = pidless
        .iter()
        .copied()
        .filter(|i| {
            shared_daemon_label(&rows[*i])
                .and_then(|label| serves.get(label))
                .map(String::as_str)
                == Some("live")
        })
        .collect();
    let own_rows: Vec<usize> = pidless
        .iter()
        .copied()
        .filter(|i| !daemon_live.contains(i))
        .collect();
    let unrouted: Vec<usize> = own_rows
        .iter()
        .copied()
        .filter(|i| pidless_route(&rows[*i]).is_none())
        .collect();
    let routed: Vec<(usize, String)> = own_rows
        .iter()
        .copied()
        .filter(|i| pidless_route(&rows[*i]).is_some())
        .map(|i| {
            (
                i,
                transport_join_key(&rows[i].short_id, &rows[i].harness_session_id),
            )
        })
        .collect();
    let codex_keys: Vec<(usize, String)> = unrouted
        .iter()
        .copied()
        .filter(|i| rows[*i].harness == "codex" && !rows[*i].harness_session_id.is_empty())
        .map(|i| (i, rows[i].harness_session_id.clone()))
        .collect();
    let mut resolved_codex: Vec<usize> = Vec::new();
    let wanted: BTreeSet<String> = codex_keys.iter().map(|(_, sid)| sid.clone()).collect();
    if !wanted.is_empty() && !deadline.is_some_and(|d| Instant::now() >= d) {
        let codex_pids = codex_rollout_pid_map(&wanted, deadline);
        for (idx, sid) in &codex_keys {
            let Some(pid) = codex_pids.get(sid) else {
                continue;
            };
            if root_pid_is_live(*pid, None) != Some(true) {
                dead_rows.push(format!(
                    "{} (codex rollout pid={pid})",
                    name_or(&rows[*idx])
                ));
                resolved_codex.push(*idx);
                continue;
            }
            roots.insert(*pid);
            resolved_codex.push(*idx);
        }
    }
    let unresolved: Vec<usize> = unrouted
        .iter()
        .copied()
        .filter(|i| !resolved_codex.contains(i))
        .collect();
    let mut advancing: Vec<usize> = Vec::new();
    for idx in &unresolved {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        if row_is_advancing(&rows[*idx]) {
            advancing.push(*idx);
        }
    }
    let fleet_unrouted: Vec<usize> = unresolved
        .iter()
        .copied()
        .filter(|i| !advancing.contains(i) && unrouted_row_costs_fleet(&rows[*i], deadline))
        .collect();
    if !fleet_unrouted.is_empty() {
        let mut labels: Vec<String> = fleet_unrouted
            .iter()
            .map(|i| {
                format!(
                    "{} (node={})",
                    name_or(&rows[*i]),
                    rows[*i].node.clone().unwrap_or_else(|| "unknown".into())
                )
            })
            .collect();
        labels.sort();
        let mut harnesses: Vec<String> = fleet_unrouted
            .iter()
            .map(|i| rows[*i].harness.clone())
            .collect();
        harnesses.sort();
        harnesses.dedup();
        gap_rows.push(format!(
            "{} pidless row(s) with no identity route ({}; rows: {})",
            fleet_unrouted.len(),
            harnesses.join(", "),
            labels.join(", ")
        ));
    }
    if !recycled_idx.is_empty() {
        let mut labels: Vec<String> = recycled_idx
            .iter()
            .map(|i| format!("{} (pid={})", name_or(&rows[*i]), rows[*i].pid.unwrap_or(0)))
            .collect();
        labels.sort();
        labels.sort();
        gap_rows.push(format!(
            "{} live row(s) whose pid was reused: {}",
            recycled_idx.len(),
            labels.join(", ")
        ));
    }
    if !dead_rows.is_empty() {
        dead_rows.sort();
        gap_rows.push(format!(
            "{} live row(s) whose resolved pid is not live: {}",
            dead_rows.len(),
            dead_rows.join(", ")
        ));
        dead_rows.clear();
    }
    if routed.is_empty() {
        return Ok((
            roots,
            if gap_rows.is_empty() {
                None
            } else {
                Some(gap_rows.join("; "))
            },
        ));
    }
    let mut socket_blind = deadline.is_some_and(|d| Instant::now() >= d);
    let socket_pids = if socket_blind {
        BTreeMap::new()
    } else {
        bg_socket_pid_map(deadline)
    };
    socket_blind = socket_blind || deadline.is_some_and(|d| Instant::now() >= d);
    let mut missing: Vec<usize> = routed
        .iter()
        .enumerate()
        .filter(|(_, (_, key))| !socket_pids.contains_key(key))
        .map(|(pos, _)| pos)
        .collect();
    let mut roster_pids: Option<BTreeMap<String, Option<u32>>> = None;
    if !missing.is_empty() {
        roster_pids = roster_pid_map();
        let mut still_missing: Vec<usize> = Vec::new();
        for pos in missing.drain(..) {
            let (idx, key) = &routed[pos];
            let readable = roster_pids.is_some();
            let in_roster = roster_pids.as_ref().map(|m| m.contains_key(key));
            if readable && in_roster == Some(false) && !socket_blind {
                continue;
            }
            let Some(pid) = roster_pids
                .as_ref()
                .and_then(|m| m.get(key))
                .copied()
                .flatten()
            else {
                still_missing.push(pos);
                continue;
            };
            match root_pid_is_live(pid, None) {
                Some(true) => {
                    roots.insert(pid);
                }
                _ => dead_rows.push(format!("{} (roster pid={pid})", name_or(&rows[*idx]))),
            }
        }
        missing = still_missing;
    }
    if !missing.is_empty() {
        gap_rows.push(format!(
            "{} bg-socket row(s) {}{}",
            missing.len(),
            if socket_blind {
                "unresolved: bg-socket resolution timed out"
            } else {
                "missing from the socket map"
            },
            if roster_pids.is_none() {
                " (roster oracle unavailable)"
            } else {
                ""
            }
        ));
    }
    for (idx, key) in &routed {
        let Some(pid) = socket_pids.get(key) else {
            continue;
        };
        match root_pid_is_live(*pid, None) {
            Some(true) => {
                roots.insert(*pid);
            }
            _ => dead_rows.push(format!("{} (socket pid={pid})", name_or(&rows[*idx]))),
        }
    }
    if !dead_rows.is_empty() {
        dead_rows.sort();
        gap_rows.push(format!(
            "{} live row(s) whose resolved pid is not live: {}",
            dead_rows.len(),
            dead_rows.join(", ")
        ));
    }
    Ok((
        roots,
        if gap_rows.is_empty() {
            None
        } else {
            Some(gap_rows.join("; "))
        },
    ))
}

/// One CPU fleet reading shared by the gate and the doctor footprint
/// command: the single producer, so the two explanations cannot drift.
/// `Err` carries the reason (an exit-4 answer).
pub(crate) fn cause_reading(memory: bool, timeout: Duration) -> Result<CauseReading, String> {
    let deadline = Instant::now().checked_add(timeout);
    let (table, _unreadable) = crate::census::process_table();
    let snapshot_pids: BTreeSet<u32> = table.iter().map(|r| r.pid).collect();
    let snapshot_at = unix_now();
    let (shared_serve_pids, shared_serve_error) = match live_shared_serve_root_pids(&snapshot_pids)
    {
        Ok((pids, gap)) => (pids, gap),
        Err(text) => {
            if text.starts_with("shared opencode serve root unattributed") {
                (BTreeSet::new(), Some(text))
            } else {
                return Err(format!("footprint unavailable: {text}"));
            }
        }
    };
    let (codex_roots, codex_verdict) = codex_app_server_serve(&snapshot_pids);
    let mut serves = BTreeMap::new();
    serves.insert("codex-app-server".to_string(), codex_verdict);
    let (root_pids, root_gap) = live_root_pids(deadline, &snapshot_pids, snapshot_at, &serves)?;
    let mut attribution_gap = root_gap;
    if let Some(serve_gap) = shared_serve_error {
        attribution_gap = Some(match attribution_gap {
            Some(existing) => format!("{existing}; {serve_gap}"),
            None => serve_gap,
        });
    }
    let discovered: BTreeSet<u32> = root_pids
        .union(&shared_serve_pids)
        .copied()
        .chain(codex_roots.iter().copied())
        .collect();
    for pid in &discovered {
        if !snapshot_pids.contains(pid) {
            return Err(
                "footprint unavailable: discovered worker root missing from ps snapshot"
                    .to_string(),
            );
        }
    }
    let mut excluded = BTreeSet::new();
    excluded.insert(std::process::id());
    let mut threshold_excluded = shared_serve_pids.clone();
    threshold_excluded.extend(codex_roots.iter().copied());
    let mut reading = parse_footprint(
        &table,
        &excluded,
        &discovered,
        &threshold_excluded,
        SUSTAINED_FLOOR_SECONDS,
    );
    reading.attribution_gap = attribution_gap;
    if memory {
        reading.memory = Some(crate::memory_groups::reading(
            &AgentsHome::from_env(),
            &table,
        ));
    }
    Ok(CauseReading {
        footprint: reading,
        sccache_server_pid: crate::cargo_build_dirs::sccache_row_pid(&table),
        sccache_restarts_1h: Some(crate::machine_watch::sccache_restarts_1h() as u64),
    })
}

/// The sustained-floor default the reading uses (the deleted leg's
/// SUSTAINED_FLOOR_SECONDS).
pub(crate) const SUSTAINED_FLOOR_SECONDS: u64 = 30;

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The longest prefix of `command` whose JSON encoding fits `byte_limit`
/// (the deleted `_bounded_json_command` bisection; the encoding is monotone
/// in the prefix length, so a boundary scan finds the same prefix).
fn bounded_json_command(command: &str, byte_limit: usize) -> String {
    let fits = |end: usize| {
        serde_json::to_string(&command[..end])
            .map(|s| s.len())
            .unwrap_or(usize::MAX)
            <= byte_limit
    };
    let mut best = 0usize;
    let mut end = 0usize;
    loop {
        if !fits(end) {
            break;
        }
        best = end;
        if end == command.len() {
            break;
        }
        end += 1;
        while end < command.len() && !command.is_char_boundary(end) {
            end += 1;
        }
    }
    command[..best].to_string()
}

/// The payload both the gate and the doctor footprint command emit. `None`
/// threshold fields stay JSON null so an unread count never reads as zero.
pub(crate) fn cause_payload(
    reading: &CauseReading,
    cause_only: bool,
    process_threshold: Option<u64>,
    degraded_note: Option<&str>,
    cwd: &Path,
) -> Value {
    let footprint = &reading.footprint;
    let capacity = cpu_capacity_cores();
    let load = load_snapshot();
    let admission = cpu_admission(
        footprint,
        capacity as f64,
        crate::agents_config::max_fleet_cpu_share(cwd),
    );
    let threshold_cores = sustained_cpu_threshold(capacity as f64, cwd);
    let measured_share = if footprint.measured_cpu_cores > 0.0 {
        footprint.fleet_cpu_cores / footprint.measured_cpu_cores * 100.0
    } else {
        0.0
    };
    let leak = if cause_only {
        "unknown"
    } else {
        leak_verdict(footprint.direct_process_count, process_threshold)
    };
    let mut exit_code = 0;
    if !cause_only {
        // The CPU axis keeps the historical exit (readers depend on 3); a gap
        // alone no longer forces an exit - both gates read the interval. When
        // BOTH alarms fire, the CPU axis wins the exit and the leak prints.
        if admission.verdict != "admit" {
            exit_code = EXIT_CAPACITY_OVER;
        } else if leak == "unexplained" {
            exit_code = EXIT_LEAK;
        }
    }
    let top_limit = if cause_only { 5 } else { usize::MAX };
    let command_limit = if cause_only { Some(1024usize) } else { None };
    let mut p = Map::new();
    p.insert(
        "sustained_cpu_cores".into(),
        json!(footprint.sustained_cpu_cores),
    );
    p.insert(
        "descendant_cpu_cores".into(),
        json!(footprint.descendant_cpu_cores),
    );
    p.insert("fleet_cpu_cores".into(), json!(footprint.fleet_cpu_cores));
    p.insert(
        "sustained_cpu_threshold_cores".into(),
        json!(threshold_cores),
    );
    p.insert("fleet_cpu_threshold_cores".into(), json!(threshold_cores));
    p.insert(
        "transient_call_count".into(),
        json!(footprint.transient_call_count),
    );
    p.insert("process_count".into(), json!(footprint.process_count));
    p.insert(
        "direct_process_count_threshold".into(),
        process_threshold.map(Value::from).unwrap_or(Value::Null),
    );
    p.insert(
        "descendant_process_count".into(),
        json!(footprint.descendant_process_count),
    );
    p.insert(
        "direct_process_count".into(),
        json!(footprint.direct_process_count),
    );
    p.insert(
        "memory".into(),
        footprint.memory.clone().unwrap_or(Value::Null),
    );
    p.insert("cpu_capacity_cores".into(), json!(capacity));
    p.insert(
        "fleet_percent_capacity".into(),
        json!(footprint.fleet_cpu_cores / capacity as f64 * 100.0),
    );
    p.insert("fleet_percent_measured_cpu".into(), json!(measured_share));
    p.insert("leak_verdict".into(), json!(leak));
    p.insert("admission".into(), admission_value(&admission));
    p.insert("capacity_verdict".into(), json!(admission.verdict));
    p.insert(
        "load_1m".into(),
        load.0.map(Value::from).unwrap_or(Value::Null),
    );
    p.insert(
        "load_5m".into(),
        load.1.map(Value::from).unwrap_or(Value::Null),
    );
    p.insert(
        "load_15m".into(),
        load.2.map(Value::from).unwrap_or(Value::Null),
    );
    p.insert("load_cpu_count".into(), json!(load.3));
    p.insert(
        "spare_pool_process_count".into(),
        json!(footprint.spare_pool_process_count),
    );
    p.insert(
        "spare_pool_cpu_cores".into(),
        json!(footprint.spare_pool_cpu_cores),
    );
    let top: Vec<Value> = footprint
        .top
        .iter()
        .take(top_limit)
        .map(|(cpu, command)| {
            let bounded = command_limit
                .map(|limit| bounded_json_command(command, limit))
                .unwrap_or_else(|| command.clone());
            json!({"cpu_percent": cpu, "command": bounded})
        })
        .collect();
    p.insert("top".into(), Value::Array(top));
    p.insert("unparsed_lines".into(), json!(footprint.unparsed_lines));
    p.insert("unparsed_samples".into(), json!(footprint.unparsed_samples));
    p.insert("exit_code".into(), json!(exit_code));
    if let Some(gap) = &footprint.attribution_gap {
        p.insert("attribution_gap".into(), json!(gap));
    }
    if !cause_only {
        p.insert(
            "sccache_server_pid".into(),
            reading
                .sccache_server_pid
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
        p.insert(
            "sccache_restarts_1h".into(),
            reading
                .sccache_restarts_1h
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
    }
    if let Some(note) = degraded_note {
        p.insert("degraded".into(), json!(note));
    }
    Value::Object(p)
}

fn admission_value(admission: &Admission) -> Value {
    json!({
        "verdict": admission.verdict,
        "axis": admission.axis,
        "reason": admission.reason,
        "share_low": admission.share_low,
        "share_high": admission.share_high,
        "bound": admission.bound,
        "fleet_cores": admission.fleet_cores,
        "machine_cores": admission.machine_cores,
        "capacity_cores": admission.capacity_cores,
        "ceiling": admission.ceiling,
        "gap": admission.gap,
        "top_holder": admission.top_holder,
    })
}

/// The live roster row count, `None` when the registry is unreadable (an
/// unreadable registry must not read as an empty fleet).
pub(crate) fn live_registry_row_count() -> Option<usize> {
    let home = AgentsHome::from_env();
    let Ok(value) = crate::registry_store::read(&home.registry_json()) else {
        return None;
    };
    let Some(rows) = value.get("agents").and_then(Value::as_array) else {
        return None;
    };
    let live = rows
        .iter()
        .filter_map(Row::from_value)
        .filter(|r| r.is_live())
        .count();
    Some(live)
}

/// The display/trend load reading; nothing gates on it (LD1).
fn load_snapshot() -> (Option<f64>, Option<f64>, Option<f64>, u64) {
    let capacity = cpu_capacity_cores();
    let mut loads = [0f64; 3];
    // SAFETY: loads points at a valid 3-element buffer.
    let ok = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
    if ok != 3 {
        return (None, None, None, capacity);
    }
    (Some(loads[0]), Some(loads[1]), Some(loads[2]), capacity)
}

/// Sorted-key JSON, the byte shape the deleted `json.dumps(..., sort_keys=True)`
/// froze for stdout consumers.
pub fn sorted_json(value: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut out = Map::new();
                for key in keys {
                    out.insert(key.clone(), sort(&map[key]));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&sort(value)).unwrap_or_else(|_| "{}".into())
}

/// `fno doctor footprint`: the reading, the payload, the human text and the
/// exit code, in one command body (the deleted `footprint_command`).
pub fn footprint_command(json_output: bool, cause_only: bool) -> i32 {
    let reading = match cause_reading(!cause_only, Duration::from_secs(5)) {
        Ok(reading) => reading,
        Err(reason) => {
            // The _emit_failure shape: the bare reason on human stdout, the
            // {"error", "exit_code": 4} object under --json. Exit 4 either way.
            if json_output {
                println!(
                    "{}",
                    json!({"error": reason, "exit_code": EXIT_UNAVAILABLE})
                );
            } else {
                println!("{reason}");
            }
            return EXIT_UNAVAILABLE;
        }
    };
    // The threshold: the roster explains the live rows + the daemon. On a
    // roster failure the measurement still prints with the threshold degraded
    // away and the reason named.
    let (threshold, degraded) = if cause_only {
        (None, None)
    } else {
        match live_registry_row_count() {
            Some(count) => (Some(count as u64 + DAEMON_ALLOWANCE), None),
            None => (None, Some("roster unavailable: no row count")),
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let payload = cause_payload(&reading, cause_only, threshold, degraded, &cwd);
    if json_output {
        println!("{}", sorted_json(&payload));
        return payload["exit_code"].as_i64().map(|c| c as i32).unwrap_or(0);
    }
    // The human render (the deleted _emit_result's text branch), reading
    // the payload's own numbers so the two outputs cannot disagree.
    let fp = &reading.footprint;
    let adm = &payload["admission"];
    let exit_code = payload["exit_code"].as_i64().unwrap_or(0) as i32;
    println!(
        "fleet CPU: {:.3} cores ({:.1}% capacity, {:.1}% of measured CPU)",
        payload["fleet_cpu_cores"].as_f64().unwrap_or(0.0),
        payload["fleet_percent_capacity"].as_f64().unwrap_or(0.0),
        payload["fleet_percent_measured_cpu"]
            .as_f64()
            .unwrap_or(0.0),
    );
    println!(
        "cpu admission: fleet {:.3} of {:.2} cores ({}) against max_fleet_cpu_share {:.1}% -> {}",
        adm["fleet_cores"].as_f64().unwrap_or(0.0),
        adm["capacity_cores"].as_f64().unwrap_or(0.0),
        share_segment(
            adm["share_low"].as_f64().unwrap_or(0.0),
            adm["share_high"].as_f64().unwrap_or(0.0),
            adm["bound"].as_str().unwrap_or("exact"),
            adm["gap"].as_str(),
        ),
        adm["ceiling"].as_f64().unwrap_or(0.0) * 100.0,
        adm["verdict"].as_str().unwrap_or("?"),
    );
    println!(
        "sustained CPU: {:.3} cores (threshold {:.3} from {} cpus; a separate axis - it did not decide the verdict)",
        payload["sustained_cpu_cores"].as_f64().unwrap_or(0.0),
        payload["sustained_cpu_threshold_cores"].as_f64().unwrap_or(0.0),
        payload["cpu_capacity_cores"].as_u64().unwrap_or(0),
    );
    println!(
        "descendant CPU: {:.3} cores ({} processes)",
        payload["descendant_cpu_cores"].as_f64().unwrap_or(0.0),
        payload["descendant_process_count"].as_u64().unwrap_or(0),
    );
    println!(
        "processes: {}",
        payload["process_count"].as_u64().unwrap_or(0)
    );
    let threshold = payload["direct_process_count_threshold"].as_u64();
    match threshold {
        Some(t) => println!(
            "unexplained processes: {} ({} direct, roster explains {t})",
            fp.direct_process_count.saturating_sub(t),
            fp.direct_process_count
        ),
        None => println!(
            "unexplained processes: unknown ({} direct, roster unavailable)",
            fp.direct_process_count
        ),
    }
    println!(
        "transient calls: {}",
        payload["transient_call_count"].as_u64().unwrap_or(0)
    );
    match reading.sccache_server_pid {
        Some(pid) => println!(
            "sccache server: pid {pid}, restarts last hour: {}",
            reading
                .sccache_restarts_1h
                .map(|r| r.to_string())
                .unwrap_or_else(|| "unknown".into())
        ),
        None => println!("sccache server: none running"),
    }
    if fp.spare_pool_process_count > 0 {
        println!(
            "claude spare pool: {} processes, {:.2} cores (idle pre-warm, not fleet; never sweep it - a bg spawn is claimed from it)",
            fp.spare_pool_process_count,
            fp.spare_pool_cpu_cores
        );
    }
    if let Some(mem) = &fp.memory {
        if let Some(line) = mem.get("line").and_then(Value::as_str) {
            println!("{line}");
        }
    }
    if let Some(gap) = &fp.attribution_gap {
        println!(
            "attribution gap: {gap} (the share reads as an interval bounded above by the machine's measured CPU)"
        );
    }
    let axis = adm["axis"].as_str().unwrap_or("");
    let axis_numbers = if axis == "fleet_cpu_share" {
        format!(
            " ({:.1}% against {:.1}%)",
            adm["share_low"].as_f64().unwrap_or(0.0) * 100.0,
            adm["ceiling"].as_f64().unwrap_or(0.0) * 100.0,
        )
    } else {
        String::new()
    };
    if exit_code == EXIT_LEAK {
        let t = threshold.unwrap_or(0);
        println!(
            "verdict: leak on unexplained processes ({} of {} direct, roster explains {t}; cpu admission: {} on {axis}{axis_numbers}, a separate axis - it did not decide the verdict) (exit {exit_code})",
            fp.direct_process_count.saturating_sub(t),
            fp.direct_process_count,
            adm["verdict"].as_str().unwrap_or("?"),
        );
    } else {
        println!(
            "verdict: {} on {axis}{axis_numbers} (exit {exit_code})",
            adm["verdict"].as_str().unwrap_or("?"),
        );
    }
    if fp.unparsed_lines > 0 {
        println!("unparsed lines: {}", fp.unparsed_lines);
        for sample in &fp.unparsed_samples {
            println!(
                "  row {} (pid {}, {}): {}",
                sample.row,
                sample
                    .pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "?".into()),
                sample.reason,
                sample.masked
            );
        }
    }
    if let Some(note) = degraded {
        println!("degraded: {note}");
    }
    if exit_code != 0 || cause_only {
        println!("top fleet consumers:");
        for (cpu, command) in fp.top.iter().take(5) {
            println!("  {command} ({cpu:.1}%)");
        }
    }
    exit_code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, ppid: u32, elapsed_s: u64, cpu: f64, command: &str) -> ProcRow {
        ProcRow {
            pid,
            ppid,
            state: 'S',
            elapsed_s,
            cpu_pct: cpu,
            rss_kb: 1024,
            command: command.to_string(),
        }
    }

    #[test]
    fn ac1_attributes_transitive_build_descendants() {
        let rows = vec![
            row(100, 1, 3600, 20.0, "fno-agents-worker --run"),
            row(101, 100, 3600, 0.0, "/bin/sh -c cargo test"),
            row(102, 101, 5, 80.0, "cargo test -p fno"),
            row(103, 101, 5, 60.0, "rustc --crate-name fno"),
            row(200, 1, 3600, 0.0, "/bin/sh -c cargo test elsewhere"),
            row(201, 200, 3600, 90.0, "cargo test -p unrelated"),
            row(202, 200, 3600, 70.0, "rustc --crate-name unrelated"),
        ];
        let out = parse_footprint(
            &rows,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            30,
        );
        assert!((out.descendant_cpu_cores - 1.4).abs() < 1e-9);
        assert!((out.fleet_cpu_cores - 1.6).abs() < 1e-9);
        assert_eq!(out.descendant_process_count, 3);
        assert_eq!(out.direct_process_count, 1);
        assert_eq!(out.process_count, 4);
        let tops: Vec<&str> = out.top.iter().map(|(_, c)| c.as_str()).collect();
        assert_eq!(
            tops,
            vec![
                "cargo test -p fno",
                "rustc --crate-name fno",
                "fno-agents-worker --run"
            ]
        );
    }

    #[test]
    fn ac1_edge_attributes_a_detached_registered_root() {
        let rows = vec![
            row(300, 1, 5, 20.0, "opencode serve --detach"),
            row(301, 300, 60, 80.0, "cargo test -p fno"),
            row(400, 1, 3600, 90.0, "cargo test -p unrelated"),
        ];
        let roots: BTreeSet<u32> = BTreeSet::from([300]);
        let out = parse_footprint(&rows, &BTreeSet::new(), &roots, &BTreeSet::new(), 30);
        assert_eq!(out.process_count, 2);
        assert_eq!(out.direct_process_count, 1);
        assert_eq!(out.descendant_process_count, 1);
        assert!((out.sustained_cpu_cores - 0.2).abs() < 1e-9);
        assert_eq!(out.transient_call_count, 0);
        assert!((out.descendant_cpu_cores - 0.8).abs() < 1e-9);
        assert!((out.fleet_cpu_cores - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ac1_edge_shared_serve_is_not_a_leak_threshold_process() {
        let rows = vec![
            row(300, 1, 5, 20.0, "opencode serve --detach"),
            row(301, 300, 60, 80.0, "cargo test -p fno"),
        ];
        let roots: BTreeSet<u32> = BTreeSet::from([300]);
        let excluded: BTreeSet<u32> = BTreeSet::from([300]);
        let out = parse_footprint(&rows, &BTreeSet::new(), &roots, &excluded, 30);
        assert_eq!(out.direct_process_count, 0);
    }

    #[test]
    fn ac3_the_classifier_counts_only_the_real_runners() {
        let rows = vec![
            row(100, 1, 3600, 10.0, "cargo test -p fno"),
            row(101, 1, 3600, 10.0, "/usr/bin/python3 -m pytest cli/tests"),
            row(102, 1, 3600, 10.0, "/bin/zsh /x/pytest-of-user/junk.sh"),
            row(103, 1, 3600, 10.0, "zsh -c echo pytest-of-user"),
        ];
        let out = parse_footprint(
            &rows,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            30,
        );
        assert_eq!(out.test_process_count, 2);
    }

    #[test]
    fn spare_pool_rows_count_but_never_fleet_attribute() {
        let rows = vec![
            row(50, 1, 3600, 12.0, "/usr/local/bin/claude bg-spare"),
            row(51, 1, 3600, 12.0, "claude --bg-pty-host --session x"),
            row(52, 1, 3600, 12.0, "claude bg-spare /tmp/x.claim.sock"),
        ];
        let out = parse_footprint(
            &rows,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            30,
        );
        // Two idle pool rows; the third argv names a live claim socket, so
        // the session took the spare and it is not pool.
        assert_eq!(out.spare_pool_process_count, 2);
        assert_eq!(out.process_count, 0);
    }

    #[test]
    fn a_claimed_spare_with_a_live_socket_is_not_the_pool() {
        let dir =
            std::env::temp_dir().join(format!("fno-spare-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("claim.claim.sock");
        std::fs::write(&sock, b"").unwrap();
        let cmd = format!("claude bg-spare {}", sock.display());
        let rows = vec![row(60, 1, 3600, 5.0, &cmd)];
        let out = parse_footprint(
            &rows,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            30,
        );
        assert_eq!(out.spare_pool_process_count, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn admission_answer_admit_hold_undecidable_by_the_interval() {
        // 1.2 fleet cores of 4 capacity = 30% share; ceiling 50% admits.
        let mut fp = Footprint::default();
        fp.fleet_cpu_cores = 1.2;
        fp.measured_cpu_cores = 3.0;
        let a = cpu_admission(&fp, 4.0, 0.5);
        assert_eq!(a.verdict, "admit");
        assert_eq!(a.bound, "exact");
        // Attributed 30% but measured 75%: the ceiling falls inside the band.
        fp.attribution_gap =
            Some("2 pidless row(s) with no identity route (claude; rows: a (node=unknown))".into());
        let a = cpu_admission(&fp, 4.0, 0.5);
        assert_eq!(a.verdict, "undecidable");
        assert_eq!(a.bound, "upper");
        assert!(a.reason.contains("cannot be decided"));
        // Attributed share alone over the ceiling holds.
        fp.attribution_gap = None;
        fp.fleet_cpu_cores = 3.0;
        let a = cpu_admission(&fp, 4.0, 0.5);
        assert_eq!(a.verdict, "hold");
        assert!(a.reason.contains("spawn held"));
    }

    #[test]
    fn share_segment_names_the_interval_rows() {
        assert_eq!(share_segment(0.411, 0.411, "exact", None), "41.1%");
        assert_eq!(
            share_segment(0.1, 0.9, "upper", Some("3 pidless row(s) ...")),
            "10.0%, up to 90.0% with 3 rows unattributed"
        );
        assert_eq!(
            share_segment(0.1, 0.9, "upper", None),
            "10.0%, up to 90.0% with rows unattributed"
        );
    }

    #[test]
    fn top_consumers_name_program_and_worktree() {
        let sustained = vec![
            (80.0f64, "cargo test -p fno".to_string()),
            (70.0, "/x/w/worktrees/x-1/cargo test -p fno".to_string()),
            (60.0, "fno-agents-worker --run".to_string()),
            (50.0, "/x/w/worktrees/x-1/cargo test -p fno2".to_string()),
        ];
        let out = top_consumers(&sustained, 2);
        assert_eq!(out[0]["name"], json!("cargo"));
        assert_eq!(out[0]["procs"], json!(2));
        assert_eq!(out[0]["worktree"], json!("/x/w/worktrees/x-1"));
        assert_eq!(out[0]["worktree_procs"], json!(2));
    }

    #[test]
    fn bounded_json_command_keeps_the_fitting_prefix() {
        let long = "x".repeat(300);
        let out = bounded_json_command(&long, 100);
        // The JSON encoding of N x's is N + 2 bytes.
        assert_eq!(out.len(), 98);
        assert!(out.chars().all(|c| c == 'x'));
    }

    #[test]
    fn transport_join_key_derives_from_the_session_first_segment() {
        assert_eq!(transport_join_key("ab12cd34", "ignored"), "ab12cd34");
        assert_eq!(transport_join_key("", "abcdefgh-1234-x"), "abcdefgh");
        assert_eq!(transport_join_key("", ""), "");
    }

    #[test]
    fn pid_alive_guard_rejects_the_broadcast_range() {
        assert_eq!(pid_alive(0, None), Some(false));
        assert_eq!(pid_alive(1, None), Some(false));
        assert_eq!(pid_alive(0x8000_0000, None), Some(false));
    }

    #[test]
    fn is_test_runner_matches_own_program_only() {
        assert!(is_test_runner("cargo test -p fno"));
        assert!(is_test_runner("cargo nextest run"));
        assert!(is_test_runner("fno doctor test cli"));
        assert!(is_test_runner("/usr/bin/python3 -m pytest x"));
        assert!(!is_test_runner("tail -f /tmp/pytest-of-user/out"));
        assert!(!is_test_runner("git log --grep pytest"));
    }
}
