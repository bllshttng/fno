//! The `fno doctor lanes` advisor: how many more lanes fit, or an honest
//! refusal. Port of `cli/src/fno/doctor_lanes.py`, whose Python leg this
//! landing deletes.
//!
//! The answer reads the WHOLE machine, not just the fleet: macmon samples
//! whole-machine CPU, memory and power, and the lane number prints ONLY when
//! both resource arms measured - a dark sensor is never headroom. The lane
//! number itself is the spawn-gate probe's own read (the learned effective
//! cap minus live slots), so the advisor and a refusal cannot disagree: one
//! decider, the probe, called in process.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::spawn_gate_command::probe_answer;

/// One macmon sample, then the pipe is closed: a one-shot read, not a
/// subscription. The tool streams forever, so the bounded kill is the NORMAL
/// exit path, and a sample that never arrived is a DARK arm.
pub(crate) const MACMON_TIMEOUT_S: u64 = 5;
pub(crate) const MEMORY_PRESSURE_TIMEOUT_S: u64 = 5;

/// Per-lane seed costs (cores, GB) when no live roster row exists to measure
/// from; observed resident sizes on the specifying node.
pub(crate) const SEED_PER_LANE_CORES: f64 = 0.1;
pub(crate) const SEED_PER_LANE_GB: f64 = 0.35;

const MEASURED: &str = "measured";
const DARK: &str = "dark";

/// One arm of the lane answer: what it read, or why it could not.
#[derive(Debug, Clone, Default)]
pub(crate) struct ArmReading {
    pub name: String,
    pub state: String,
    pub value: Option<Value>,
    pub source: String,
    pub reason: String,
}

/// The lane reading: arms, the answer, or the named refusal.
#[derive(Debug, Clone, Default)]
pub(crate) struct LaneReading {
    pub arms: Vec<ArmReading>,
    pub lane_count: Option<i64>,
    pub per_lane_cpu_cores: Option<f64>,
    pub per_lane_mem_gb: Option<f64>,
    pub cost_source: String,
    pub refusal_reason: String,
    pub census: Value,
}

/// The lanes command body for the census op's `--lanes` arm: the JSON or
/// human render, exit 3 on refusal.
pub fn lanes_command(json_output: bool) -> i32 {
    let reading = read_lanes();
    if json_output {
        println!(
            "{}",
            crate::footprint_cause::sorted_json(&reading.to_json())
        );
    } else {
        println!("{}", render(&reading));
    }
    if reading.lane_count.is_none() {
        3
    } else {
        0
    }
}

/// The first complete JSON object in one command's captured stdout, or None.
fn first_json_line(raw: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(raw);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if value.is_object() {
                return Some(value);
            }
        }
    }
    None
}

/// One `macmon pipe -s 1` sample, bounded, or a named darkness. A missing
/// binary, an empty pipe and unparseable output are all darkness - never an
/// assumption of headroom.
fn read_macmon(timeout: u64) -> (Option<Value>, Option<String>) {
    let Some(binary) = which_macmon() else {
        return (
            None,
            Some("macmon not on PATH (brew install macmon; Apple Silicon only)".to_string()),
        );
    };
    let mut child = match Command::new(&binary)
        .args(["pipe", "-s", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return (None, Some(format!("macmon failed: {e}"))),
    };
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stream) = stdout {
            use std::io::Read;
            let _ = stream.read_to_end(&mut buf);
        }
        buf
    });
    // macmon streams forever; the timeout kill is the NORMAL exit path.
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let raw = loop {
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            break reader.join().unwrap_or_default();
        }
        if matches!(child.try_wait(), Ok(Some(_))) {
            // macmon exited on its own (an error): take whatever it wrote.
            break reader.join().unwrap_or_default();
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let sample = first_json_line(&raw);
    if sample.is_none() {
        return (
            None,
            Some(format!("macmon produced no sample within {timeout:.0}s")),
        );
    }
    (sample, None)
}

fn which_macmon() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("macmon"))
        .find(|candidate| candidate.is_file())
}

/// The system free-memory percentage (0..1) from `memory_pressure`: the
/// fallback arm for a machine with no swap file.
fn read_memory_pressure(timeout: u64) -> (Option<f64>, Option<String>) {
    let child = Command::new("memory_pressure")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return (None, Some("memory_pressure failed: spawn".to_string()));
    };
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stream) = stdout {
            use std::io::Read;
            let _ = stream.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + Duration::from_secs(timeout);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    let raw = reader.join().unwrap_or_default();
    let text = String::from_utf8_lossy(&raw);
    let Some(re) = regex::Regex::new(r"System-wide memory free percentage:\s*(\d+)%").ok() else {
        return (
            None,
            Some("memory_pressure produced no free-percentage line".to_string()),
        );
    };
    let Some(caps) = re.captures(&text) else {
        return (
            None,
            Some("memory_pressure produced no free-percentage line".to_string()),
        );
    };
    let Some(pct) = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()) else {
        return (
            None,
            Some("memory_pressure produced no free-percentage line".to_string()),
        );
    };
    (Some(pct / 100.0), None)
}

impl ArmReading {
    fn measured(name: &str, value: Value, source: &str) -> Self {
        Self {
            name: name.to_string(),
            state: MEASURED.into(),
            value: Some(value),
            source: source.into(),
            reason: String::new(),
        }
    }

    fn dark(name: &str, reason: &str) -> Self {
        Self {
            name: name.to_string(),
            state: DARK.into(),
            value: None,
            source: String::new(),
            reason: reason.into(),
        }
    }
}

fn arm_to_json(arm: &ArmReading) -> Value {
    json!({
        "name": arm.name,
        "state": arm.state,
        "value": arm.value.clone().unwrap_or(Value::Null),
        "source": arm.source,
        "reason": arm.reason,
    })
}

impl LaneReading {
    fn to_json(&self) -> Value {
        json!({
            "lane_count": self.lane_count,
            "per_lane_cpu_cores": self.per_lane_cpu_cores,
            "per_lane_mem_gb": self.per_lane_mem_gb,
            "cost_source": self.cost_source,
            "refused_reason": self.refusal_reason,
            "census": self.census,
            "arms": Value::Array(self.arms.iter().map(arm_to_json).collect()),
        })
    }
}

/// The CPU axis, read through the SAME seam the gate reads (one decider,
/// one sample), so the advisor can never disagree with a refusal.
fn cpu_admission_arm() -> ArmReading {
    let reading = match crate::footprint_cause::cause_reading(false, Duration::from_secs(5)) {
        Ok(reading) => reading,
        Err(reason) => {
            return ArmReading::dark(
                "cpu admission",
                &format!("the CPU instrument is unreadable: {reason}"),
            );
        }
    };
    let capacity = crate::footprint_cause::cpu_capacity_cores() as f64;
    let ceiling =
        crate::agents_config::max_fleet_cpu_share(&std::env::current_dir().unwrap_or_default());
    let admission = crate::footprint_cause::cpu_admission(&reading.footprint, capacity, ceiling);
    if admission.axis == "cpu_instrument" {
        // Zeros the instrument never measured are not a reading; the
        // admission's own words name why the arm is dark.
        return ArmReading::dark("cpu admission", &admission.reason);
    }
    let loads = load_averages();
    ArmReading::measured(
        "cpu admission",
        json!({
            "fleet_cores": round3(admission.fleet_cores),
            "capacity_cores": round2(admission.capacity_cores),
            "share_low": round4(admission.share_low),
            "share_high": round4(admission.share_high),
            "ceiling": admission.ceiling,
            "verdict": admission.verdict,
            "bound": admission.bound,
            "load_15m": loads.2,
            "load_1m": loads.0,
            "load_5m": loads.1,
        }),
        "cpu_admission",
    )
}

/// The trend-line load averages; nothing decides on them.
fn load_averages() -> (Option<Value>, Option<Value>, Option<Value>) {
    let mut loads = [0f64; 3];
    // SAFETY: loads points at a valid 3-element buffer.
    let ok = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
    if ok != 3 {
        return (None, None, None);
    }
    (
        Some(json!(round1(loads[0]))),
        Some(json!(round1(loads[1]))),
        Some(json!(round1(loads[2]))),
    )
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

/// The whole-machine CPU arm: macmon's cpu_usage_pct plus the footprint
/// census counts (AC10). Above 1.0 is the sensor lying, not a percent
/// spelling to rescue.
fn machine_cpu_arm(
    sample: Option<&Value>,
    reason: Option<&str>,
    counts: Option<(u64, u64)>,
) -> ArmReading {
    let Some(sample) = sample else {
        return ArmReading::dark("whole-machine cpu", reason.unwrap_or("macmon dark"));
    };
    let Some(pct) = sample.get("cpu_usage_pct").and_then(Value::as_f64) else {
        return ArmReading::dark("whole-machine cpu", "macmon cpu_usage_pct unreadable");
    };
    ArmReading::measured(
        "whole-machine cpu",
        json!({
            "busy_fraction": round3(pct),
            "capacity_cores": crate::footprint_cause::cpu_capacity_cores(),
            // AC10: census rides the fleet read; unknown, never guessed.
            "runnable": counts.map(|c| json!(c.0)).unwrap_or(Value::Null),
            "processes": counts.map(|c| json!(c.1)).unwrap_or(Value::Null),
        }),
        "macmon cpu_usage_pct",
    )
}

/// The memory arm: macmon's swap numbers when a swap file exists, else
/// memory_pressure - swap pinned at zero is CONFIGURATION there, which
/// proves nothing, and zero swap is never read as headroom.
fn memory_arm(sample: Option<&Value>, macmon_reason: Option<&str>) -> ArmReading {
    let mem = sample
        .and_then(|s| s.get("memory"))
        .cloned()
        .unwrap_or(Value::Null);
    let num = |v: &Value| v.as_f64();
    let total = mem.get("ram_total").and_then(num);
    let usage = mem.get("ram_usage").and_then(num);
    let swap_total = mem.get("swap_total").and_then(num);
    let swap_usage = mem.get("swap_usage").and_then(num);
    if let (Some(total), Some(usage), Some(st), Some(su)) = (total, usage, swap_total, swap_usage) {
        if st > 0.0 {
            let available = (total - usage) + (st - su);
            return ArmReading::measured(
                "memory",
                json!({
                    "available_gb": round1((available.max(0.0)) / 1e9),
                    "swap_used_gb": round1(su / 1e9),
                }),
                "macmon swap_usage (swap_total > 0)",
            );
        }
    }
    let no_swap = swap_total == Some(0.0);
    let (free_pct, reason) = read_memory_pressure(MEMORY_PRESSURE_TIMEOUT_S);
    if let Some(free_pct) = free_pct {
        let available_gb = total
            .filter(|t| *t > 0.0)
            .map(|t| round1(free_pct * t / 1e9));
        return ArmReading::measured(
            "memory",
            json!({
                "free_fraction": (free_pct * 100.0).round() / 100.0,
                "available_gb": available_gb,
            }),
            if no_swap {
                "memory_pressure (swap_total is 0 - no swap file)"
            } else {
                "memory_pressure (macmon memory block unreadable)"
            },
        );
    }
    let why = if no_swap {
        format!(
            "no swap file and memory_pressure unreadable: {}",
            reason.unwrap_or_default()
        )
    } else {
        format!(
            "macmon memory unreadable ({}) and memory_pressure unreadable: {}",
            macmon_reason.unwrap_or("memory block unreadable"),
            reason.unwrap_or_default()
        )
    };
    ArmReading::dark("memory", &why)
}

/// The power and thermals arm.
fn power_arm(sample: Option<&Value>, reason: Option<&str>) -> ArmReading {
    let Some(sample) = sample else {
        return ArmReading::dark("power and thermals", reason.unwrap_or("macmon dark"));
    };
    let power = sample.get("sys_power").and_then(Value::as_f64);
    let temps = sample
        .get("temp")
        .and_then(|t| t.get("cpu_temp_avg"))
        .and_then(Value::as_f64);
    if power.is_none() && temps.is_none() {
        return ArmReading::dark("power and thermals", "macmon temp/power unreadable");
    }
    ArmReading::measured(
        "power and thermals",
        json!({
            "sys_power_w": power.map(round1),
            "cpu_temp_c": temps.map(round1),
        }),
        "macmon sys_power + temp.cpu_temp_avg",
    )
}

/// The fleet reading + live rows + the read cost, from ONE footprint pass
/// (the reader the leak threshold uses, so the two cannot describe
/// different fleets).
fn fleet_snapshot() -> (
    Option<crate::footprint_cause::CauseReading>,
    Option<String>,
    u128,
) {
    let started = Instant::now();
    let result = crate::footprint_cause::cause_reading(true, Duration::from_secs(5));
    let read_ms = started.elapsed().as_millis();
    match result {
        Ok(reading) => (Some(reading), None, read_ms),
        Err(reason) => (None, Some(reason), read_ms),
    }
}

/// Per-lane CPU cores and GB, measured from the fleet's own attributed
/// footprint over the live roster; the documented seed when no row is live.
fn fleet_cost(
    reading: Option<&crate::footprint_cause::CauseReading>,
    row_count: usize,
) -> (f64, f64, usize, String) {
    if row_count == 0 {
        let why = match reading {
            None => "footprint unavailable",
            Some(_) => "no live roster rows to measure",
        };
        return (
            SEED_PER_LANE_CORES,
            SEED_PER_LANE_GB,
            0,
            format!("seed ({why})"),
        );
    }
    let reading = reading.expect("row_count > 0 implies a reading");
    let per_cpu = (0.01f64).max(reading.footprint.fleet_cpu_cores / row_count as f64);
    let fleet_mb = reading
        .footprint
        .memory
        .as_ref()
        .and_then(|m| m.get("fleet"))
        .and_then(|f| f.get("mb"))
        .and_then(Value::as_f64);
    match fleet_mb {
        Some(mb) => (
            per_cpu,
            (0.01f64).max(mb / 1024.0 / row_count as f64),
            row_count,
            "measured from the live roster's attributed footprint".to_string(),
        ),
        None => (
            per_cpu,
            SEED_PER_LANE_GB,
            row_count,
            "cpu measured; memory seed (physical footprint unread)".to_string(),
        ),
    }
}

/// The team census: leads, workers, tests. Leads and workers are ROW counts,
/// tests is a PROCESS count, never folded together. The team roles fold
/// (gather_team) stays with the Python leg until the agents family ports it,
/// so those two counts read null with the reason named - the rule every arm
/// follows: an unread count NAMES why.
fn census_of(
    reading: Option<&crate::footprint_cause::CauseReading>,
    row_count: Option<usize>,
    share: Value,
    read_ms: u128,
) -> Value {
    let tests = reading.map(|r| r.footprint.test_process_count);
    let gap = reading.and_then(|r| r.footprint.attribution_gap.clone());
    json!({
        "leads": Value::Null,
        "lead_conflicts": Value::Null,
        "workers": Value::Null,
        "tests": tests.map(|t| json!(t)).unwrap_or(Value::Null),
        "roster_rows": row_count.map(|c| json!(c)).unwrap_or(Value::Null),
        "gate_notes": [
            "team roles fold unreadable in the native reading: leads/workers stay with the Python team fold until the agents family ports it"
        ],
        "attribution_gap": gap.map(|g| json!(g)).unwrap_or(Value::Null),
        "top_consumers": reading
            .map(|r| Value::Array(crate::footprint_cause::top_consumers(&r.footprint.top, 5)))
            .unwrap_or(Value::Null),
        "share": share,
        "roster_error": row_count.map(|_| Value::Null).unwrap_or(json!("roster unavailable")),
        "read_ms": json!(read_ms),
    })
}

/// Assemble every arm, then answer or refuse. The refusal rule: the lane
/// number needs BOTH resource arms (whole-machine CPU and memory) measured.
pub(crate) fn read_lanes() -> LaneReading {
    let (sample, macmon_reason) = read_macmon(MACMON_TIMEOUT_S);
    let load_arm = cpu_admission_arm();
    // One fleet read serves the census, the per-lane divisor, and the
    // machine arm's counts (AC10); taken before the refusal branch.
    let (footprint, _fp_error, read_ms) = fleet_snapshot();
    let row_count = crate::footprint_cause::live_registry_row_count();
    let counts = footprint.as_ref().map(|r| {
        (
            r.footprint.runnable_count,
            r.footprint.machine_process_count,
        )
    });
    let cpu_arm = machine_cpu_arm(sample.as_ref(), macmon_reason.as_deref(), counts);
    let mem_arm = memory_arm(sample.as_ref(), macmon_reason.as_deref());
    let power_arm = power_arm(sample.as_ref(), macmon_reason.as_deref());
    let mut reading = LaneReading::default();
    reading.arms = vec![load_arm.clone(), cpu_arm, mem_arm, power_arm];
    let full_probe = probe_answer(&json!({"mode": "probe", "caller_session": Value::Null}));
    let share = full_probe.get("share").cloned().unwrap_or(Value::Null);
    reading.census = census_of(footprint.as_ref(), row_count, share, read_ms);
    // The refusal rule: the lane number needs BOTH resource arms measured.
    let cpu_dark = reading
        .arms
        .iter()
        .any(|a| a.name == "whole-machine cpu" && a.state == DARK);
    let mem_dark = reading
        .arms
        .iter()
        .any(|a| a.name == "memory" && a.state == DARK);
    if cpu_dark || mem_dark {
        let dark: Vec<String> = reading
            .arms
            .iter()
            .filter(|a| a.state == DARK)
            .map(|a| format!("{} dark ({})", a.name, a.reason))
            .collect();
        let working: Vec<String> = reading
            .arms
            .iter()
            .filter(|a| a.state == MEASURED)
            .map(|a| a.name.clone())
            .collect();
        reading.refusal_reason = format!(
            "the machine arms cannot answer the lane question: {}{}",
            dark.join("; "),
            if working.is_empty() {
                String::new()
            } else {
                format!(" | still working: {}", working.join(", "))
            }
        );
        return reading;
    }
    let (per_cpu, per_gb, row_n, mut cost_source) =
        fleet_cost(footprint.as_ref(), row_count.unwrap_or(0));
    // The lane answer IS the gate's own cap read (one decider, no second
    // arithmetic). A dark or holding CPU axis caps the answer at 0 (AC12).
    let probe = probe_answer(&json!({"mode": "probe", "only": ["lanes"]}));
    let slots = probe
        .get("lanes")
        .and_then(|l| l.get("slots"))
        .and_then(Value::as_i64);
    let effective = probe
        .get("lanes")
        .and_then(|l| l.get("effective"))
        .and_then(Value::as_i64);
    let mut answer: Option<i64> = None;
    if let (Some(slots), Some(effective)) = (slots, effective) {
        answer = Some(0.max(effective - slots));
        cost_source += &format!("; probe {slots} live, effective cap {effective}");
    }
    // A dark sensor is never headroom, and neither is a hold, an undecidable
    // band, or a refusal (AC12).
    let load_arm = &reading.arms[0];
    let hold_verdict = load_arm
        .value
        .as_ref()
        .and_then(|v| v.get("verdict"))
        .and_then(Value::as_str)
        .map(|v| v == "hold" || v == "undecidable" || v == "refuse")
        .unwrap_or(false);
    if load_arm.state == DARK || hold_verdict {
        answer = Some(0);
        let cap_why = if load_arm.state == DARK {
            "dark".to_string()
        } else {
            load_arm
                .value
                .as_ref()
                .and_then(|v| v.get("verdict"))
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string()
        };
        cost_source += &format!("; cpu admission {cap_why}, answer capped at 0");
    }
    let Some(answer) = answer else {
        reading.refusal_reason = format!(
            "the gate cannot answer the lane question: the spawn-gate probe carries no effective cap ({})",
            probe.get("reason").and_then(Value::as_str).unwrap_or("gate unavailable")
        );
        return reading;
    };
    reading.lane_count = Some(answer);
    reading.per_lane_cpu_cores = Some(round3(per_cpu));
    reading.per_lane_mem_gb = Some(round3(per_gb));
    reading.cost_source = if row_n > 0 {
        format!("{cost_source} ({row_n} live row(s))")
    } else {
        cost_source
    };
    reading
}

/// Rendered so an unread count says so: a count the reader can act on and
/// a count nobody took must never look the same.
fn census_lines(census: &Value) -> Vec<String> {
    if census.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return Vec::new();
    }
    let n = |key: &str| -> String {
        match census.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => "unknown".to_string(),
            Some(v) => v.to_string(),
        }
    };
    let truthy = |key: &str| -> bool {
        match census.get(key) {
            Some(Value::Number(num)) => num.as_f64().map(|f| f != 0.0).unwrap_or(false),
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Bool(b)) => *b,
            Some(Value::Array(a)) => !a.is_empty(),
            _ => false,
        }
    };
    let mut lines = vec![format!(
        "  team: {} lead(s), {} worker(s), {} running test(s) ({} live roster row(s), {} ms)",
        n("leads"),
        n("workers"),
        n("tests"),
        n("roster_rows"),
        n("read_ms")
    )];
    if truthy("lead_conflicts") {
        lines.push(format!(
            "  team conflicts: {} scope(s) held by more than one live role - a bare lead count hides this",
            n("lead_conflicts")
        ));
    }
    if truthy("roster_error") {
        lines.push(format!(
            "  roster: {} - the counts above are unread",
            n("roster_error")
        ));
    }
    if let Some(Value::Array(notes)) = census.get("gate_notes") {
        for note in notes {
            if let Value::String(s) = note {
                lines.push(format!("  {s}"));
            }
        }
    }
    if truthy("attribution_gap") {
        lines.push(format!(
            "  attribution gap: {} - the fleet CPU share is an undercount, not headroom",
            n("attribution_gap")
        ));
    }
    lines
}

/// The answer and every arm's reading, or the refusal naming everything.
fn render(reading: &LaneReading) -> String {
    let mut lines: Vec<String> = Vec::new();
    if !reading.refusal_reason.is_empty() {
        lines.push(
            "lanes: REFUSED - no lane number is printed because a dark sensor is not headroom"
                .to_string(),
        );
        for arm in &reading.arms {
            if arm.state == DARK {
                lines.push(format!("  {}: dark - {}", arm.name, arm.reason));
            } else {
                lines.push(format!("  {}: measured ({})", arm.name, arm.source));
            }
        }
        lines.extend(census_lines(&reading.census));
        return lines.join("\n");
    }
    let lane_n = reading
        .lane_count
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    lines.push(format!("lanes: {lane_n} more fit"));
    for arm in &reading.arms {
        let v = arm
            .value
            .as_ref()
            .map(|val| val.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        lines.push(format!("  {}: {} ({})", arm.name, v, arm.source));
    }
    let cpu = reading
        .per_lane_cpu_cores
        .map(|v| format!("{v}"))
        .unwrap_or_else(|| "unknown".to_string());
    let mem = reading
        .per_lane_mem_gb
        .map(|v| format!("{v}"))
        .unwrap_or_else(|| "unknown".to_string());
    lines.push(format!(
        "  per-lane cost: {cpu} cores, {mem} GB - {}",
        reading.cost_source
    ));
    lines.extend(census_lines(&reading.census));
    lines.join("\n")
}
