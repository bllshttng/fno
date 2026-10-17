//! `org-remote`: the cross-machine lead hand-off relay verb.
//!
//! The front door claims `agents org promote <name>@<host> --scope <scope>`
//! and execs this verb on the predecessor's machine. One ssh hop runs
//! `record-succession` ON the target host: the remote machine's own
//! `fno agents org promote` grants, then a pending succession record
//! attaches, so the successor's next beat runs verify, the release readback,
//! and the retro attempt (the same steps the spawn `--hand-off` path runs).
//! On remote success the sending machine vacates the predecessor row, files
//! announce and transferred from the successor's name, and runs the retro
//! here, where the predecessor's transcript lives. Fail-closed ordering: the
//! remote leg must exit 0 before anything local changes; an unreachable host
//! leaves the predecessor seated and no record on either machine.

use crate::team_names::{attach_pending_succession, PendingSuccession};

fn usage() -> String {
    "usage: fno agents org promote <name>@<host> --scope <scope>".into()
}

/// What the ssh leg carries and what the local legs write. Pure data, so the
/// planning is testable without a network.
pub struct HandoffPlan {
    pub host: String,
    pub ssh_argv: Vec<String>,
    pub pending: PendingSuccession,
}

/// Parse and plan the hand-off: pure, so tests cover the shapes without a
/// network. Refuses the cases the plan declares: no `@host` target, a
/// missing --scope, and a predecessor row with no session id (the retro's
/// join key). `holder` is the live scope holder as `(name, session)`.
pub fn plan_handoff(
    args: &[String],
    holder: (String, Option<String>),
    ts: &str,
) -> Result<HandoffPlan, String> {
    let Some(target) = args.first() else {
        return Err(usage());
    };
    let (successor, host) = crate::mail_remote::split_target(target)
        .map_err(|_| format!("the target is <name>@<host>: {target:?} has no @host split"))?;
    let mut scope = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--scope" => {
                i += 1;
                scope = Some(
                    args.get(i)
                        .ok_or("--scope needs a value")?
                        .trim()
                        .to_string(),
                );
                i += 1;
            }
            other => {
                return Err(format!(
                    "unexpected argument {other:?}; the form is org promote <name>@<host> --scope <scope>"
                ));
            }
        }
    }
    let scope = scope.ok_or_else(usage)?;
    if holder.1.is_none() {
        return Err(
            "the predecessor row carries no harness_session_id, so the retro join has no key; re-register the row (`fno agents register`), or move the lead with the remote machine's own `fno agents org promote` and file the retro by hand (`fno-agents intel --write`)"
                .into(),
        );
    }
    let pending = PendingSuccession {
        successor_name: successor.clone(),
        successor_session: None,
        predecessor_name: holder.0.clone(),
        predecessor_session: holder.1.clone(),
        ts: ts.to_string(),
    };
    let ssh_words: Vec<String> = vec![
        "agents".into(),
        "org-remote".into(),
        "record-succession".into(),
        "--scope".into(),
        scope.clone(),
        "--successor".into(),
        successor.clone(),
        "--predecessor".into(),
        holder.0.clone(),
        "--predecessor-session".into(),
        holder.1.clone().unwrap_or_default(),
    ];
    Ok(HandoffPlan {
        host,
        ssh_argv: crate::mail_remote::ssh_argv(&host, &ssh_words),
        pending,
    })
}

/// The live holder of the canonical scope in THIS machine's registry, as
/// `(name, session)`. `None` when no live row holds it.
fn local_holder(
    registry_path: &std::path::Path,
    scope: &str,
) -> Result<Option<(String, Option<String>)>, String> {
    let canon = crate::territory::canonical_scope(scope);
    let live = crate::team_names::live_index(registry_path)
        .map_err(|e| format!("registry unreadable: {e}"))?;
    Ok(live
        .get(&canon)
        .map(|team| (team.holder.clone(), team.holder_session.clone())))
}

/// Clear the role fields on every live row holding the canonical scope: the
/// local vacate. Under `state::update_registry`'s lock, the registry's own
/// contract.
fn vacate_scope_holder(
    registry_path: &std::path::Path,
    scope: &str,
) -> Result<Vec<String>, String> {
    let canon = crate::territory::canonical_scope(scope);
    crate::state::update_registry(registry_path, |doc| {
        let mut vacated = Vec::new();
        for row in doc.entries.iter_mut() {
            let holds = row
                .role_scope
                .as_deref()
                .map(|s| crate::territory::canonical_scope(s) == canon)
                .unwrap_or(false)
                && !crate::lead_state::is_terminal(row);
            if holds {
                row.role_scope = None;
                row.role_level = None;
                row.role_grantor = None;
                vacated.push(row.name.clone());
            }
        }
        Ok(vacated)
    })
}

/// The remote leg, run ON the target host by the ssh hop: grant through the
/// machine's own verb, then attach the pending record. Grant first, record
/// only on grant success; a grant failure refuses having written nothing,
/// and a record failure after a successful grant exits non-zero with a loud
/// warning (the sending machine then holds, both seated).
fn cmd_record_succession(args: &mut dyn Iterator<Item = String>) -> i32 {
    let mut scope = None;
    let mut successor = None;
    let mut predecessor = None;
    let mut predecessor_session = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scope" => scope = args.next(),
            "--successor" => successor = args.next(),
            "--predecessor" => predecessor = args.next(),
            "--predecessor-session" => predecessor_session = args.next(),
            other => {
                eprintln!("unexpected argument {other:?}; {}", usage());
                return 2;
            }
        }
    }
    let (Some(scope), Some(successor), Some(predecessor)) = (scope, successor, predecessor) else {
        eprintln!("{}", usage());
        return 2;
    };
    let grant_argv: Vec<OsString> = [
        "agents".to_string(),
        "org".to_string(),
        "promote".to_string(),
        successor.clone(),
        "--scope".to_string(),
        scope.clone(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let granted = std::process::Command::new(crate::mail_remote::fno_binary())
        .args(&grant_argv)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !granted {
        eprintln!(
            "org-remote: the remote grant exited non-zero; the pending record was not written"
        );
        return 1;
    }
    let pending = crate::team_names::PendingSuccession {
        successor_name: successor.clone(),
        successor_session: None,
        predecessor_name: predecessor.clone(),
        predecessor_session,
        ts: crate::daemon::now_rfc3339_like(),
    };
    let home = crate::paths::AgentsHome::from_env();
    let store = home.team_names_json();
    if attach_pending_succession(&store, &scope, pending).is_err() {
        eprintln!(
            "org-remote: the pending record attach failed after a successful grant; both machines read seated"
        );
        return 1;
    }
    println!("succession recorded over {scope}: {predecessor} -> {successor} (verify on the successor's next beat)");
    0
}

/// The relay leg, on the predecessor's machine: resolve the local holder,
/// refuse a predecessor row with no session id, run the ssh hop, and only on
/// remote success vacate, journal, and file the retro locally.
fn cmd_promote(args: &[String]) -> i32 {
    let home = crate::paths::AgentsHome::from_env();
    let registry_path = home.registry_json();
    let Some(scope) = scope_of(args) else {
        eprintln!("{}", usage());
        return 2;
    };
    let holder = match local_holder(&registry_path, scope) {
        Ok(Some(holder)) => holder,
        Ok(None) => {
            eprintln!(
                "org-remote: no live row holds {scope:?} on this machine; the hand-off refuses with the seat unchanged"
            );
            return 2;
        }
        Err(e) => {
            eprintln!("org-remote: holder readback: {e}");
            return 2;
        }
    };
    caller_check(&holder);
    let ts = crate::daemon::now_rfc3339_like();
    let plan = match plan_handoff(args, holder, &ts) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("org-remote: {e}");
            return 2;
        }
    };
    let relay_exit = std::process::Command::new("ssh")
        .args(&plan.ssh_argv)
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or(-1);
    if relay_exit != 0 {
        eprintln!(
            "org-remote: the ssh hop exited {relay_exit}; the seat is unchanged and no record landed on either machine"
        );
        return relay_exit;
    }
    match vacate_scope_holder(&registry_path, scope) {
        Ok(vacated) => {
            if vacated.is_empty() {
                eprintln!(
                    "org-remote: the local vacate cleared no row; the seat readback differs from the holder read"
                );
            }
        }
        Err(e) => {
            eprintln!(
                "org-remote: local vacate failed after a successful hand-off: {e}; announce, transferred, and the retro stay unfiled until it succeeds"
            );
            return 1;
        }
    }
    crate::succession_txn::announce(scope, &plan.pending);
    crate::succession_txn::transferred(scope, &plan.pending);
    file_retro(scope, &plan.pending);
    println!(
        "hand-off over {scope}: {pred} -> {succ}@{host}; the successor's next beat verifies and releases",
        pred = plan.pending.predecessor_name,
        succ = plan.pending.successor_name,
        host = plan.host,
    );
    0
}

/// The retro step of the hand-off, filed where the predecessor's transcript
/// lives: the same detached intel writer the successor's beat would launch,
/// named by the same succession id.
fn file_retro(scope: &str, pending: &PendingSuccession) {
    let Some(cwd) = std::env::current_dir().ok().filter(|p| p.is_dir()) else {
        return;
    };
    crate::succession_txn::retro(
        scope,
        &cwd,
        pending,
        &crate::succession_txn::succession_id(pending, scope),
    );
}

/// The `--scope` value from a hand-off argv, trimmed.
fn scope_of(args: &[String]) -> Option<&str> {
    args.iter()
        .position(|a| a == "--scope")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.trim())
}

/// Only the holder itself (its ambient session id matches the predecessor's)
/// or an attended shell (no ambient session identity) may hand the role
/// down - the same caller rule team_settle's `--hand-off` applies (read at
/// team_settle.rs:385-407 at plan time).
fn caller_check(holder: &(String, Option<String>)) {
    let get = |name: &str| std::env::var(name).ok();
    let identity = crate::spawn_context::resolve_self_identity(
        &get,
        None,
        None,
        &crate::paths::AgentsHome::from_env(),
    );
    let session = identity.session_id.as_deref().map(str::trim);
    if let Some(session) = session.filter(|s| !s.is_empty()) {
        if Some(session) != holder.1.as_deref() {
            eprintln!(
                "org-remote: session {session} is not the holder of this scope; only the holder or an attended shell hands the role down"
            );
            std::process::exit(2);
        }
    }
}

/// The verb dispatcher: `promote` (the relay, on the predecessor's machine)
/// and `record-succession` (the remote leg, on the target host).
pub fn run(args: &[String]) -> i32 {
    match args.first().map(|a| a.as_str()) {
        Some("promote") => cmd_promote(&args[1..]),
        Some("record-succession") => cmd_record_succession(&mut args[1..].iter().cloned()),
        _ => {
            eprintln!("usage: fno-agents org-remote <promote|record-succession> ...");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder() -> (String, Option<String>) {
        ("Ellery".into(), Some("sess-123".into()))
    }

    #[test]
    fn org_remote_units_guard_their_contracts() {
        // The plan refuses what it declares: no target, no @ split, no
        // scope, an unexpected argument, and a holder with no session id
        // (the retro's join key).
        assert!(plan_handoff(&[], holder(), "t0").is_err());
        assert!(plan_handoff(
            &["Quinlan".into(), "--scope".into(), "fno".into()],
            holder(),
            "t"
        )
        .is_err());
        assert!(plan_handoff(&["Quinlan@imac2d2".into()], holder(), "t").is_err());
        assert!(plan_handoff(
            &["Q@h".into(), "--scope".into(), "fno".into(), "extra".into()],
            holder(),
            "t"
        )
        .is_err());
        assert!(plan_handoff(
            &["Q@h".into(), "--scope".into(), "fno".into()],
            ("E".into(), None),
            "t"
        )
        .is_err());
        // The happy plan: the pending record carries the join keys, and the
        // ssh argv names the remote leg with every argument spelled.
        let plan = plan_handoff(
            &["Quinlan@imac2d2".into(), "--scope".into(), "fno".into()],
            holder(),
            "t9",
        )
        .expect("plans");
        assert_eq!(plan.host, "imac2d2");
        assert_eq!(plan.pending.successor_name, "Quinlan");
        assert_eq!(plan.pending.predecessor_name, "Ellery");
        assert_eq!(
            plan.pending.predecessor_session.as_deref(),
            Some("sess-123")
        );
        let quoted: Vec<String> = plan.ssh_argv.clone();
        let line = quoted.join(" ");
        assert!(line.contains("record-succession"));
        assert!(line.contains("'--scope'"));
        assert!(line.contains("'fno'"));
        assert!(line.contains("'--successor'"));
        assert!(line.contains("'Quinlan'"));
        assert!(line.contains("'sess-123'"));
        // scope_of reads the flag the relay leg resolves the holder with.
        assert_eq!(
            scope_of(&["x".into(), "--scope".into(), "fno.org".into()]),
            Some("fno.org")
        );
        assert_eq!(scope_of(&["x".into()]), None);
    }

    #[test]
    fn attach_keeps_the_name_and_joins_on_the_predecessor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = tmp.path().join("team_names.json");
        let pending = crate::team_names::PendingSuccession {
            successor_name: "Quinlan".into(),
            successor_session: None,
            predecessor_name: "Ellery".into(),
            predecessor_session: Some("sess-123".into()),
            ts: "2026-10-16T23:00:00Z".into(),
        };
        attach_pending_succession(&store, "fno", pending).expect("attaches to a fresh record");
        let raw = std::fs::read_to_string(&store).expect("store written");
        assert!(raw.contains("pending_succession"));
        assert!(raw.contains("sess-123"));
        // The record keeps the successor's existing name (no rename leg).
        let doc: serde_json::Value = serde_json::from_str(&raw).expect("valid store json");
        let rec = &doc["teams"]["fno"];
        assert_eq!(rec["name"], "Quinlan");
        assert_eq!(rec["pending_succession"]["predecessor_name"], "Ellery");
        // Overwrite-on-retry: a second attach replaces the record cleanly.
        let again = crate::team_names::PendingSuccession {
            successor_name: "Quinlan".into(),
            successor_session: None,
            predecessor_name: "Ellery".into(),
            predecessor_session: Some("sess-999".into()),
            ts: "2026-10-16T23:30:00Z".into(),
        };
        attach_pending_succession(&store, "fno", again).expect("re-attach succeeds");
        let raw2 = std::fs::read_to_string(&store).expect("store readable");
        assert!(raw2.contains("sess-999"));
    }
}
