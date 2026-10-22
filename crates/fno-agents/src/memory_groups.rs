//! What physical memory the pane stack, the daemon, the fleet and the Claude
//! spare pool hold: `ri_phys_footprint`, never summed RSS, which counts each
//! shared page once per process. Contract: docs/architecture/machine-footprint.md.

use crate::census::ProcRow;
use serde_json::{json, Value};
use std::collections::HashSet;

/// The physical footprint of `pid` now and the kernel's lifetime peak, in
/// bytes. `None` for a pid that exited or that this user cannot read.
pub fn phys_footprint(pid: u32) -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // The prototype says `rusage_info_t *`, but the kernel copies the
        // whole struct to that address. Pass the struct itself: the address
        // of a pointer local lets 296 bytes overwrite the saved registers.
        let result = unsafe {
            libc::proc_pid_rusage(
                pid as libc::c_int,
                libc::RUSAGE_INFO_V4,
                (&mut info as *mut libc::rusage_info_v4).cast(),
            )
        };
        (result == 0).then_some((info.ri_phys_footprint, info.ri_lifetime_max_phys_footprint))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Server,
    Client,
    Keeper,
    Daemon,
    SparePool,
}

/// The group a process row belongs to, from argv[0]'s basename and argv[1].
/// Never a substring scan: a session whose prompt names `--server` is a
/// session.
pub fn classify(command: &str) -> Option<Group> {
    let argv: Vec<&str> = command.split_whitespace().collect();
    let bin = argv.first()?.rsplit('/').next().unwrap_or_default();
    let first = argv.get(1).copied();
    match bin {
        "fno" if first == Some("--server") => Some(Group::Server),
        // An attached client is bare `fno`, `fno --session <name>` or
        // `fno mux attach ...`; every other `fno` argv is a short CLI call.
        "fno"
            if first.is_none()
                || first == Some("--session")
                || (first == Some("mux") && argv.get(2) == Some(&"attach")) =>
        {
            Some(Group::Client)
        }
        "fno-agents-worker" if first == Some("--pane") => Some(Group::Keeper),
        "fno-agents-daemon" => Some(Group::Daemon),
        // The shapes `footprint.is_claude_spare_pool` counts, so the memory
        // line and the spare-pool process count name the same processes.
        "claude"
            if matches!(
                first,
                Some("bg-spare" | "bg-pty-host" | "--bg-spare" | "--bg-pty-host")
            ) =>
        {
            Some(Group::SparePool)
        }
        _ => None,
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Sum {
    procs: u64,
    bytes: u64,
    peak: u64,
    unread: u64,
}

impl Sum {
    fn add(&mut self, reading: Option<(u64, u64)>) {
        self.procs += 1;
        match reading {
            Some((now, peak)) => {
                self.bytes = self.bytes.saturating_add(now);
                self.peak = self.peak.max(peak);
            }
            None => self.unread += 1,
        }
    }

    fn mb(&self) -> f64 {
        self.bytes as f64 / 1024.0 / 1024.0
    }
}

/// The reading over `rows`, with `fleet` the pids the live sessions own.
/// `read` is the per-pid reader; tests pass a fixed one.
pub fn group_reading(
    rows: &[ProcRow],
    fleet: &HashSet<u32>,
    read: impl Fn(u32) -> Option<(u64, u64)>,
) -> Value {
    let (mut server, mut client, mut keepers) = (Sum::default(), Sum::default(), Sum::default());
    let (mut daemon, mut spare, mut fleet_sum) = (Sum::default(), Sum::default(), Sum::default());
    let mut daemon_pid = None;
    for row in rows {
        let group = classify(&row.command);
        let sum = match group {
            Some(Group::Server) => &mut server,
            Some(Group::Client) => &mut client,
            Some(Group::Keeper) => &mut keepers,
            Some(Group::Daemon) => {
                daemon_pid.get_or_insert(row.pid);
                &mut daemon
            }
            Some(Group::SparePool) => &mut spare,
            None if fleet.contains(&row.pid) => &mut fleet_sum,
            None => continue,
        };
        sum.add(read(row.pid));
    }
    let stack_mb = server.mb() + client.mb() + keepers.mb();
    let per_pane = (keepers.procs > 0).then(|| stack_mb / keepers.procs as f64);
    let unread: u64 = [server, client, keepers, daemon, spare, fleet_sum]
        .iter()
        .map(|sum| sum.unread)
        .sum();
    let mut line = format!(
        "memory (physical footprint): pane stack {stack_mb:.0} MB for {} panes{} \
         (server {:.0}, clients {:.0}, keepers {:.0}); daemon {}; fleet {:.0} MB in {} processes; \
         claude spare pool {:.0} MB in {} processes (not fleet)",
        keepers.procs,
        per_pane.map_or_else(String::new, |mb| format!(", {mb:.0} MB a pane")),
        server.mb(),
        client.mb(),
        keepers.mb(),
        if daemon.procs == 0 {
            "not running".to_string()
        } else {
            format!(
                "{:.0} MB, peak {:.0} MB",
                daemon.mb(),
                daemon.peak as f64 / 1024.0 / 1024.0
            )
        },
        fleet_sum.mb(),
        fleet_sum.procs,
        spare.mb(),
        spare.procs,
    );
    if unread > 0 {
        line.push_str(&format!("; {unread} processes unreadable, not counted"));
    }
    json!({
        "basis": "phys_footprint",
        "pane_stack": {
            "mb": stack_mb,
            "server_mb": server.mb(),
            "client_mb": client.mb(),
            "clients": client.procs,
            "keeper_mb": keepers.mb(),
            "panes": keepers.procs,
            "per_pane_mb": per_pane,
        },
        "daemon": {
            "pid": daemon_pid,
            "mb": (daemon.procs > 0).then(|| daemon.mb()),
            "peak_mb": (daemon.procs > 0).then(|| daemon.peak as f64 / 1024.0 / 1024.0),
        },
        "fleet": { "mb": fleet_sum.mb(), "procs": fleet_sum.procs },
        "spare_pool": { "mb": spare.mb(), "procs": spare.procs },
        "unread": unread,
        "line": line,
    })
}

/// The live reading: the fleet is every pid a live registry session owns.
pub fn reading(home: &crate::paths::AgentsHome, rows: &[ProcRow]) -> Value {
    reading_for(rows, &crate::session_cost::session_roots(home, rows))
}

/// The same reading over roots the caller already resolved.
pub fn reading_for(rows: &[ProcRow], roots: &[crate::session_cost::SessionRoot]) -> Value {
    if !cfg!(target_os = "macos") {
        return json!({
            "basis": "unavailable",
            "line": "memory: unavailable (physical footprint is read on macOS only)",
        });
    }
    let fleet = crate::session_cost::owned_pids(rows, roots);
    group_reading(rows, &fleet, phys_footprint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, command: &str) -> ProcRow {
        ProcRow {
            pid,
            ppid: 1,
            state: 'S',
            elapsed_s: 60,
            cpu_pct: 0.0,
            rss_kb: 0,
            command: command.into(),
        }
    }

    #[test]
    fn classify_reads_argv_zero_and_one_only() {
        let cases = [
            (
                "/Users/u/.cargo/bin/fno --server /Users/u/.fno/mux/main.sock",
                Some(Group::Server),
            ),
            ("fno", Some(Group::Client)),
            ("fno --session main", Some(Group::Client)),
            ("fno mux attach main", Some(Group::Client)),
            ("fno backlog get x-1", None),
            (
                "/x/fno-agents-worker --pane --sock s --pane-key 1",
                Some(Group::Keeper),
            ),
            ("/x/fno-agents-worker --graph", None),
            ("/x/fno-agents-daemon --home /h", Some(Group::Daemon)),
            ("/x/claude bg-spare --x", Some(Group::SparePool)),
            ("/x/claude --bg-pty-host", Some(Group::SparePool)),
            (
                "/x/claude --resume s -p 'start --server and bg-spare'",
                None,
            ),
        ];
        for (command, want) in cases {
            assert_eq!(classify(command), want, "{command}");
        }
    }

    #[test]
    fn groups_sum_physical_bytes_and_split_the_pane_stack_per_pane() {
        const MB: u64 = 1024 * 1024;
        let rows = vec![
            row(10, "fno --server /s"),
            row(11, "fno"),
            row(20, "fno-agents-worker --pane --sock a"),
            row(21, "fno-agents-worker --pane --sock b"),
            row(30, "fno-agents-daemon --home /h"),
            row(40, "claude bg-spare"),
            row(50, "claude --resume s"),
            row(51, "node mcp-server"),
            row(60, "unrelated"),
        ];
        let fleet: HashSet<u32> = [50, 51].into_iter().collect();
        let read = |pid: u32| match pid {
            10 => Some((100 * MB, 100 * MB)),
            11 => Some((40 * MB, 40 * MB)),
            20 | 21 => Some((10 * MB, 10 * MB)),
            30 => Some((140 * MB, 1000 * MB)),
            40 => Some((300 * MB, 300 * MB)),
            50 => Some((500 * MB, 500 * MB)),
            51 => None,
            _ => Some((999 * MB, 999 * MB)),
        };
        let out = group_reading(&rows, &fleet, read);
        assert_eq!(out["pane_stack"]["mb"], 160.0);
        assert_eq!(out["pane_stack"]["panes"], 2);
        assert_eq!(out["pane_stack"]["per_pane_mb"], 80.0);
        assert_eq!(out["daemon"]["pid"], 30);
        assert_eq!(out["daemon"]["peak_mb"], 1000.0);
        assert_eq!(out["fleet"]["mb"], 500.0);
        assert_eq!(out["fleet"]["procs"], 2);
        assert_eq!(out["spare_pool"]["mb"], 300.0);
        assert_eq!(out["unread"], 1);
        let line = out["line"].as_str().unwrap();
        assert!(
            line.contains("pane stack 160 MB for 2 panes, 80 MB a pane"),
            "{line}"
        );
        assert!(line.contains("daemon 140 MB, peak 1000 MB"), "{line}");
        assert!(line.contains("1 processes unreadable"), "{line}");
    }
}
