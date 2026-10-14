//! A foreground harness typed into an fno pane carries proof of which pane it
//! runs in, so the session report can bind the registry row there (x-763c).
//!
//! `FNO_PANE` is exported by every inline pane (`pty.rs`) and every
//! keeper-hosted one (`pane_keeper.rs`), but it is also inherited by anything
//! THOSE processes spawn, including a long-lived daemon that was merely
//! BORN inside a pane once. [`from_parts`] is the one gate that tells "this
//! process is a pane-hosted harness session" apart from "this process just
//! happens to have inherited a stale pane env": the env must be numeric and
//! present, a harness must be provably nearest on the process tree (never
//! claude pool machinery - [`crate::spawn_context::session_identity_from_table`]
//! already refuses that), and that harness's own parent must be a real
//! process, not a detached shared host (ppid 0 or 1) and not itself nested
//! inside another harness.

use crate::census::ProcRow;
use crate::spawn_context;
use std::collections::BTreeMap;

/// Mirrors `spawn_context::MAX_ANCESTRY_DEPTH` (private there); both walks
/// bound the same live census, so a change to one is a change to the other.
const MAX_ANCESTRY_DEPTH: usize = 25;

/// A proven pane claim: the mux session name and pane id a foreground
/// harness is running under right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneClaim {
    pub session: String,
    pub pane_id: u64,
}

/// True when some process strictly above `parent_pid` (its parent, its
/// parent's parent, ...) is itself a harness: the session this claim would
/// name is nested inside another harness's pane turn (an agent's Bash tool
/// running `claude -p ...` in the same pane), so the OUTER session owns the
/// pane, not this one.
fn nested_in_another_harness(table: &BTreeMap<u32, ProcRow>, parent_pid: u32) -> bool {
    let mut pid = parent_pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        let Some(row) = table.get(&pid) else {
            return false;
        };
        let ppid = row.ppid;
        if ppid == 0 || ppid == pid {
            return false;
        }
        let Some(prow) = table.get(&ppid) else {
            return false;
        };
        if spawn_context::harness_name_of_command(&prow.command).is_some() {
            return true;
        }
        pid = ppid;
    }
    false
}

/// The pure decision, over an injected env getter and census table so it is
/// cheaply unit-testable: a pane claim only when every one of these holds -
/// `FNO_PANE` trims to digits only (pane 0 is valid); `FNO_SERVER`, else
/// `FNO_SESSION`, is non-empty (same precedence as the pane-host pin,
/// `hook/session_state.rs`); the ancestry walk names a harness that is not
/// claude pool machinery; walking up through any further ancestors that walk
/// also names as the SAME harness (a harness's own multi-process launch
/// chain), the topmost one's parent is a real process (not ppid 0 or 1 - a
/// detached shared host, e.g. a long-lived server merely born in a pane
/// once, is never a pane child); and no process above THAT parent is itself
/// a harness (otherwise this session is nested inside another harness's
/// pane turn).
pub fn from_parts<F: Fn(&str) -> Option<String>>(
    get: F,
    table: &BTreeMap<u32, ProcRow>,
    start_pid: u32,
) -> Option<PaneClaim> {
    let pane_raw = get("FNO_PANE")?;
    let pane_raw = pane_raw.trim();
    if pane_raw.is_empty() || !pane_raw.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let pane_id: u64 = pane_raw.parse().ok()?;

    let session = get("FNO_SERVER")
        .filter(|s| !s.trim().is_empty())
        .or_else(|| get("FNO_SESSION").filter(|s| !s.trim().is_empty()))?;

    let (harness_pid, harness) = spawn_context::session_identity_from_table(table, start_pid)?;

    // Collapse consecutive ancestors the walk also names as this SAME
    // harness: a harness's own wrapper layers are one launch, not nesting.
    let mut collapsed = harness_pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        let Some(row) = table.get(&collapsed) else {
            break;
        };
        let ppid = row.ppid;
        if ppid == 0 || ppid == collapsed {
            break;
        }
        let Some(prow) = table.get(&ppid) else {
            break;
        };
        if spawn_context::harness_name_of_command(&prow.command) == Some(harness) {
            collapsed = ppid;
            continue;
        }
        break;
    }

    let collapsed_row = table.get(&collapsed)?;
    let parent_pid = collapsed_row.ppid;
    if parent_pid == 0 || parent_pid == 1 {
        return None;
    }
    if nested_in_another_harness(table, parent_pid) {
        return None;
    }

    Some(PaneClaim {
        session: session.trim().to_string(),
        pane_id,
    })
}

/// The ambient wrapper: reads the table only after `FNO_PANE` is present, so
/// a session outside any pane pays no census cost.
pub fn ambient() -> Option<PaneClaim> {
    std::env::var("FNO_PANE")
        .ok()
        .filter(|v| !v.trim().is_empty())?;
    let table = spawn_context::ancestry_table();
    let start_pid = unsafe { libc::getppid() } as u32;
    from_parts(|k| std::env::var(k).ok(), &table, start_pid)
}

/// Binds `entries[idx]` to `claim`: sets its `mux` and clears `short_id` (the
/// one-live-ref invariant, `state::validate_single_live_ref`) in one write,
/// and clears `mux` on every OTHER row whose origin is `operator` and whose
/// mux already equals the claim - a later foreground session in the same
/// pane takes the pointer from a stale one; a spawned row's mux is never
/// touched by this. Returns `(old, new)` only when the target row's mux
/// actually changed, so a repeat report writes nothing new.
pub fn bind(
    entries: &mut [crate::state::RegistryEntry],
    idx: usize,
    claim: &PaneClaim,
) -> Option<(Option<crate::state::MuxRef>, crate::state::MuxRef)> {
    let new_mux = crate::state::MuxRef {
        session: claim.session.clone(),
        pane_id: claim.pane_id,
    };
    if entries[idx].mux.as_ref() == Some(&new_mux) {
        return None;
    }
    let old = entries[idx].mux.clone();
    for (i, entry) in entries.iter_mut().enumerate() {
        if i == idx {
            continue;
        }
        if entry.origin.as_deref() == Some("operator") && entry.mux.as_ref() == Some(&new_mux) {
            entry.mux = None;
        }
    }
    entries[idx].mux = Some(new_mux.clone());
    entries[idx].short_id.clear();
    Some((old, new_mux))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, ppid: u32, command: &str) -> ProcRow {
        ProcRow {
            pid,
            ppid,
            state: 'S',
            elapsed_s: 0,
            cpu_pct: 0.0,
            rss_kb: 0,
            command: command.to_string(),
        }
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    }

    /// AC2-HP: FNO_PANE=7, FNO_SERVER=main, table zsh -> codex -> bash ->
    /// fno-agents, from the fno-agents pid -> session main, pane 7.
    #[test]
    fn happy_path_binds_to_pane() {
        let table: BTreeMap<u32, ProcRow> = [
            row(1, 0, "/sbin/launchd"),
            row(10, 1, "/bin/zsh -il"),
            row(20, 10, "codex"),
            row(30, 20, "bash /some/hook.sh"),
            row(40, 30, "fno-agents report --kind session"),
        ]
        .into_iter()
        .map(|r| (r.pid, r))
        .collect();
        let get = env(&[("FNO_PANE", "7"), ("FNO_SERVER", "main")]);
        let claim = from_parts(get, &table, 40).expect("must bind");
        assert_eq!(claim.session, "main");
        assert_eq!(claim.pane_id, 7);
    }

    /// AC2-ERR (nested): the harness sits under another claude
    /// (claude -> zsh -> claude -> bash -> fno-agents) -> None.
    #[test]
    fn nested_harness_refuses() {
        let table: BTreeMap<u32, ProcRow> = [
            row(1, 0, "/sbin/launchd"),
            row(5, 1, "/Users/x/.local/bin/claude"),
            row(10, 5, "/bin/zsh -il"),
            row(20, 10, "/Users/x/.local/bin/claude"),
            row(30, 20, "bash /some/hook.sh"),
            row(40, 30, "fno-agents report --kind session"),
        ]
        .into_iter()
        .map(|r| (r.pid, r))
        .collect();
        let get = env(&[("FNO_PANE", "7"), ("FNO_SERVER", "main")]);
        assert_eq!(from_parts(get, &table, 40), None);
    }

    /// AC2-ERR (pool machinery): under `claude bg-pty-host` -> None, via
    /// `session_identity_from_table`'s own refusal.
    #[test]
    fn claude_pool_machinery_refuses() {
        let table: BTreeMap<u32, ProcRow> = [
            row(1, 0, "/sbin/launchd"),
            row(
                5,
                1,
                "claude bg-pty-host --bg-pty-host /tmp/x.pty.sock 200 50 -- claude --bg-spare /tmp/x.claim.sock",
            ),
            row(30, 5, "bash /some/hook.sh"),
            row(40, 30, "fno-agents report --kind session"),
        ]
        .into_iter()
        .map(|r| (r.pid, r))
        .collect();
        let get = env(&[("FNO_PANE", "7"), ("FNO_SERVER", "main")]);
        assert_eq!(from_parts(get, &table, 40), None);
    }

    /// AC2-ERR (bad pane value): FNO_PANE is non-numeric (`%7`, a uuid) ->
    /// None, never a parse.
    #[test]
    fn non_numeric_pane_refuses() {
        let table: BTreeMap<u32, ProcRow> = [
            row(1, 0, "/sbin/launchd"),
            row(10, 1, "/bin/zsh -il"),
            row(20, 10, "codex"),
            row(30, 20, "bash /some/hook.sh"),
            row(40, 30, "fno-agents report --kind session"),
        ]
        .into_iter()
        .map(|r| (r.pid, r))
        .collect();
        let get = env(&[("FNO_PANE", "%7"), ("FNO_SERVER", "main")]);
        assert_eq!(from_parts(get, &table, 40), None);

        let get_uuid = env(&[
            ("FNO_PANE", "9f1b1e4e-1111-2222-3333-444444444444"),
            ("FNO_SERVER", "main"),
        ]);
        assert_eq!(from_parts(get_uuid, &table, 40), None);
    }

    /// A detached shared host: the harness's own parent is ppid 1 (e.g. a
    /// long-lived daemon started once inside a pane) -> None.
    #[test]
    fn detached_shared_host_refuses() {
        let table: BTreeMap<u32, ProcRow> = [
            row(1, 0, "/sbin/launchd"),
            row(5, 1, "opencode.exe serve --service"),
            row(40, 5, "fno-agents report --kind session"),
        ]
        .into_iter()
        .map(|r| (r.pid, r))
        .collect();
        let get = env(&[("FNO_PANE", "74"), ("FNO_SERVER", "main")]);
        assert_eq!(from_parts(get, &table, 40), None);
    }
}
