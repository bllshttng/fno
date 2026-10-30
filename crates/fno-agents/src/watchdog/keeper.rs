//! The keeper lane: orphaned keepers, named and collected on --apply-all.
//!
//! Discovery is the process table joined through argv, never a socket walk: a
//! keeper leaks precisely when nothing owns its socket directory, so a walk
//! reports a clean machine over live processes. The probe vocabulary mirrors
//! the daemon's `KeeperProbe` and adds `absent` for a socket file already gone.
//! `silent` and `unreadable` never reap, because silence never proves death.

use crate::orphans::Row;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A keeper younger than this is left alone: a fresh keeper is a keeper.
pub const REAP_MIN_AGE_S: f64 = 600.0;
/// argv[0] basename, matched exactly, so a renamed lookalike never matches.
pub const KEEPER_BIN_NAME: &str = "fno-agents-worker";
const LANE_FLAGS: [(&str, &str); 3] = [
    ("--pane", "pane"),
    ("--keeper", "thread"),
    ("--store-keeper", "store"),
];
/// One probe's reply budget: a wedged keeper must be named inside this bound.
const PROBE_BUDGET: Duration = Duration::from_millis(750);
/// The whole sweep's probe budget. Keepers left unprobed read as a distinct
/// never-reap state, never as a death reading.
const SWEEP_BUDGET: Duration = Duration::from_secs(10);

pub const LISTENER: &str = "listener";
pub const NO_LISTENER: &str = "no_listener";
pub const ABSENT: &str = "absent";
pub const SILENT: &str = "silent";
pub const UNREADABLE: &str = "unreadable";
pub const UNPROBED: &str = "unprobed";
/// connect() failed with an errno that is not a refusal. The listener is
/// unproven, not dead: only a refused or vanished endpoint may reap.
pub const UNREACHABLE: &str = "unreachable";

pub const REAP: &str = "reap";
pub const LEAVE: &str = "leave";

/// Store keeper RSS bound in KiB (2 GiB): a fresh keeper holds the graph
/// resident (~0.8 GB); past it, growth is the leak shape.
const DEFAULT_KEEPER_RSS_KB: u64 = 2 * 1024 * 1024;

/// `FNO_STORE_KEEPER_RSS_KB` or the 2 GiB default, read at call time.
pub fn keeper_rss_bound_kb() -> u64 {
    std::env::var("FNO_STORE_KEEPER_RSS_KB")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_KEEPER_RSS_KB)
}

/// True when `graph` opens as a sqlite store with a version.
fn store_reads_sqlite(graph: &Path) -> bool {
    crate::backlog::export_status(graph).is_ok_and(|version| !version.is_empty())
}

/// One live keeper candidate read off the process table.
#[derive(Clone, Debug, PartialEq)]
pub struct KeeperObs {
    pub pid: u32,
    /// "pane" (--pane), "thread" (--keeper) or "store" (--store-keeper).
    pub lane: String,
    pub sock: Option<PathBuf>,
    pub session: Option<String>,
    pub cwd: Option<String>,
    pub age_s: f64,
    pub child_pids: Vec<u32>,
    pub sock_state: String,
    /// The registry row whose pid or keeper_child_pid matches.
    pub claimed_by: Option<String>,
    /// False when the registry could not be read whole. A failed read leaves
    /// `claimed_by` None, which must never read as unclaimed.
    pub registry_ok: bool,
    pub graph: Option<PathBuf>,
    pub rss_kb: Option<u64>,
}

/// Probe one keeper socket with the keeper's own frame codec. Connect refused is
/// `no_listener`. Every way the conversation after a connect can fail is
/// `silent`: reading it as no listener turns a wedged keeper into a death
/// certificate.
pub fn sock_state_of(sock: Option<&Path>) -> String {
    use std::os::unix::net::UnixStream;
    let Some(sock) = sock else {
        return UNREADABLE.to_string();
    };
    if !sock.exists() {
        return ABSENT.to_string();
    }
    // std has no connect timeout, and a full backlog can block connect: the
    // thread keeps a wedged socket from wedging the sweep.
    let (sender, receiver) = std::sync::mpsc::channel();
    let target = sock.to_path_buf();
    std::thread::spawn(move || {
        let _ = sender.send(UnixStream::connect(&target));
    });
    let stream = match receiver.recv_timeout(PROBE_BUDGET) {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            return match error.raw_os_error() {
                Some(libc::ECONNREFUSED) => NO_LISTENER,
                Some(libc::ENOENT) => ABSENT,
                _ => UNREACHABLE,
            }
            .to_string();
        }
        Err(_) => return UNREACHABLE.to_string(),
    };
    identify_over(stream).to_string()
}

/// One `Identify`, then the budget to answer with JSON.
fn identify_over(mut stream: std::os::unix::net::UnixStream) -> &'static str {
    use crate::pane_keeper::{decode, encode, Decode, Frame};
    use std::io::{Read, Write};
    if stream.set_read_timeout(Some(PROBE_BUDGET)).is_err()
        || stream.set_write_timeout(Some(PROBE_BUDGET)).is_err()
        || stream.write_all(&encode(&Frame::Identify)).is_err()
    {
        return SILENT;
    }
    let deadline = Instant::now() + PROBE_BUDGET;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    while Instant::now() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return SILENT,
            Ok(count) => buf.extend_from_slice(&chunk[..count]),
        }
        loop {
            match decode(&buf) {
                Decode::NeedMore => break,
                Decode::Violation(_) => return SILENT,
                Decode::Frame(Frame::IdentifyReply(payload), _) => {
                    return if serde_json::from_slice::<Value>(&payload).is_ok() {
                        LISTENER
                    } else {
                        SILENT
                    };
                }
                Decode::Frame(_, used) => {
                    buf.drain(..used);
                }
            }
        }
    }
    SILENT
}

/// `(verdict, reason)`, pure over one observation and the store read. REAP needs
/// a positive reading: a dead socket past grace, a keeper illegally resident on
/// a sqlite graph while config reads sqlite, or RSS over the leak bound. The
/// fail-closed arms (unreadable registry, a claimed keeper) outrank all three,
/// so claimed work is never killed.
pub fn keeper_verdict_with(
    obs: &KeeperObs,
    grace_s: Option<f64>,
    bound_kb: u64,
    store_is_sqlite: &dyn Fn(&Path) -> bool,
    read_source_is_sqlite: &dyn Fn() -> bool,
) -> (String, String) {
    let grace_s = grace_s.unwrap_or(REAP_MIN_AGE_S);
    let leave = |reason: String| (LEAVE.to_string(), reason);
    if !obs.registry_ok {
        return leave("registry unreadable, so the claim arm cannot read - no reap".to_string());
    }
    if let Some(row) = &obs.claimed_by {
        return leave(format!("registry row {row} claims this keeper"));
    }
    if let Some(rss_kb) = obs.rss_kb.filter(|rss| *rss > bound_kb) {
        let gb = rss_kb as f64 / (1024.0 * 1024.0);
        return (
            REAP.to_string(),
            format!(
                "rss {gb:.2} GB exceeds the {} MiB bound - growth with requests served is the leak shape",
                bound_kb / 1024
            ),
        );
    }
    if obs.lane == "store" {
        if let Some(graph) = &obs.graph {
            if store_is_sqlite(graph) && read_source_is_sqlite() {
                return (
                    REAP.to_string(),
                    format!(
                        "{} reads backend=sqlite while graph.read_source=sqlite - a resident keeper must not exist there (clients serve by exec)",
                        graph.display()
                    ),
                );
            }
        }
    }
    match obs.sock_state.as_str() {
        NO_LISTENER | ABSENT => {}
        SILENT => {
            return leave(
                "socket accepted and stayed silent - silence never proves death".to_string(),
            );
        }
        UNREADABLE => {
            return leave("argv declares no --sock, so the socket arm cannot read".to_string());
        }
        UNPROBED => {
            return leave("sweep probe budget spent before this keeper was probed".to_string());
        }
        UNREACHABLE => {
            return leave(
                "connect failed without refusing - the listener is unproven, not dead".to_string(),
            );
        }
        _ => {
            let sock = obs
                .sock
                .as_ref()
                .map_or("None".to_string(), |path| path.display().to_string());
            return leave(format!("socket has a live listener ({sock})"));
        }
    }
    if obs.age_s <= grace_s {
        return leave(format!(
            "age {:.0}s <= grace {grace_s:.0}s - a fresh keeper is a keeper",
            obs.age_s
        ));
    }
    (
        REAP.to_string(),
        format!(
            "socket {}, no registry row claims it, age {:.0}s > grace {grace_s:.0}s",
            obs.sock_state, obs.age_s
        ),
    )
}

/// [`keeper_verdict_with`] over the real environment and graph store.
pub fn keeper_verdict(obs: &KeeperObs, grace_s: Option<f64>) -> (String, String) {
    keeper_verdict_with(
        obs,
        grace_s,
        keeper_rss_bound_kb(),
        &store_reads_sqlite,
        &|| store_reads_sqlite(&crate::backlog::settings::graph_path()),
    )
}

/// The lane's one read: observations, a verdict per pid, and why it is broken.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeeperLaneResult {
    pub observations: Vec<KeeperObs>,
    pub verdicts: BTreeMap<u32, (String, String)>,
    pub broken_reason: Option<String>,
}

impl KeeperLaneResult {
    pub fn broken(&self) -> bool {
        self.broken_reason.is_some()
    }

    fn verdict(&self, pid: u32) -> (&str, &str) {
        self.verdicts
            .get(&pid)
            .map_or((LEAVE, ""), |(verdict, reason)| {
                (verdict.as_str(), reason.as_str())
            })
    }

    /// A broken lane reaps nothing, so a partial run's REAP verdicts made before
    /// the failure never surface as collectable.
    pub fn reapable(&self) -> Vec<&KeeperObs> {
        if self.broken() {
            return Vec::new();
        }
        self.observations
            .iter()
            .filter(|obs| self.verdict(obs.pid).0 == REAP)
            .collect()
    }

    pub fn to_json(&self) -> Value {
        let keepers: Vec<Value> = self
            .observations
            .iter()
            .map(|obs| {
                let (verdict, reason) = self.verdict(obs.pid);
                json!({
                    "pid": obs.pid,
                    "lane": obs.lane,
                    "session": obs.session,
                    "sock": obs.sock.as_ref().map(|path| path.display().to_string()),
                    "graph": obs.graph.as_ref().map(|path| path.display().to_string()),
                    "cwd": obs.cwd,
                    "age_s": obs.age_s.round_ties_even() as i64,
                    "rss_kb": obs.rss_kb,
                    "child_pids": obs.child_pids,
                    "sock_state": obs.sock_state,
                    "claimed_by": obs.claimed_by,
                    "verdict": verdict,
                    "reason": reason,
                })
            })
            .collect();
        json!({
            "broken": self.broken(),
            "broken_reason": self.broken_reason,
            "keepers": keepers,
        })
    }

    /// Every finding names its verdict, its reason, and (for a reapable one) the
    /// command that collects it.
    pub fn render(&self) -> String {
        let mut lines = vec![format!(
            "keeper lane: {} candidate(s)",
            self.observations.len()
        )];
        if let Some(reason) = &self.broken_reason {
            lines.push(format!("verdict withheld ({reason})"));
        }
        for obs in &self.observations {
            let (verdict, reason) = self.verdict(obs.pid);
            let sock = obs
                .sock
                .as_ref()
                .map_or("-".to_string(), |path| path.display().to_string());
            let rss = obs
                .rss_kb
                .map_or("-".to_string(), |kb| format!("{} MiB", kb / 1024));
            let graph = obs
                .graph
                .as_ref()
                .map_or("-".to_string(), |path| path.display().to_string());
            lines.push(format!(
                "  pid {:<7} {:<6} age {:5.1}h rss {:>9} {:<11} {:<5} {}  sock={} graph={}",
                obs.pid,
                obs.lane,
                obs.age_s / 3600.0,
                rss,
                obs.sock_state,
                verdict,
                reason,
                sock,
                graph
            ));
        }
        let reapable = self.reapable();
        if reapable.is_empty() {
            lines.push("reapable: 0".to_string());
        } else {
            let pids: Vec<String> = reapable.iter().map(|obs| obs.pid.to_string()).collect();
            lines.push(format!(
                "reapable: {}  (dry run; `fno agents watchdog --only keeper --apply-all` collects: {})",
                reapable.len(),
                pids.join(", ")
            ));
        }
        lines.join("\n")
    }

    /// Kill exactly the REAP verdicts and their hosted children. Returns
    /// `(keeper_pids, child_pids)` actually reaped.
    pub fn reap_keepers(&self) -> (Vec<u32>, Vec<u32>) {
        self.reap_with(
            &|pid: u32| {
                // A keeper is its own group leader (it calls setsid). Pid 0 and
                // pid 1 name a group this lane must never signal.
                if pid > 1 {
                    // SAFETY: a plain signal call; an error is read by the arms below.
                    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
                }
            },
            &crate::orphans::kill_proc,
            &is_dead,
        )
    }

    /// One group signal ends the keeper and everything it hosts together. Killing
    /// the children first makes the keeper exit on its own before its own signal
    /// lands, and the receipt would read 0 over a pid we did end. Any survivor
    /// falls back to the per-pid term-then-kill escalation.
    fn reap_with(
        &self,
        group_kill: &dyn Fn(u32),
        kill: &dyn Fn(u32) -> bool,
        dead: &dyn Fn(u32) -> bool,
    ) -> (Vec<u32>, Vec<u32>) {
        let mut keepers: Vec<u32> = Vec::new();
        let mut children: Vec<u32> = Vec::new();
        for obs in self.reapable() {
            group_kill(obs.pid);
            for child in &obs.child_pids {
                if dead(*child) || (kill(*child) && dead(*child)) {
                    children.push(*child);
                }
            }
            if dead(obs.pid) || (kill(obs.pid) && dead(obs.pid)) {
                keepers.push(obs.pid);
            }
        }
        (keepers, children)
    }
}

/// True when the pid is gone or a zombie. A zombie holds no pty and no socket,
/// so it is dead for every purpose this lane has. A process this uid cannot read
/// answers alive: the receipt only claims what it knows.
fn is_dead(pid: u32) -> bool {
    !crate::orphans::pid_alive(pid) || crate::census::pid_is_zombie(pid)
}

/// The registry's claim net: any pid or keeper_child_pid a row names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaimCensus {
    pub claimed: HashMap<u32, String>,
    pub registry_ok: bool,
    pub registry_error: Option<String>,
}

/// Read the claim net. A damaged registry must not read as empty, and a read
/// that dropped rows must not read as a whole census: an omitted claiming row
/// would hand a live keeper to the kill.
pub fn claim_census(registry_path: &Path) -> ClaimCensus {
    let (registry, raw_rows) = match crate::state::load_registry_with_counts(registry_path) {
        Ok(read) => read,
        Err(error) => {
            return ClaimCensus {
                claimed: HashMap::new(),
                registry_ok: false,
                registry_error: Some(error.to_string()),
            };
        }
    };
    let mut claimed: HashMap<u32, String> = HashMap::new();
    for row in &registry.entries {
        for pid in [row.pid, row.keeper_child_pid].into_iter().flatten() {
            claimed.insert(pid, row.name.clone());
        }
    }
    if registry.entries.len() != raw_rows {
        return ClaimCensus {
            claimed,
            registry_ok: false,
            registry_error: Some(
                "registry read is incomplete: row(s) were skipped this fno cannot represent, so the claim census is not whole"
                    .to_string(),
            ),
        };
    }
    ClaimCensus {
        claimed,
        registry_ok: true,
        registry_error: None,
    }
}

/// `(session, sock, graph)` off the keeper's own command line.
fn parse_argv(argv: &[String]) -> (Option<String>, Option<PathBuf>, Option<PathBuf>) {
    let mut session: Option<String> = None;
    let mut sock: Option<PathBuf> = None;
    let mut graph: Option<PathBuf> = None;
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session" => session = args.next().cloned(),
            "--sock" => {
                sock = args.next().filter(|v| !v.is_empty()).map(PathBuf::from);
            }
            "--graph" => {
                graph = args.next().filter(|v| !v.is_empty()).map(PathBuf::from);
            }
            _ => {}
        }
    }
    (session, sock, graph)
}

fn keeper_lane_of(row: &Row) -> Option<&'static str> {
    let first = row.argv.first()?;
    if first.rsplit('/').next() != Some(KEEPER_BIN_NAME) {
        return None;
    }
    LANE_FLAGS
        .iter()
        .find(|(flag, _)| row.argv.iter().any(|arg| arg == flag))
        .map(|(_, lane)| *lane)
}

/// Enumerate keepers over a read table, probe their sockets, join the claim net
/// and decide. Read-only. The kill lives in `reap_keepers`, behind --apply-all.
pub fn discover_rows(
    now_s: f64,
    rows: &[Row],
    census: &ClaimCensus,
    grace_s: Option<f64>,
    probe: &mut dyn FnMut(Option<&Path>) -> String,
    rss_for: &dyn Fn(u32) -> Option<u64>,
    cwd_for: &dyn Fn(u32) -> Option<String>,
) -> KeeperLaneResult {
    let mut result = KeeperLaneResult::default();
    // Instant, never now_s: the budget bounds work time, and an injected epoch
    // must not arm or spend it.
    let probe_deadline = Instant::now() + SWEEP_BUDGET;
    for row in rows {
        let Some(lane) = keeper_lane_of(row) else {
            continue;
        };
        let (session, sock, graph) = parse_argv(&row.argv);
        let create = if row.start_time != 0.0 {
            row.start_time
        } else {
            now_s
        };
        let children: Vec<u32> = rows
            .iter()
            .filter(|other| other.ppid == row.pid && other.pid != row.pid)
            .map(|other| other.pid)
            .collect();
        let cwd = row
            .cwd
            .clone()
            .filter(|c| !c.is_empty())
            .or_else(|| cwd_for(row.pid));
        let mut claimed_by = census.claimed.get(&row.pid).cloned();
        if claimed_by.is_none() && census.registry_ok {
            claimed_by = children
                .iter()
                .filter_map(|child| census.claimed.get(child))
                .find(|name| !name.is_empty())
                .cloned();
        }
        // A cheap local check first: an absent socket needs no probe, and
        // spending the budget on it would starve keepers only a probe can read.
        let sock_state = match sock.as_deref() {
            Some(path) if !path.exists() => ABSENT.to_string(),
            declared if Instant::now() < probe_deadline => probe(declared),
            _ => UNPROBED.to_string(),
        };
        let obs = KeeperObs {
            pid: row.pid,
            lane: lane.to_string(),
            sock,
            session,
            cwd: cwd.filter(|c| !c.is_empty()),
            age_s: (now_s - create).max(0.0),
            child_pids: children,
            sock_state,
            claimed_by,
            registry_ok: census.registry_ok,
            graph,
            rss_kb: rss_for(row.pid),
        };
        result
            .verdicts
            .insert(row.pid, keeper_verdict(&obs, grace_s));
        result.observations.push(obs);
    }
    if !census.registry_ok {
        // Named once at the result level too: a run whose every verdict reads
        // "registry unreadable" must say why at the top, not once per row.
        let detail = census.registry_error.as_deref().unwrap_or("");
        result.broken_reason = Some(format!("registry unreadable: {detail}"));
    }
    result
}

fn process_cwd(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        crate::orphans::read_cwd(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .map(|path| path.display().to_string())
    }
}

/// The lane over the live machine. An empty process table is a dead instrument,
/// never a clean machine.
pub fn discover() -> KeeperLaneResult {
    let reapers: HashSet<u32> = HashSet::from([1]);
    let rows: Vec<Row> = crate::orphans::read_rows(&reapers);
    if rows.is_empty() {
        return KeeperLaneResult {
            broken_reason: Some("keeper enumeration failed: process table unreadable".to_string()),
            ..KeeperLaneResult::default()
        };
    }
    let now_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0);
    let census = claim_census(&crate::paths::AgentsHome::from_env().registry_json());
    // A row with zero resident memory is one the table could not read.
    let mut resident: HashMap<u32, u64> = HashMap::new();
    if rows.iter().any(|row| keeper_lane_of(row).is_some()) {
        for row in crate::census::process_table().0 {
            if row.rss_kb > 0 {
                resident.insert(row.pid, row.rss_kb);
            }
        }
    }
    discover_rows(
        now_s,
        &rows,
        &census,
        None,
        &mut |sock: Option<&Path>| sock_state_of(sock),
        &|pid: u32| resident.get(&pid).copied(),
        &process_cwd,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn keeper_row(pid: u32, sock: &str, start_time: f64) -> Row {
        Row {
            pid,
            ppid: 1,
            uid: None,
            start_time,
            cpu_seconds: 0.0,
            exe_name: KEEPER_BIN_NAME.to_string(),
            argv: [
                "/x/fno-agents-worker",
                "--pane",
                "--session",
                "s",
                "--sock",
                sock,
            ]
            .iter()
            .map(|arg| arg.to_string())
            .collect(),
            cwd: None,
        }
    }

    fn whole_census(claimed: &[(u32, &str)]) -> ClaimCensus {
        ClaimCensus {
            claimed: claimed.iter().map(|(p, n)| (*p, n.to_string())).collect(),
            registry_ok: true,
            registry_error: None,
        }
    }

    fn discovered(rows: &[Row], census: &ClaimCensus) -> KeeperLaneResult {
        discover_rows(
            10_000.0,
            rows,
            census,
            None,
            &mut |_: Option<&Path>| NO_LISTENER.to_string(),
            &|_: u32| None,
            &|_: u32| None,
        )
    }

    #[test]
    fn the_sweep_joins_argv_to_claims_and_a_damaged_registry_reaps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("k.sock");
        std::fs::write(&sock, "").unwrap();
        let sock = sock.to_string_lossy().into_owned();
        let mut hosted_child = keeper_row(30, &sock, 1.0);
        hosted_child.ppid = 20;
        hosted_child.argv = vec!["claude".to_string()];
        let rows = vec![
            keeper_row(10, &sock, 1.0),
            keeper_row(20, &sock, 1.0),
            hosted_child,
        ];

        let whole = discovered(&rows, &whole_census(&[(30, "row-b")]));
        assert_eq!(whole.verdicts[&10].0, REAP);
        assert_eq!(
            whole.verdicts[&20].1,
            "registry row row-b claims this keeper"
        );
        assert_eq!(whole.reapable().len(), 1);

        let damaged = ClaimCensus {
            claimed: HashMap::new(),
            registry_ok: false,
            registry_error: Some("bad".to_string()),
        };
        let broken = discovered(&rows, &damaged);
        assert_eq!(
            broken.broken_reason.as_deref(),
            Some("registry unreadable: bad")
        );
        assert!(broken.reapable().is_empty());
        let touched = RefCell::new(Vec::new());
        let receipt = broken.reap_with(
            &|pid: u32| touched.borrow_mut().push(pid),
            &|_: u32| true,
            &|_: u32| true,
        );
        assert_eq!(receipt, (vec![], vec![]));
        assert!(touched.borrow().is_empty());
    }

    #[test]
    fn the_receipt_credits_only_a_pid_that_is_dead_after_the_signal() {
        let result = KeeperLaneResult {
            observations: vec![KeeperObs {
                pid: 10,
                lane: "pane".to_string(),
                sock: None,
                session: None,
                cwd: None,
                age_s: 1.0,
                child_pids: vec![11, 12],
                sock_state: ABSENT.to_string(),
                claimed_by: None,
                registry_ok: true,
                graph: None,
                rss_kb: None,
            }],
            verdicts: BTreeMap::from([(10, (REAP.to_string(), "r".to_string()))]),
            broken_reason: None,
        };
        let receipt = result.reap_with(&|_: u32| {}, &|_: u32| true, &|pid: u32| pid != 12);
        assert_eq!(receipt, (vec![10], vec![11]));
    }

    #[test]
    fn a_probe_reads_only_a_refused_or_vanished_socket_as_dead() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(sock_state_of(None), UNREADABLE);
        let gone = dir.path().join("gone.sock");
        assert_eq!(sock_state_of(Some(gone.as_path())), ABSENT);

        let refused = dir.path().join("refused.sock");
        drop(std::os::unix::net::UnixListener::bind(&refused).unwrap());
        assert_eq!(sock_state_of(Some(refused.as_path())), NO_LISTENER);

        let quiet = dir.path().join("quiet.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&quiet).unwrap();
        assert_eq!(sock_state_of(Some(quiet.as_path())), SILENT);
    }
}
