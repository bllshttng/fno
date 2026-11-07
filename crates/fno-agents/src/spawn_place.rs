//! The spawn placement read (x-8460): which host takes the next worker.
//!
//! A `--machine <host>` pin relays the spawn through the existing
//! spawn-remote relay. With no pin, the fleet map answers: each machine's
//! daemon publishes its machine_watch sample (CPU fraction and verdict)
//! and capacity state (the session cap), so the caller reads every host's
//! free session slots and load, and the host with the most headroom takes
//! the worker when it beats this machine. A stale beat, `takes-work` off,
//! a hot or runaway verdict, or an unknown cap: the host never takes one.
//! No `store.remote_url`, an unreadable map, or no better host: the spawn
//! falls through to the local gate unchanged, where a refusal names the
//! host and its load.
//!
//! A relayed spawn never re-places. The relayed words carry `--spawned-by`
//! (the cross-host parent edge), and a same-host forward rides
//! `FNO_MAIL_LOCAL_FORWARD`, so the far intercept skips and the target
//! host's own gate answers.

use crate::agents_top::LIVE_STATUSES;
use crate::fleet_beacon::{self, Machine, Session};
use crate::paths::AgentsHome;
use crate::store_remote;

/// The caller-side placement flags on the thread lane. Portal geometry
/// belongs to the caller's mux (dispatch_launch's remote-thread rule), so a
/// relayed thread spawn carries none of them and the caller places the
/// portal from its own params after the receipt.
pub const THREAD_STRIP: &[&str] = &[
    "--tab",
    "--portal",
    "--split",
    "-x",
    "--from",
    "--workspace",
    "-s",
];

/// The pane lane keeps its target-host placement (dispatch_launch's rule:
/// remote panes anchor on the target host), so only the caller's mux-session
/// anchor is stripped.
pub const PANE_STRIP: &[&str] = &["--mux-session"];

/// One host that may take the worker: its free session slots and load.
#[derive(Debug, Clone, PartialEq)]
pub struct HostPick {
    pub host: String,
    pub free: i64,
    pub cpu: Option<f64>,
}

/// The pre-fence `--machine` pin, space or `=` spelling. A duplicate or a
/// value with an `@` reads as a typo and is the caller's error.
pub fn pinned_host(args: &[String]) -> Result<Option<String>, String> {
    let mut pin: Option<String> = None;
    let mut it = args.iter();
    while let Some(word) = it.next() {
        if word == "--" {
            break;
        }
        if word == "--machine" {
            if pin.is_some() {
                return Err("spawn takes one --machine".to_string());
            }
            let value = it
                .next()
                .ok_or_else(|| "--machine needs a host".to_string())?;
            if value.is_empty() || value.contains('@') {
                return Err(format!("--machine takes a bare host, got {value:?}"));
            }
            pin = Some(value.clone());
        } else if let Some(v) = word.strip_prefix("--machine=") {
            if pin.is_some() {
                return Err("spawn takes one --machine".to_string());
            }
            if v.is_empty() || v.contains('@') {
                return Err(format!("--machine takes a bare host, got {v:?}"));
            }
            pin = Some(v.to_string());
        }
    }
    Ok(pin)
}

/// Build the relay tail. Strips the `strip` flags (each with its value,
/// space or `=` form, never past a `--` fence), then carries
/// `name_to_carry` across: a plain pre-fence `--name` extends with
/// `@host`, an `@`-bearing `--name` is left alone (it carries its own
/// address), and with no `--name` at all `--name <name>@<host>` is
/// injected at the front.
pub fn relay_args(args: &[String], host: &str, name_to_carry: &str, strip: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut name_seen = false;
    let mut it = args.iter();
    while let Some(word) = it.next() {
        if word == "--" {
            out.push(word.clone());
            out.extend(it.cloned());
            break;
        }
        let (flag, inline) = match word.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (word.clone(), None),
        };
        if strip.contains(&flag.as_str()) {
            if inline.is_none() {
                it.next();
            }
            continue;
        }
        if flag == "--name" && !name_seen {
            name_seen = true;
            let value = match inline {
                Some(v) => v,
                // A dangling bare --name rides: the gate refuses it with its
                // own voice, so the relay stays infallible.
                None => match it.next() {
                    Some(v) => v.clone(),
                    None => {
                        out.push(word.clone());
                        continue;
                    }
                },
            };
            if !value.contains('@') {
                let extended = format!("{value}@{host}");
                if inline.is_none() {
                    out.push(word.clone());
                    out.push(extended);
                } else {
                    out.push(format!("--name={extended}"));
                }
                continue;
            }
        }
        out.push(word.clone());
    }
    if !name_seen {
        let mut with_name = vec!["--name".to_string(), format!("{name_to_carry}@{host}")];
        with_name.extend(out);
        return with_name;
    }
    out
}

/// The machine's live sessions in the fleet read: rows with no status yet
/// and rows whose status is one of the census live words.
pub fn live_count(sessions: &[Session], machine_id: &str) -> usize {
    sessions
        .iter()
        .filter(|s| s.machine_id == machine_id)
        .filter(|s| {
            s.status
                .as_deref()
                .map_or(true, |st| LIVE_STATUSES.contains(&st))
        })
        .count()
}

/// Whether the machine may take a worker: fresh beat, takes-work on, verdict
/// not hot or runaway, and a capacity figure it can judge.
pub fn eligible(machine: &Machine, now: i64) -> bool {
    let fresh = now - machine.beat_at <= fleet_beacon::DEFAULT_FRESH_S;
    fresh
        && machine.takes_work
        && !matches!(machine.verdict.as_deref(), Some("hot") | Some("runaway"))
        && machine.cap.is_some()
}

/// The pin as a pick: a pinned host relays regardless of the numbers.
pub fn pinned_pick(host: String) -> HostPick {
    HostPick {
        host,
        free: 0,
        cpu: None,
    }
}

/// Whether the pin names this machine: a same-host pin never relays, the
/// local gate takes it (today's behavior, portal placement included).
pub fn same_host_pin(host: &str) -> bool {
    crate::mail::address::normalize_host(host) == crate::name_ledger::local_bus_host()
}

/// Rank the candidates by free slots, then by lower CPU; the caller relays
/// only when the winner beats the local machine's own free slots (`None`
/// never relays: an unknown local headroom keeps today's behavior).
pub fn pick<'a>(candidates: &'a [HostPick], local_free: Option<i64>) -> Option<&'a HostPick> {
    let best = candidates.iter().max_by(|a, b| {
        a.free.cmp(&b.free).then(
            a.cpu
                .partial_cmp(&b.cpu)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    let best = best?;
    match local_free {
        Some(local) if best.free > local => Some(best),
        _ => None,
    }
}

/// The fleet read behind the placement decision: this machine's own row from
/// the local sample and capacity state, every eligible remote machine ranked,
/// and the winner only when it beats local. A hot or runaway local verdict
/// reads as zero local headroom, so a cooler host takes the worker.
pub fn best_remote_host(home: &AgentsHome, now: i64) -> Result<Option<HostPick>, String> {
    let local = fleet_beacon::local_machine(home, now);
    let Some(remote) = store_remote::configured()? else {
        return Ok(None);
    };
    let fleet = fleet_beacon::read(&remote)?;
    let local_live = live_count(&fleet.sessions, &local.machine_id);
    let local_free = match local.cap {
        Some(cap) => {
            let free = cap - local_live as i64;
            if matches!(local.verdict.as_deref(), Some("hot") | Some("runaway")) {
                Some(0)
            } else {
                Some(free.max(0))
            }
        }
        None => None,
    };
    let candidates: Vec<HostPick> = fleet
        .machines
        .iter()
        .filter(|m| m.machine_id != local.machine_id)
        .filter(|m| eligible(m, now))
        .map(|m| HostPick {
            host: m.host.clone(),
            free: m.cap.unwrap_or(0) - live_count(&fleet.sessions, &m.machine_id) as i64,
            cpu: m.cpu,
        })
        .filter(|c| c.free > 0)
        .collect();
    Ok(pick(&candidates, local_free).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn pinned_host_reads_the_pin_shapes() {
        // Space and equals forms both pin; a duplicate, a missing value, an
        // empty value, and an @-bearing value are the caller's typos.
        let a = args(&["--machine", "mbp3", "fix", "it"]);
        assert_eq!(pinned_host(&a).expect("plans"), Some("mbp3".to_string()));
        let a = args(&["--machine=mbp3", "fix", "it"]);
        assert_eq!(pinned_host(&a).expect("plans"), Some("mbp3".to_string()));
        // A value with an @ is a typo, not a host.
        let a = args(&["--machine", "a@b", "hi"]);
        assert!(pinned_host(&a).is_err());
        // A duplicate pin is the caller's error.
        let a = args(&["--machine", "mbp3", "--machine", "imac2d2"]);
        assert!(pinned_host(&a).is_err());
        // The fence hides a --machine the harness owns.
        let a = args(&["hi", "--", "--machine", "mbp3"]);
        assert_eq!(pinned_host(&a).expect("plans"), None);
    }

    #[test]
    fn relay_args_carries_the_name_and_strips_the_caller_geometry() {
        // No --name: the minted name injects at the front as name@host.
        let a = args(&["fix", "it"]);
        assert_eq!(
            relay_args(&a, "mbp3", "t-abc123", THREAD_STRIP),
            args(&["--name", "t-abc123@mbp3", "fix", "it"])
        );
        // A plain --name extends with @host; a --name=q spelling extends in
        // place.
        let a = args(&["--name", "quinlan", "fix"]);
        assert_eq!(
            relay_args(&a, "mbp3", "t-abc123", THREAD_STRIP),
            args(&["--name", "quinlan@mbp3", "fix"])
        );
        let a = args(&["--name=quinlan", "fix"]);
        assert_eq!(
            relay_args(&a, "mbp3", "t-abc123", THREAD_STRIP),
            args(&["--name=quinlan@mbp3", "fix"])
        );
        // The strip list removes the caller geometry with its value, and the
        // fence hides a harness's own placement flags.
        let a = args(&[
            "--tab",
            "1",
            "--portal=2",
            "-x",
            "down",
            "--from",
            "portal 0",
            "fix",
        ]);
        assert_eq!(
            relay_args(&a, "mbp3", "t-abc", THREAD_STRIP),
            args(&["fix"])
        );
        let a = args(&["fix", "--", "--tab", "1"]);
        assert_eq!(
            relay_args(&a, "mbp3", "t-abc", THREAD_STRIP),
            args(&["--name", "t-abc@mbp3", "fix", "--", "--tab", "1"])
        );
        // A pane relay keeps its placement flags and drops the caller's
        // mux-session anchor only.
        let a = args(&[
            "--as",
            "pane",
            "--mux-session",
            "sess-1",
            "--tab",
            "1",
            "fix",
        ]);
        assert_eq!(
            relay_args(&a, "mbp3", "t-abc", PANE_STRIP),
            args(&["--as", "pane", "--tab", "1", "fix"])
        );
        // A workspace rides the caller side: stripped so the remote gate's
        // placement validation sees a bare thread, kept in the caller's
        // params for the portal placement after the receipt.
        let a = args(&["--workspace", "ops", "--portal", "1", "fix"]);
        assert_eq!(
            relay_args(&a, "mbp3", "t-abc", THREAD_STRIP),
            args(&["--name", "t-abc@mbp3", "fix"])
        );
    }

    #[test]
    fn pick_ranks_free_then_cpu_and_relays_only_over_local() {
        let mk = |host: &str, free: i64, cpu: Option<f64>| HostPick {
            host: host.to_string(),
            free,
            cpu,
        };
        let one = [mk("a", 4, Some(0.1))];
        let picked = pick(&one, Some(2)).map(|p| p.host.clone());
        assert_eq!(picked.as_deref(), Some("a"));
        let tie = [mk("a", 4, Some(0.3)), mk("b", 4, Some(0.1))];
        let picked = pick(&tie, Some(2)).map(|p| p.host.clone());
        assert_eq!(picked.as_deref(), Some("b"));
        let none_local = [mk("a", 4, Some(0.1))];
        assert!(pick(&none_local, None).is_none());
        let equal_free = [mk("a", 4, Some(0.1))];
        assert!(pick(&equal_free, Some(4)).is_none());
    }
}
