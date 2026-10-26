//! The orphan sweep: processes that outlived whatever started them, said out
//! loud. Ported from the Python `fno.agents.orphans` the day its verb moved
//! onto the native census door (`fno-agents census --orphans`); the two rules
//! below are the module's thesis, inherited verbatim.
//!
//! The measurement this exists for: 73 `yes > /dev/null` at PPID 1, 797% CPU,
//! 71 core-hours in one night, plus a `grep -rn` at 64% and a set of background
//! tasks whose session had exited. The guard in `hooks/bg-process-guard.py`
//! prevents the first class at creation time. Nothing can refuse the other two
//! at creation time, so this sweep is what covers them, and it is the only
//! path-agnostic net in the design: a test fixture, a non-Claude harness, and a
//! hook that leaks its own work all land here.
//!
//! **Assert a positive marker, never an absence.** A sweep reporting zero
//! orphans has two explanations, and a clean machine looks exactly like a dead
//! instrument. So the sweep plants its own orphans first and must find them
//! before it may report a count. Each arm of the predicate gets its own probe
//! that cannot satisfy the other arm; a control clearing an easier bar than a
//! real finding clears is the same lie as the absence assertion it replaced.
//!
//! **Never `ps ... | awk`.** That pipeline returned count=0 twice during the
//! investigation, because the harness truncated 1189 lines and the pipeline
//! counted the summary. The table read here returns structs (the same libproc
//! walk [`crate::census`] uses), so there is no text stream to truncate.
//!
//! The reaper is MEASURED from the sweep's own probe, never assumed to be 1: a
//! subreaper host (`systemd --user`, PR_SET_CHILD_SUBREAPER containers)
//! reparents orphans elsewhere, and a literal `== 1` made every sweep there
//! print `verdict withheld` forever.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Name prefix that makes a process ours by construction. `--reap` kills only
/// these, which is why the guard's refusal text and `scripts/lib/loadgen.sh`
/// both hand back an `fno-load-...` name rather than a bare `timeout`.
pub const FNO_PREFIX: &str = "fno-";

/// A named orphan younger than this is left alone. A worker that just launched
/// something is not an orphan yet, it is a worker.
pub const REAP_MIN_AGE_S: f64 = 600.0;

const PROBE_LIFETIME_S: u64 = 30;

/// Name a sweep gives its own NAME probe. Excluded from findings so two sweeps
/// running inside one probe lifetime do not report each other's controls,
/// which the shared hourly stamp across worktrees makes likely.
const PROBE_PREFIX: &str = "fno-orphan-probe-";

/// The same marker for the CWD probe, deliberately WITHOUT the `fno-` prefix
/// so the NAME arm still cannot claim it and `--reap` still cannot kill it. A
/// concurrent sweep must be able to tell the control from a real orphan; the
/// NAME marker's leak on this arm is why the prefixes differ.
const CWD_PROBE_PREFIX: &str = "orphan-probe-cwd-";

/// Footnote's own daemons. Each runs detached at PPID 1 by design, so PPID 1
/// says nothing about whether one leaked, and a deliberate-rename test already
/// shields them from `--reap`. Matched on argv[0] exactly, never by prefix, so
/// a renamed `fno-agents-daemon-load` is still a finding.
const OWN_DAEMONS: &[&str] = &["fno-agents-daemon", "fno-agents-worker"];

/// One row of the process table as the sweep reads it. `cwd` is only read for
/// rows whose ppid is a reaper: it is a separate syscall per process, and a
/// process with no readable cwd can still match the NAME arm.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub pid: u32,
    pub ppid: u32,
    pub uid: Option<u32>,
    /// Epoch seconds of the process start; the seen-key's stable half.
    pub start_time: f64,
    /// Lifetime user+system CPU seconds. A mean over the whole life is the
    /// honest number for an orphan: the question is what it has cost.
    pub cpu_seconds: f64,
    /// The executable's name (`ps -o comm`), which an `exec -a` rename does
    /// not touch.
    pub exe_name: String,
    /// The launch-time argv. Attribution evidence, not current state.
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

impl Row {
    /// What an operator sees in `top`: argv[0], not the executable.
    ///
    /// MEASURED 2026-08-13, and it corrects the assumption this module was
    /// designed on. After `exec -a fno-load-test sleep 20`, `ps -o comm`
    /// prints `fno-load-test` but the process name reads `sleep`: the kernel
    /// name follows the executable and the rename lands in argv[0] alone.
    /// Attributing on the executable made the NAME arm unfirable, and the
    /// probe is what caught it.
    pub fn display_name(&self) -> &str {
        if let Some(first) = self.argv.first() {
            if !first.is_empty() {
                return first.rsplit('/').next().unwrap_or(first);
            }
        }
        &self.exe_name
    }

    /// True when argv[0] does not match the executable, i.e. someone
    /// relabelled this process on purpose.
    ///
    /// Load-bearing for the reap. `fno-agents` and `fno-mux` are real binaries
    /// whose argv[0] IS their name, and a bare `fno-` prefix test would hand
    /// them to the killer. A deliberate rename cannot happen by accident.
    pub fn was_renamed(&self) -> bool {
        !self.exe_name.is_empty() && self.display_name() != self.exe_name
    }
}

/// One finding: a process the sweep can name, with the arm that claimed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub pid: u32,
    pub name: String,
    pub exe_name: String,
    pub renamed: bool,
    pub cmdline: String,
    pub cwd: Option<String>,
    pub cpu_seconds: f64,
    pub age_seconds: f64,
    pub start_time: f64,
    /// NAME | CWD: which attribution arm claimed the row.
    pub arm: &'static str,
}

impl Finding {
    /// Mean CPU over the process's whole life. Deliberately not an
    /// instantaneous sample: a sweep that sleeps to sample is a sweep nobody
    /// runs at session start.
    pub fn avg_cpu_percent(&self) -> f64 {
        if self.age_seconds <= 0.0 {
            return 0.0;
        }
        100.0 * self.cpu_seconds / self.age_seconds
    }

    pub fn core_hours(&self) -> f64 {
        self.cpu_seconds / 3600.0
    }

    /// The reap gate: all three arms must hold. Attribution is a heuristic,
    /// and a heuristic must not hold a kill signal - this conjunction is what
    /// keeps it from holding one.
    pub fn reapable(&self) -> bool {
        self.renamed && self.name.starts_with(FNO_PREFIX) && self.age_seconds > REAP_MIN_AGE_S
    }
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub total: u64,
    /// Processes whose ppid is a reaper. Named for the CONDITION, not for
    /// pid 1: the label `ppid=1` was hardcoded inside a design whose whole
    /// point is that the reaper is measured.
    pub orphaned: u64,
    pub same_uid: u64,
    pub census_excluded: u64,
    /// Footnote's own long-lived daemons, counted rather than listed. Never
    /// silently dropped: an unnamed exclusion is the absence trap this module
    /// exists to refuse.
    pub daemons_excluded: u64,
    pub findings: Vec<Finding>,
    pub reaped: Vec<Finding>,
    pub control: std::collections::BTreeMap<&'static str, bool>,
    pub broken_reason: Option<String>,
    /// Whether `--reap` was asked for. The renderer must not print a `reaped:`
    /// line on a plain report run: "reaped: 0" there reads as an attempted
    /// kill that found nothing, when no kill was ever attempted.
    pub reap_requested: bool,
}

impl ScanResult {
    pub fn broken(&self) -> bool {
        self.broken_reason.is_some()
    }
}

/// The ppids that mark a process whose parent is gone.
///
/// A subreaper claims only its OWN descendants. PID 1 stays the reaper of last
/// resort for everything else, so a host with `systemd --user` has TWO such
/// ppids live at once. Measuring one reaper and filtering `ppid != reaper`
/// dropped every orphan under the other, and the sweep still printed a green
/// control and a positive count. A count that is wrong is worse than a
/// withheld one.
fn reaper_set(reaper: u32) -> HashSet<u32> {
    HashSet::from([reaper, 1])
}

/// Which arm claims this row, or None.
///
/// Exactly two arms. An earlier draft had a third matching
/// `.claude/worktrees/` in argv; CWD already catches everything it would, and
/// a third arm is a third thing the positive control has to prove.
///
/// There is no CPU floor here on purpose. CPU is reported on every finding
/// and gates none of them: specimen 3 burned no CPU at all, and a floor is a
/// bar a probe cannot clear without burning real CPU to clear it.
fn attribute(row: &Row, roots: &[String]) -> Option<&'static str> {
    if row.was_renamed() && row.display_name().starts_with(FNO_PREFIX) {
        return Some("NAME");
    }
    let cwd = row.cwd.as_deref()?;
    if cwd.contains("/.claude/worktrees/") {
        return Some("CWD");
    }
    for root in roots {
        let root = root.trim_end_matches('/');
        if cwd == root || cwd.starts_with(&format!("{root}/")) {
            return Some("CWD");
        }
    }
    None
}

/// The sweep's host: every machine touch behind a seam, so the tests drive
/// the arms the live probes cannot reach (foreign uid, census membership, the
/// reap age gate) exactly the way the Python suite drove them through
/// monkeypatched module functions.
pub trait SweepHost {
    fn repo_roots(&self) -> Vec<String>;
    /// Start a `sleep` orphaned to PPID 1 (renamed when `rename` is given)
    /// and return its pid.
    fn spawn_probe(&self, rename: Option<&str>, cwd: &Path) -> Option<u32>;
    /// The pid an orphan reparents to on this host, measured not assumed.
    fn await_orphaned(&self, pid: Option<u32>) -> Option<u32>;
    /// The exclusion set: pids of live workers that are supposed to exist.
    /// `None` is "the census is unreadable", which breaks the scan - never an
    /// empty set, which reads as "no live workers" and hands every legitimate
    /// `claude --bg` process to a reap the SessionStart hook runs unattended.
    fn census_pids(&self) -> Option<HashSet<u32>>;
    /// One table read. `reapers` scopes the cwd syscall to the rows the CWD
    /// arm can use.
    fn read_rows(&self, reapers: &HashSet<u32>) -> Vec<Row>;
    /// True when the pid still exists. EPERM means it is there and this uid
    /// cannot signal it: ALIVE. Reading a bare error as death makes both the
    /// kill control and the reap re-check answer "killed" for a process still
    /// running.
    fn pid_alive(&self, pid: u32) -> bool;
    /// SIGTERM, then SIGKILL if it is still there. Returns whether OUR signal
    /// reached the process: a pid that exited on its own between the scan and
    /// the reap must not be credited to the killer.
    fn kill(&self, pid: u32) -> bool;
    fn now(&self) -> f64;
    fn uid(&self) -> u32;
    /// The fixed, empty probe scratch directory. One directory, never a fresh
    /// tempdir per run: a sweep killed at its time bound never reaches its own
    /// cleanup, and a SIGKILL cannot be handled in-process at all, so the fix
    /// is to have nothing to clean up.
    fn scratch(&self) -> PathBuf;
}

/// The live host: the real machine.
pub struct Live;

impl SweepHost for Live {
    fn repo_roots(&self) -> Vec<String> {
        repo_roots()
    }

    fn spawn_probe(&self, rename: Option<&str>, cwd: &Path) -> Option<u32> {
        spawn_probe(rename, cwd)
    }

    fn await_orphaned(&self, pid: Option<u32>) -> Option<u32> {
        await_orphaned(pid, std::time::Duration::from_secs(3))
    }

    fn census_pids(&self) -> Option<HashSet<u32>> {
        Some(crate::agents_top::census_worker_pids())
    }

    fn read_rows(&self, reapers: &HashSet<u32>) -> Vec<Row> {
        read_rows(reapers)
    }

    fn pid_alive(&self, pid: u32) -> bool {
        pid_alive(pid)
    }

    fn kill(&self, pid: u32) -> bool {
        kill_proc(pid)
    }

    fn now(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    fn uid(&self) -> u32 {
        unsafe { libc::getuid() }
    }

    fn scratch(&self) -> PathBuf {
        let base = std::env::temp_dir().join("fno-orphan-probe");
        if std::fs::create_dir_all(&base).is_ok() {
            return base;
        }
        std::env::temp_dir()
    }
}

/// Plant the controls, enumerate, attribute, and optionally reap.
///
/// `skip_probe` is `name` or `cwd` and exists so an operator can break one
/// arm on purpose and watch the verdict be withheld. Without that, "the
/// control passed" is itself an unfalsifiable claim.
pub fn scan(host: &dyn SweepHost, reap: bool, skip_probe: Option<&str>) -> ScanResult {
    let mut result = ScanResult {
        reap_requested: reap,
        ..Default::default()
    };
    if let Some(skip) = skip_probe {
        if skip != "name" && skip != "cwd" {
            // An unrecognised value silently skipped nothing, the scan came
            // back healthy, and an operator read that as "the control works".
            // A falsifier that cannot fail is the shape this module refuses;
            // `FNO_ORPHANS_SKIP_PROBE=NAME` is one typo away.
            result.broken_reason = Some(format!(
                "FNO_ORPHANS_SKIP_PROBE={skip:?} is not 'name' or 'cwd'"
            ));
            return result;
        }
    }

    let roots = host.repo_roots();
    if roots.is_empty() {
        // No git root means the CWD arm has no territory. A fallback to the
        // cwd made every PPID-1 process under $HOME a finding when the
        // command ran from a home directory: a wall of noise from the one
        // command whose output has to stay trustworthy. Inventing territory
        // to keep a control green is the absence trap wearing a different
        // hat, so refuse and say which arm has nowhere to look.
        result.broken_reason = Some("no git root here, so the CWD arm has no territory".into());
        return result;
    }
    let repo_root = roots[0].clone();
    let scratch = host.scratch();
    let nonce: u64 = (host.now() * 1000.0) as u64 ^ (std::process::id() as u64);

    // NAME probe: renamed, and parked OUTSIDE any repo so the CWD arm cannot
    // carry it. Giving it the repo root as cwd would let CWD match first and
    // prove nothing about names.
    let name_pid = if skip_probe == Some("name") {
        None
    } else {
        host.spawn_probe(Some(&format!("{PROBE_PREFIX}{nonce:x}")), &scratch)
    };
    // CWD probe: in the repo, and marked with a prefix that carries no
    // `fno-`, so only the CWD arm can still claim it while a concurrent sweep
    // can tell it apart from a real orphan.
    let cwd_pid = if skip_probe == Some("cwd") {
        None
    } else {
        host.spawn_probe(
            Some(&format!("{CWD_PROBE_PREFIX}{nonce:x}")),
            Path::new(&repo_root),
        )
    };

    let mut census = host.census_pids();
    if census.is_none() {
        // Refuse rather than proceed with no exclusion set. Every legitimate
        // `claude --bg` worker is at PPID 1 with a worktree cwd, so an
        // unreadable census turns the fleet into findings, and SessionStart
        // runs `--reap` unattended.
        result.broken_reason = Some("census unreadable".into());
        census = Some(HashSet::new());
    }
    let census_pids = census.unwrap_or_default();
    let now = host.now();
    let uid = host.uid();
    let mut found_name_probe = false;
    let mut found_cwd_probe = false;

    // Inside the enumerated arm with every failure captured. An exception out
    // here escaped scan() as a traceback in the Python leg: exit 1, empty
    // stdout, silence for a full window - the one outcome this module exists
    // to make impossible. The reaper is MEASURED from our own probe, never
    // assumed to be 1. Either probe answers it; they reparent the same way.
    let reaper = match host.await_orphaned(name_pid) {
        Some(r) => Some(r),
        None => host.await_orphaned(cwd_pid),
    };
    if reaper.is_none() {
        // No probe settled, so there is no reaper to compare against and
        // every ppid test below would be meaningless. Fall THROUGH rather
        // than return: the kill block below is what cleans up our own two
        // probes, so returning here left them running for their full
        // lifetime on exactly the host where reparenting is already
        // misbehaving - the one path in this module that leaks the thing the
        // module hunts. Both arms stay False, so the verdict is still
        // withheld, and `control` names which arms failed.
        result.broken_reason = Some("no probe reparented, so the reaper is unknown".into());
    }
    let reapers = reaper.map(reaper_set).unwrap_or_default();
    let rows = host.read_rows(&reapers);
    for row in &rows {
        result.total += 1;
        if !reapers.contains(&row.ppid) {
            continue;
        }
        result.orphaned += 1;
        if row.uid != Some(uid) {
            continue;
        }
        result.same_uid += 1;
        // A row with no pid is unusable: it cannot be excluded by census,
        // reported by number, or killed. The struct carries no Option here -
        // an unreadable row never reaches the table at all.
        let pid = row.pid;
        if census_pids.contains(&pid) || pid == std::process::id() {
            result.census_excluded += 1;
            continue;
        }
        let arm = match attribute(row, &roots) {
            Some(arm) => arm,
            None => continue,
        };
        let display = row.display_name();
        if (display.starts_with(PROBE_PREFIX) || display.starts_with(CWD_PROBE_PREFIX))
            && Some(pid) != name_pid
            && Some(pid) != cwd_pid
        {
            continue; // another sweep's live probe, not an orphan
        }
        if OWN_DAEMONS.contains(&display) {
            // PPID 1 is these daemons' NORMAL state, so their presence
            // carries no signal at all, and every restart mints a fresh
            // seen-key that speaks again through --quiet-unless-new. Counted,
            // never dropped in silence.
            result.daemons_excluded += 1;
            continue;
        }
        let finding = Finding {
            pid,
            name: display.to_string(),
            exe_name: row.exe_name.clone(),
            renamed: row.was_renamed(),
            cmdline: if row.argv.is_empty() {
                row.exe_name.clone()
            } else {
                row.argv.join(" ")
            },
            cwd: row.cwd.clone(),
            cpu_seconds: row.cpu_seconds,
            age_seconds: (now - row.start_time).max(0.0),
            start_time: row.start_time,
            arm,
        };
        if Some(pid) == name_pid {
            found_name_probe = arm == "NAME";
            continue;
        }
        if Some(pid) == cwd_pid {
            found_cwd_probe = arm == "CWD";
            continue;
        }
        result.findings.push(finding);
    }

    // The kill control is measured AFTER the kill, never before it. Setting
    // it from "a probe was spawned" asserts an easier thing than the real
    // kill does - a broken kill path would report healthy while `--reap`
    // silently no-opped and still printed `reaped: N`. Both halves, for the
    // same reason the reap needs both: a probe that exited on its own
    // certifies nothing about the kill path.
    let mut killed_ok = true;
    for pid in [name_pid, cwd_pid].into_iter().flatten() {
        let signalled = host.kill(pid);
        killed_ok = killed_ok && signalled && !host.pid_alive(pid);
    }
    let probes_planted = name_pid.is_some() || cwd_pid.is_some();
    result.control.insert("NAME", found_name_probe);
    result.control.insert("CWD", found_cwd_probe);
    result.control.insert("kill", killed_ok && probes_planted);

    if result.broken_reason.is_none() {
        let missing: Vec<&str> = [
            ("NAME", found_name_probe),
            ("CWD", found_cwd_probe),
            ("kill", result.control["kill"]),
        ]
        .into_iter()
        .filter(|(_, ok)| !ok)
        .map(|(arm, _)| arm)
        .collect();
        if !missing.is_empty() {
            result.broken_reason = Some(format!("{} arm FAILED", missing.join(" and ")));
        }
    }

    // A broken scan reaps nothing. An instrument that cannot see its own
    // probe has not earned a kill signal.
    if reap && !result.broken() {
        for finding in &result.findings {
            if !finding.reapable() {
                continue;
            }
            // BOTH halves, never assumed. An unconditional append printed
            // `reaped: N` for a process still running, and the other
            // direction - a pid that exited on its own between the scan and
            // the reap - is a receipt for a signal nobody sent. The kill
            // control proves the PATH works against a probe we own; it says
            // nothing about this particular signal.
            if host.kill(finding.pid) && !host.pid_alive(finding.pid) {
                result.reaped.push(finding.clone());
            }
        }
    }

    result
        .findings
        .sort_by(|a, b| b.cpu_seconds.total_cmp(&a.cpu_seconds));
    result
}

/// Findings first, count last, and no count at all when the scan is broken.
pub fn render(result: &ScanResult) -> String {
    let mut lines = vec![format!(
        "scan: {} processes, {} reparented, {} same-uid, {} census-excluded, {} own-daemon",
        result.total,
        result.orphaned,
        result.same_uid,
        result.census_excluded,
        result.daemons_excluded,
    )];
    if result.broken() {
        lines.push(format!(
            "control: {}",
            result.broken_reason.as_deref().unwrap_or("unknown")
        ));
        lines.push("verdict withheld (scan-broken)".into());
        return lines.join("\n");
    }
    lines.push(
        "control: NAME arm ok, CWD arm ok, kill path ok \
         (uid, census and reap-age arms are unit-tested, not probed)"
            .into(),
    );
    for f in &result.findings {
        lines.push(format!(
            "  pid {:<7} {:<24} {:>6.1}% cpu {:>6.2} core-h  {}",
            f.pid,
            truncate_24(&f.name),
            f.avg_cpu_percent(),
            f.core_hours(),
            f.cwd.as_deref().unwrap_or("-"),
        ));
    }
    lines.push(format!("orphans: {}", result.findings.len()));
    if !result.reaped.is_empty() {
        let names: Vec<String> = result.reaped.iter().map(|f| f.pid.to_string()).collect();
        lines.push(format!(
            "reaped: {}  (fno-named, age > 10m: {})",
            result.reaped.len(),
            names.join(", ")
        ));
    } else if result.reap_requested && !result.findings.is_empty() {
        // Every orphan predating the guard is named whatever it was named,
        // and a name-gated reap can never touch it. Say so rather than
        // letting the gap read as a broken reap.
        lines.push("reaped: 0  (nothing both fno-named and older than 10m)".into());
    }
    lines.join("\n")
}

/// Python's `f.name[:24]:<24`: truncate to 24 chars, then pad.
fn truncate_24(name: &str) -> String {
    let cut: String = name.chars().take(24).collect();
    format!("{cut:<24}")
}

/// Identify the PROCESS, not the pid slot, using its START TIME.
///
/// `pid:name` alone filters out a genuinely new orphan that lands on a reused
/// pid with the same command name. An age BUCKET breaks the quiet gate
/// instead: age advances every minute, so a long-lived daemon gets a fresh
/// key at every hourly sweep, reads as new forever, and reprints at every
/// session start. Start time is stable for the life of the process, which is
/// the property the key actually needs.
fn seen_key(f: &Finding) -> String {
    format!("{}:{}:{}", f.pid, f.name, f.start_time as i64)
}

/// Where the already-reported set lives, anchored to THIS working tree.
///
/// Not a bare relative `.fno/...`: a caller with an unexpected cwd would nest
/// a second state directory somewhere it does not belong. `--show-toplevel`,
/// never `repo_roots()[0]`: those answer different questions. `repo_roots`
/// answers "which paths are ATTRIBUTABLE to this project" and is deliberately
/// every worktree, so its first entry is the canonical checkout. Ask it where
/// MY state lives and every worktree writes its quiet-gate into the canonical
/// checkout instead of its own: worktrees then silence each other's findings.
/// Falls back to cwd wherever git cannot name a working tree.
pub fn seen_path() -> PathBuf {
    let out = crate::bounded_cmd::output_with_timeout_result(
        {
            let mut c = std::process::Command::new("git");
            c.args(["rev-parse", "--show-toplevel"]);
            c
        },
        5,
    );
    let top = out
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let base = if top.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(top)
    };
    base.join(".fno").join(".orphan-sweep-seen")
}

/// Record this scan's findings and answer "is any of them new?".
///
/// The command always reports everything. This exists for the SessionStart
/// nudge, which fires on every session and would otherwise reprint the same
/// long-lived third-party daemons forever until someone disables the hook. A
/// known finding is not news; an unknown one is.
///
/// A broken scan WITHHELD its findings, so recording them as reported would
/// silence them on the next healthy sweep: leave the file alone and answer
/// loud. An unreadable or unwritable file also means "everything is new",
/// which is the loud direction. The file splits on LINES, never whitespace: a
/// key carries argv[0], which on macOS is routinely `Foo Helper`.
pub fn filter_new(result: &ScanResult, seen_path: &Path) -> bool {
    if result.broken() {
        return true;
    }
    let keys: std::collections::BTreeSet<String> = result.findings.iter().map(seen_key).collect();
    let seen: HashSet<String> = std::fs::read_to_string(seen_path)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default();
    if let Some(parent) = seen_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        seen_path,
        keys.iter().cloned().collect::<Vec<_>>().join("\n"),
    );
    !keys.iter().all(|k| seen.contains(k))
}

/// The same content as the text render, for the machine reader.
pub fn to_json(result: &ScanResult) -> Value {
    json!({
        "scan": {
            "total": result.total,
            "orphaned": result.orphaned,
            "same_uid": result.same_uid,
            "census_excluded": result.census_excluded,
            "daemons_excluded": result.daemons_excluded,
        },
        "control": result.control,
        "broken": result.broken(),
        "broken_reason": result.broken_reason,
        "orphans": if result.broken() { Value::Null } else {
            json!(result.findings.iter().map(|f| json!({
                "pid": f.pid,
                "name": f.name,
                "cmdline_launch_time": f.cmdline,
                "cwd": f.cwd,
                "arm": f.arm,
                "avg_cpu_percent": round_to(f.avg_cpu_percent(), 1),
                "core_hours": round_to(f.core_hours(), 3),
                "age_seconds": f.age_seconds.round() as i64,
            })).collect::<Vec<_>>())
        },
        "reaped": result.reaped.iter().map(|f| f.pid).collect::<Vec<_>>(),
    })
}

fn round_to(value: f64, places: u32) -> f64 {
    let factor = 10f64.powi(places as i32);
    (value * factor).round() / factor
}

/// The `fno-agents census --orphans` door. Hidden, like the verb it serves:
/// `fno agents orphans [--reap] [--quiet-unless-new] [--json]` routes here.
/// Exit 2 on a broken scan, WITHOUT an orphan count: a clean machine and a
/// half-blind instrument must never print the same line.
pub fn run(args: &[String]) -> i32 {
    let mut reap = false;
    let mut quiet = false;
    let mut as_json = false;
    for arg in args {
        match arg.as_str() {
            "--reap" => reap = true,
            "--quiet-unless-new" => quiet = true,
            "--json" | "-J" => as_json = true,
            other => {
                eprintln!("fno agents orphans: unknown flag {other}");
                return 2;
            }
        }
    }
    let skip = std::env::var("FNO_ORPHANS_SKIP_PROBE")
        .ok()
        .filter(|s| !s.is_empty());
    let result = scan(&Live, reap, skip.as_deref());
    let mut speak = true;
    if quiet {
        // A broken scan always speaks: silence there is the exact failure
        // this command exists to make impossible. The recording pass runs
        // FIRST, never behind a short-circuiting or: recording this scan's
        // findings is its side effect, and skipping it on a reaping run left
        // the seen-file stale after exactly the most interesting sweep. A
        // reap ALWAYS speaks too: a finding reported at 8 minutes is no
        // longer new when the next sweep kills it past the age gate, and
        // that path signalled a process and printed nothing.
        let is_new = filter_new(&result, &seen_path());
        speak = result.broken() || !result.reaped.is_empty() || is_new;
    }
    if speak {
        let payload = if as_json {
            serde_json::to_string_pretty(&to_json(&result)).unwrap_or_else(|_| "{}".into())
        } else {
            render(&result)
        };
        println!("{payload}");
    }
    if result.broken() {
        return 2;
    }
    0
}

// ─── the live machine reads ─────────────────────────────────────────────────

/// Paths whose descendants are attributable to this project.
///
/// The canonical checkout covers `.claude/worktrees/*` because those live
/// inside it. `git worktree list` adds the rest: an externally-based worktree
/// under `config.paths.worktrees_base` is beneath neither the checkout nor a
/// literal `.claude/worktrees/` path, so a path test alone reported a clean
/// machine on those projects.
fn repo_roots() -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    if let Some(common) = crate::git_paths::common_dir(
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    ) {
        if let Some(parent) = common.parent() {
            roots.push(parent.display().to_string());
        }
    }
    let cmd = {
        let mut c = std::process::Command::new("git");
        c.args(["worktree", "list", "--porcelain"]);
        c
    };
    if let Ok(listing) = crate::bounded_cmd::output_with_timeout_result(cmd, 5) {
        for line in String::from_utf8_lossy(&listing.stdout).lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                let path = path.trim();
                if !path.is_empty() && !roots.iter().any(|r| r == path) {
                    roots.push(path.to_string());
                }
            }
        }
    }
    roots
}

/// Start a `sleep` that is orphaned to PPID 1, and return its pid.
///
/// The parent shell exits immediately after backgrounding the child, so the
/// child is reparented exactly the way a real orphan is. `exec -a` is bash
/// only; zsh has no such flag, which is also why the guard's refusal text
/// spells out `bash -c`.
fn spawn_probe(rename_to: Option<&str>, cwd: &Path) -> Option<u32> {
    let bash = find_bash()?;
    // The child's stdout MUST be redirected, not inherited. An inherited
    // pipe stays open for the child's whole life, so a capturing read blocks
    // until the probe's own `sleep` ends, hits the bound, and reports a
    // healthy machine as scan-broken.
    let tail = format!("sleep {PROBE_LIFETIME_S} >/dev/null 2>&1 & echo $!");
    let script = match rename_to {
        Some(name) => format!("exec -a {name} {tail}"),
        None => tail,
    };
    let mut cmd = std::process::Command::new(bash);
    cmd.arg("-c").arg(script).current_dir(cwd);
    let out = crate::bounded_cmd::output_with_timeout_result(cmd, 10).ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<u32>()
            .ok()
    })?
}

fn find_bash() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("bash"))
        .find(|p| p.is_file())
}

/// The pid an orphan reparents to on this host, measured not assumed: poll
/// the probe's ppid until one answer repeats, which means the parent shell is
/// gone and the kernel has settled the child somewhere. Returns the reaper's
/// pid, or None when the probe never settled.
///
/// `_spawn_probe` already waits for the parent shell to exit, so the probe is
/// reparented by the time we hold its pid. Reparenting is still not
/// instantaneous, hence waiting for the same answer twice rather than
/// trusting the first read.
fn await_orphaned(pid: Option<u32>, timeout: std::time::Duration) -> Option<u32> {
    let pid = pid?;
    let deadline = std::time::Instant::now() + timeout;
    let mut last: Option<u32> = None;
    while std::time::Instant::now() < deadline {
        match ppid_of(pid) {
            None => return None,
            Some(ppid) => {
                if ppid != 0 && ppid != std::process::id() && last == Some(ppid) {
                    return Some(ppid);
                }
                last = Some(ppid);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

/// The ppid of one pid. macOS delegates to the census table's one-pid
/// probe; Linux reads `/proc` directly, because the poll loop below ticks
/// every 50ms and a `ps` exec per tick would spend the whole await budget
/// forking.
#[cfg(target_os = "macos")]
fn ppid_of(pid: u32) -> Option<u32> {
    crate::census::parent_pid(pid)
}

#[cfg(target_os = "linux")]
fn ppid_of(pid: u32) -> Option<u32> {
    proc_stat_fields(pid).map(|(_, ppid, _, _, _)| ppid)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn ppid_of(_pid: u32) -> Option<u32> {
    None
}

/// True when the pid still exists. EPERM means the process is there and this
/// uid cannot signal it, which is ALIVE. Reading a bare error as death makes
/// both the kill control and the reap re-check answer "killed" for a process
/// still running.
fn pid_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// SIGTERM, then SIGKILL if it is still there. Shared by the reap and by
/// probe cleanup, so every run exercises the kill path `--reap` uses.
///
/// Returns whether OUR signal reached the process. A pid that exited on its
/// own between the scan and the reap must not hand the caller a "reaped: 1"
/// receipt for a signal nobody sent, so the caller re-checks liveness too.
fn kill_proc(pid: u32) -> bool {
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        return false;
    }
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !pid_alive(pid) {
            return true;
        }
    }
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } != 0 {
        // Our SIGTERM landed; only the follow-up failed.
        return true;
    }
    // SIGKILL is not instantaneous. The Python leg once reported `kill arm
    // FAILED` microseconds before a stubborn probe died, because the caller's
    // liveness re-check raced this window; the grace here closes that.
    for _ in 0..20 {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    true
}

/// Every process this uid can read, cwd read only for reaper children.
///
/// The orphan sweep and the keeper lane enumerate through ONE implementation
/// (the census table walk), which is how two sweeps avoid drifting apart.
#[cfg(target_os = "macos")]
pub fn read_rows(reapers: &HashSet<u32>) -> Vec<Row> {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0u32; count as usize + 64];
    let filled = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast::<libc::c_void>(),
            (pids.len() * std::mem::size_of::<u32>()) as libc::c_int,
        )
    };
    if filled <= 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for &pid in &pids[..filled as usize] {
        if pid == 0 {
            continue;
        }
        let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let bsd_size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut bsd as *mut _ as *mut libc::c_void,
                bsd_size,
            )
        };
        // A short write is a pid that exited mid-walk or a kernel task:
        // neither belongs in the table.
        if written != bsd_size {
            continue;
        }
        let zombie = bsd.pbi_status == libc::SZOMB;
        // Lifetime CPU comes off the task read; a zombie's task read is a
        // zero write, and a zombie has burned no more CPU anyway.
        let mut cpu_seconds = 0.0;
        if !zombie {
            let mut task: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
            let task_size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
            let got = unsafe {
                libc::proc_pidinfo(
                    pid as libc::c_int,
                    libc::PROC_PIDTASKINFO,
                    0,
                    &mut task as *mut _ as *mut libc::c_void,
                    task_size,
                )
            };
            if got == task_size {
                // The totals are nanoseconds of user+system time over the
                // process's whole life.
                cpu_seconds = (task.pti_total_user + task.pti_total_system) as f64 / 1e9;
            }
        }
        // cwd() is a separate syscall and the common failure (a process this
        // uid cannot inspect) must not drop the row: a process with no
        // readable cwd can still match the NAME arm.
        let cwd = if reapers.contains(&bsd.pbi_ppid) {
            read_cwd(pid)
        } else {
            None
        };
        rows.push(Row {
            pid,
            ppid: bsd.pbi_ppid,
            uid: Some(bsd.pbi_uid),
            start_time: bsd.pbi_start_tvsec as f64,
            cpu_seconds,
            exe_name: crate::census::comm_string(&bsd.pbi_comm),
            argv: crate::census::argv_of(pid).unwrap_or_default(),
            cwd,
        });
    }
    rows
}

/// The Linux leg: /proc reads of the same fields, no `ps` text stream.
#[cfg(target_os = "linux")]
pub fn read_rows(reapers: &HashSet<u32>) -> Vec<Row> {
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let boot_epoch = boot_time_epoch_s();
    let mut rows = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return rows;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == 0 {
            // The swapper has no stat the parser can trust. This process's
            // own row stays: the scan excludes it by pid, the same arm the
            // Python leg used, so the census-excluded count reads the same.
            continue;
        }
        let Some((_state, ppid, utime, stime, starttime)) = proc_stat_fields(pid) else {
            continue;
        };
        let uid = proc_uid(pid);
        let argv: Vec<String> = std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .map(|text| {
                text.split('\0')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let exe_name = argv
            .first()
            .map(|a| a.rsplit('/').next().unwrap_or(a).to_string())
            .unwrap_or_else(|| format!("[{pid}]"));
        let cwd = if reapers.contains(&ppid) {
            std::fs::read_link(format!("/proc/{pid}/cwd"))
                .ok()
                .map(|p| p.display().to_string())
        } else {
            None
        };
        rows.push(Row {
            pid,
            ppid,
            uid,
            start_time: boot_epoch + starttime as f64 / clk_tck,
            cpu_seconds: (utime + stime) as f64 / clk_tck,
            exe_name,
            argv,
            cwd,
        });
    }
    rows
}

/// No table on an unsupported target: the sweep reads an empty machine and
/// the control gate withholds the verdict, which is the honest dead-instrument
/// answer there.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn read_rows(_reapers: &HashSet<u32>) -> Vec<Row> {
    Vec::new()
}

#[cfg(target_os = "linux")]
/// `(state, ppid, utime, stime, starttime)` from `/proc/<pid>/stat`, parsed
/// AFTER the comm field's closing paren so a `)` inside a process name cannot
/// shift the fields.
fn proc_stat_fields(pid: u32) -> Option<(char, u32, u64, u64, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = text.rsplit_once(')')?.1.trim_start();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // rest[0] is state (overall field 3); ppid is rest[1], utime rest[11],
    // stime rest[12], starttime rest[19].
    Some((
        fields.first()?.chars().next()?,
        fields.get(1)?.parse().ok()?,
        fields.get(11)?.parse().ok()?,
        fields.get(12)?.parse().ok()?,
        fields.get(19)?.parse().ok()?,
    ))
}

#[cfg(target_os = "linux")]
fn proc_uid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn boot_time_epoch_s() -> f64 {
    std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|l| {
                l.strip_prefix("btime ")
                    .and_then(|v| v.trim().parse::<f64>().ok())
            })
        })
        .unwrap_or(0.0)
}

/// The cwd of one pid through `PROC_PIDVNODEPATHINFO`, `None` when unreadable.
/// A short read is a process that exited mid-read or a pid this uid cannot
/// inspect: no cwd, never a verdict.
#[cfg(target_os = "macos")]
fn read_cwd(pid: u32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if written != size {
        return None;
    }
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flat_map(|row| row.iter())
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    if bytes.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fabricated host: the same seam set the Python suite drove through
    /// monkeypatched module functions. `probe_pids` hands out the planted
    /// control pids in order, so a test's table can carry rows for them and
    /// every test runs through the SAME control gate the live command runs
    /// through rather than around it.
    struct Fake {
        rows: Vec<Row>,
        census: Option<HashSet<u32>>,
        roots: Vec<String>,
        probe_pids: Vec<Option<u32>>,
        reaper: Option<u32>,
        kill_ok: bool,
        alive: bool,
        now: f64,
        uid: u32,
        /// Kills `kill` was asked for, recorded instead of sent: a test suite
        /// that signals a real process is a worse bug than anything it tests.
        kill_log: std::cell::RefCell<Vec<u32>>,
        /// The queue `spawn_probe` hands out, re-armed from `probe_pids` per
        /// scan so the name probe answers first, then the cwd probe.
        scan_queue: std::cell::RefCell<Vec<Option<u32>>>,
    }

    /// (9001, 9002): ordinary live pids on a developer machine, stepped aside
    /// when the test process itself runs there.
    fn probe_pids() -> (u32, u32) {
        if std::process::id() == 9001 {
            (9101, 9102)
        } else {
            (9001, 9002)
        }
    }

    impl Default for Fake {
        fn default() -> Self {
            let (name_pid, cwd_pid) = probe_pids();
            Fake {
                rows: Vec::new(),
                census: Some(HashSet::new()),
                roots: vec!["/repo".into()],
                probe_pids: vec![Some(name_pid), Some(cwd_pid)],
                reaper: Some(1),
                kill_ok: true,
                alive: false,
                now: 1_800_000_000.0,
                uid: unsafe { libc::getuid() },
                kill_log: std::cell::RefCell::new(Vec::new()),
                scan_queue: std::cell::RefCell::new(vec![Some(name_pid), Some(cwd_pid)]),
            }
        }
    }

    /// One fabricated row with the test suite's defaults: the RUNNING uid
    /// (a hardcoded 501 passes on a mac and matches nothing on a CI runner,
    /// so every finding vanishes and the file goes green-on-nothing), an
    /// explicit start time pinning the process identity across scans, and an
    /// age of 60s.
    fn proc_row(
        pid: u32,
        argv0: &str,
        exe: &str,
        cwd: Option<&str>,
        uid: u32,
        start: f64,
        cpu: f64,
    ) -> Row {
        Row {
            pid,
            ppid: 1,
            uid: Some(uid),
            start_time: start,
            cpu_seconds: cpu,
            exe_name: exe.into(),
            argv: vec![argv0.into(), "30".into()],
            cwd: cwd.map(str::to_string),
        }
    }

    impl Fake {
        /// Insert the two control rows the declared probe pids name, so the
        /// control gate runs for real over the fabricated table.
        fn plant_controls(&mut self) {
            let (name_pid, cwd_pid) = probe_pids();
            self.rows.insert(
                0,
                proc_row(
                    name_pid,
                    "fno-orphan-probe-test",
                    "sleep",
                    Some("/tmp/x"),
                    self.uid,
                    self.now - 1.0,
                    0.0,
                ),
            );
            self.rows.insert(
                1,
                proc_row(
                    cwd_pid,
                    "orphan-probe-cwd-test",
                    "sleep",
                    Some("/repo"),
                    self.uid,
                    self.now - 1.0,
                    0.0,
                ),
            );
        }

        fn reaped_kills(&self) -> Vec<u32> {
            let (name_pid, cwd_pid) = probe_pids();
            self.kill_log
                .borrow()
                .iter()
                .copied()
                .filter(|p| *p != name_pid && *p != cwd_pid)
                .collect()
        }
    }

    impl SweepHost for Fake {
        fn repo_roots(&self) -> Vec<String> {
            self.roots.clone()
        }
        fn spawn_probe(&self, _rename: Option<&str>, _cwd: &Path) -> Option<u32> {
            let mut queue = self.scan_queue.borrow_mut();
            if queue.is_empty() {
                None
            } else {
                queue.remove(0)
            }
        }
        fn await_orphaned(&self, _pid: Option<u32>) -> Option<u32> {
            self.reaper
        }
        fn census_pids(&self) -> Option<HashSet<u32>> {
            self.census.clone()
        }
        fn read_rows(&self, _reapers: &HashSet<u32>) -> Vec<Row> {
            self.rows.clone()
        }
        fn pid_alive(&self, _pid: u32) -> bool {
            self.alive
        }
        fn kill(&self, pid: u32) -> bool {
            self.kill_log.borrow_mut().push(pid);
            self.kill_ok
        }
        fn now(&self) -> f64 {
            self.now
        }
        fn uid(&self) -> u32 {
            self.uid
        }
        fn scratch(&self) -> PathBuf {
            std::env::temp_dir()
        }
    }

    impl Fake {
        /// Re-arm the probe queue each scan so the name probe answers first,
        /// then the cwd probe, exactly as the live host plants them.
        fn scan(&self, reap: bool, skip: Option<&str>) -> ScanResult {
            *self.scan_queue.borrow_mut() = self.probe_pids.clone();
            scan(self, reap, skip)
        }
    }

    const START: f64 = 1_800_000_000.0 - 3600.0;

    #[test]
    fn a_real_fno_binary_is_not_claimed_by_the_name_arm() {
        // `fno-agents-daemon` runs at PPID 1 in this repo right now. A bare
        // fno-prefix test claims it, and `--reap` then kills the daemon.
        // Requiring a genuine rename is what stops that.
        let daemon = proc_row(
            2,
            "/Users/x/.cargo/bin/fno-agents-daemon",
            "fno-agents-daemon",
            Some("/elsewhere"),
            501,
            START,
            0.0,
        );
        assert!(!daemon.was_renamed());
        assert_eq!(attribute(&daemon, &["/repo".to_string()]), None);
    }

    #[test]
    fn display_name_reads_argv0_not_the_executable() {
        // The loadgen shape contract: the emitted name lands in argv[0], the
        // kernel name stays `yes`, and the rename is what the NAME arm reads.
        let row = proc_row(1, "fno-load-lgtest-1", "yes", None, 501, START, 0.0);
        assert_eq!(row.display_name(), "fno-load-lgtest-1");
        assert!(row.was_renamed());
        let unnamed = Row {
            argv: vec![],
            exe_name: "sleep".into(),
            ..proc_row(1, "x", "sleep", None, 501, START, 0.0)
        };
        assert_eq!(unnamed.display_name(), "sleep");
    }

    #[test]
    fn a_probe_that_never_reparents_withholds_the_verdict() {
        // With no reaper there is nothing to compare a ppid against. Counting
        // against a guessed reaper is the absence trap; say the reaper is
        // unknown instead.
        let mut fake = Fake::default();
        fake.probe_pids = vec![Some(9001), None];
        fake.reaper = None;
        let result = fake.scan(false, None);
        assert!(result.broken());
        assert!(result
            .broken_reason
            .as_deref()
            .unwrap_or_default()
            .contains("reaper is unknown"));
        assert!(result.findings.is_empty());
    }

    #[test]
    fn an_out_of_enum_skip_probe_refuses() {
        let fake = Fake::default();
        let result = fake.scan(false, Some("NAME"));
        assert!(result.broken());
        assert!(result
            .broken_reason
            .as_deref()
            .unwrap_or_default()
            .contains("not 'name' or 'cwd'"));
        assert!(result.findings.is_empty());
    }

    #[test]
    fn outside_a_repo_the_sweep_withholds_rather_than_claim_cwd() {
        let mut fake = Fake::default();
        fake.roots = Vec::new();
        let result = fake.scan(false, None);
        assert!(result.broken());
        assert!(result
            .broken_reason
            .as_deref()
            .unwrap_or_default()
            .contains("no git root"));
        assert!(result.findings.is_empty());
        assert!(render(&result).contains("verdict withheld (scan-broken)"));
    }

    #[test]
    fn a_probe_that_died_on_its_own_does_not_certify_the_kill_path() {
        // The control must prove WE ended the probe, not that it is gone.
        let mut fake = Fake::default();
        fake.kill_ok = false;
        fake.alive = false;
        fake.plant_controls();
        let result = fake.scan(false, None);
        assert!(
            result.broken(),
            "a kill nobody made cannot certify the kill path"
        );
        assert!(result
            .broken_reason
            .as_deref()
            .unwrap_or_default()
            .contains("kill arm FAILED"));
        assert!(render(&result).contains("verdict withheld (scan-broken)"));
    }

    #[test]
    fn our_own_daemon_is_counted_not_listed() {
        // PPID 1 is this daemon's normal state, so its presence carries no
        // signal. Counted instead, never dropped in silence.
        let mut fake = Fake::default();
        fake.rows.push(proc_row(
            42,
            "/Users/x/.cargo/bin/fno-agents-daemon",
            "fno-agents-daemon",
            Some("/repo"),
            fake.uid,
            START,
            0.0,
        ));
        fake.plant_controls();
        let result = fake.scan(true, None);
        assert!(result.findings.is_empty());
        assert_eq!(result.daemons_excluded, 1);
        assert!(render(&result).contains("1 own-daemon"));
        assert!(result.reaped.is_empty());
    }

    #[test]
    fn cwd_arm_claims_an_ordinary_name_under_the_repo() {
        let mut fake = Fake::default();
        fake.rows.push(proc_row(
            50,
            "grep",
            "grep",
            Some("/repo/.claude/worktrees/wt-9f21"),
            fake.uid,
            START,
            0.0,
        ));
        fake.plant_controls();
        let result = fake.scan(false, None);
        assert_eq!(
            result
                .findings
                .iter()
                .map(|f| (f.pid, f.arm))
                .collect::<Vec<_>>(),
            vec![(50, "CWD")]
        );
    }

    #[test]
    fn foreign_uid_is_skipped() {
        let mut fake = Fake::default();
        fake.rows.push(proc_row(
            53,
            "grep",
            "grep",
            Some("/repo"),
            fake.uid + 1,
            START,
            0.0,
        ));
        fake.plant_controls();
        let result = fake.scan(false, None);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn census_worker_is_skipped() {
        // A `claude --bg` worker is detached to PPID 1 with a worktree cwd.
        // Without this exclusion the first --reap takes down the fleet.
        let mut fake = Fake::default();
        fake.rows.push(proc_row(
            54,
            "claude",
            "claude",
            Some("/repo/.claude/worktrees/x-1"),
            fake.uid,
            START,
            0.0,
        ));
        let mut census = HashSet::new();
        census.insert(54);
        fake.census = Some(census);
        fake.plant_controls();
        let result = fake.scan(false, None);
        assert!(result.findings.is_empty());
        assert_eq!(result.census_excluded, 1);
    }

    #[test]
    fn missing_name_probe_withholds_the_verdict() {
        let mut fake = Fake::default();
        // The name probe plants nothing: only the cwd row exists.
        fake.rows.push(proc_row(
            60,
            "grep",
            "grep",
            Some("/repo"),
            fake.uid,
            START,
            0.0,
        ));
        let (_, cwd_pid) = probe_pids();
        fake.rows.insert(
            0,
            proc_row(
                cwd_pid,
                "orphan-probe-cwd-test",
                "sleep",
                Some("/repo"),
                fake.uid,
                START,
                0.0,
            ),
        );
        fake.probe_pids = vec![None, Some(cwd_pid)];
        let result = fake.scan(false, None);
        assert!(result.broken());
        assert!(result
            .broken_reason
            .as_deref()
            .unwrap_or_default()
            .contains("NAME"));
        let rendered = render(&result);
        assert!(rendered.contains("verdict withheld (scan-broken)"));
        assert!(!rendered.contains("orphans:"));
    }

    #[test]
    fn a_broken_scan_reaps_nothing() {
        let mut fake = Fake::default();
        // No probe rows planted and no probes planted: the control gate
        // breaks the scan, and the reapable-looking finding must survive it.
        fake.rows.push(proc_row(
            61,
            "fno-load-x1",
            "sleep",
            None,
            fake.uid,
            START,
            0.0,
        ));
        let result = fake.scan(true, None);
        assert!(result.broken());
        assert!(fake.reaped_kills().is_empty());
    }

    #[test]
    fn reap_kills_an_old_renamed_fno_process() {
        let mut fake = Fake::default();
        fake.rows.push(proc_row(
            70,
            "fno-load-x1",
            "sleep",
            None,
            fake.uid,
            START,
            0.0,
        ));
        fake.plant_controls();
        let result = fake.scan(true, None);
        assert_eq!(fake.reaped_kills(), vec![70]);
        assert_eq!(
            result.reaped.iter().map(|f| f.pid).collect::<Vec<_>>(),
            vec![70]
        );
    }

    #[test]
    fn reap_spares_a_young_one() {
        let mut fake = Fake::default();
        fake.rows.push(proc_row(
            71,
            "fno-load-x1",
            "sleep",
            None,
            fake.uid,
            fake.now - 60.0,
            0.0,
        ));
        fake.plant_controls();
        let result = fake.scan(true, None);
        assert!(fake.reaped_kills().is_empty());
        assert_eq!(
            result.findings.iter().map(|f| f.pid).collect::<Vec<_>>(),
            vec![71]
        );
    }

    #[test]
    fn reap_spares_an_unnamed_orphan() {
        // Tonight's 73 processes were named `yes`. Every orphan predating the
        // guard is unnamed, so the reap can never touch it, and that is
        // correct.
        let mut fake = Fake::default();
        fake.rows.push(proc_row(
            72,
            "yes",
            "yes",
            Some("/repo/.claude/worktrees/wt-01ae"),
            fake.uid,
            START,
            0.0,
        ));
        fake.plant_controls();
        let result = fake.scan(true, None);
        assert!(fake.reaped_kills().is_empty());
        assert_eq!(
            result.findings.iter().map(|f| f.pid).collect::<Vec<_>>(),
            vec![72]
        );
        assert!(render(&result).contains("reaped: 0"));
    }

    #[test]
    fn seen_key_ignores_age() {
        // The key must depend on the process's START time, which is fixed,
        // never on how long it has been running, which is not.
        let born = START;
        let young = Finding {
            pid: 90,
            name: "node".into(),
            exe_name: "node".into(),
            renamed: false,
            cmdline: "node".into(),
            cwd: Some("/repo".into()),
            cpu_seconds: 0.0,
            age_seconds: 600.0,
            start_time: born,
            arm: "CWD",
        };
        let old = Finding {
            age_seconds: 600.0 + 3600.0,
            ..young.clone()
        };
        assert_eq!(seen_key(&young), seen_key(&old));
    }

    #[test]
    fn filter_new_records_and_answers_loud_twice() {
        let dir = std::env::temp_dir().join(format!("fno-orphans-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let seen = dir.join(".orphan-sweep-seen");
        let mut fake = Fake::default();
        fake.rows.push(proc_row(
            90,
            "node",
            "node",
            Some("/repo"),
            fake.uid,
            START,
            0.0,
        ));
        fake.plant_controls();
        let result = fake.scan(false, None);
        assert!(filter_new(&result, &seen), "the first scan is news");
        assert!(!filter_new(&result, &seen), "the second scan is not");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_scan_answers_loud_and_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("fno-orphans-broken-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let seen = dir.join(".orphan-sweep-seen");
        let mut fake = Fake::default();
        fake.census = None;
        let result = fake.scan(true, None);
        assert!(result.broken());
        assert!(result.reaped.is_empty());
        assert!(filter_new(&result, &seen), "broken withholds: answer loud");
        assert!(!seen.exists(), "a broken scan records nothing");
        assert!(!render(&result).contains("orphans:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_census_breaks_the_scan() {
        // An empty exclusion set reads as "no live workers" and hands every
        // `claude --bg` process to a reap that SessionStart runs unattended.
        let mut fake = Fake::default();
        fake.census = None;
        fake.rows.push(proc_row(
            92,
            "claude",
            "claude",
            Some("/repo"),
            fake.uid,
            START,
            0.0,
        ));
        fake.plant_controls();
        let result = fake.scan(true, None);
        assert!(result.broken());
        assert!(result
            .broken_reason
            .as_deref()
            .unwrap_or_default()
            .contains("census"));
        assert!(result.reaped.is_empty());
        assert!(!render(&result).contains("orphans:"));
    }

    #[test]
    fn render_carries_the_count_and_the_reap_lines() {
        let mut result = ScanResult {
            total: 400,
            orphaned: 3,
            same_uid: 2,
            census_excluded: 1,
            daemons_excluded: 1,
            ..Default::default()
        };
        result.control.insert("NAME", true);
        result.control.insert("CWD", true);
        result.control.insert("kill", true);
        result.findings.push(Finding {
            pid: 70,
            name: "fno-load-x1".into(),
            exe_name: "sleep".into(),
            renamed: true,
            cmdline: "fno-load-x1 30".into(),
            cwd: None,
            cpu_seconds: 60.0,
            age_seconds: 1800.0,
            start_time: START,
            arm: "NAME",
        });
        let text = render(&result);
        assert!(text.contains(
            "scan: 400 processes, 3 reparented, 2 same-uid, 1 census-excluded, 1 own-daemon"
        ));
        assert!(text.contains("orphans: 1"));
        assert!(
            !text.contains("reaped:"),
            "a plain report prints no reaped line"
        );
        result.reap_requested = true;
        assert!(render(&result).contains("reaped: 0  (nothing both fno-named and older than 10m)"));
        result.reaped.push(result.findings[0].clone());
        assert!(render(&result).contains("reaped: 1  (fno-named, age > 10m: 70)"));
        let payload = to_json(&result);
        assert_eq!(payload["orphans"][0]["pid"], 70);
        assert_eq!(payload["reaped"][0], 70);
        assert_eq!(payload["scan"]["orphaned"], 3);
    }

    #[test]
    fn the_json_shield_withholds_the_orphan_list_when_broken() {
        let mut fake = Fake::default();
        fake.census = None;
        let result = fake.scan(false, None);
        let payload = to_json(&result);
        assert_eq!(payload["broken"], true);
        assert!(payload["orphans"].is_null());
    }

    #[test]
    fn the_reap_gate_needs_all_three_arms() {
        let born = START;
        let mk = |renamed: bool, fno_named: bool, age: f64| Finding {
            pid: 1,
            name: if fno_named {
                "fno-load-x1".into()
            } else {
                "yes".into()
            },
            exe_name: if renamed {
                "sleep".into()
            } else {
                "fno-load-x1".into()
            },
            renamed,
            cmdline: String::new(),
            cwd: None,
            cpu_seconds: 0.0,
            age_seconds: age,
            start_time: born,
            arm: "NAME",
        };
        assert!(mk(true, true, REAP_MIN_AGE_S + 1.0).reapable());
        assert!(
            !mk(true, true, REAP_MIN_AGE_S).reapable(),
            "the gate is strictly older than"
        );
        assert!(!mk(false, true, 3600.0).reapable());
        assert!(!mk(true, false, 3600.0).reapable());
    }

    #[test]
    fn attribution_splits_its_two_arms() {
        let mut renamed = proc_row(1, "fno-load-x1", "sleep", None, 501, START, 0.0);
        assert_eq!(attribute(&renamed, &["/repo".to_string()]), Some("NAME"));
        renamed.cwd = Some("/tmp/elsewhere".into());
        // Still NAME: the rename decides, the cwd is read only for the CWD
        // arm.
        assert_eq!(attribute(&renamed, &["/repo".to_string()]), Some("NAME"));
        let plain = proc_row(2, "grep", "grep", Some("/repo/sub/dir"), 501, START, 0.0);
        assert_eq!(attribute(&plain, &["/repo".to_string()]), Some("CWD"));
        let worktree = proc_row(
            3,
            "grep",
            "grep",
            Some("/x/.claude/worktrees/w-1"),
            501,
            START,
            0.0,
        );
        assert_eq!(attribute(&worktree, &[]), Some("CWD"));
        let outside = proc_row(4, "grep", "grep", Some("/opt/thing"), 501, START, 0.0);
        assert_eq!(attribute(&outside, &["/repo".to_string()]), None);
    }
}
