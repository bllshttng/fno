//! The liveness rule for claimed nodes: every open node that holds a
//! `node:` or `reconcile:` claim, or reads `in_progress`, has a live run, an
//! explicit wait, or a recovery path. Anything else is Uncovered.
//!
//! A pid, a transcript age or a note is evidence, never liveness. Live needs
//! the claim to classify Live through the session witness on this machine,
//! or a fresh fleet-beacon row naming the claim's session on another one.
//!
//! The daemon arm sweeps once per [`INTERVAL`], writes the served verdict to
//! `<agents home>/node-liveness.json`, and files one `node-liveness` fleet
//! task per node Uncovered past the grace window. Under the default
//! balanced-auto restart posture, the first filing also resumes the node's
//! registry worker in place, so uncommitted work in its worktree survives.
//! The task closes the first sweep the node reads covered.
//! `fno agents status --liveness --nodes` reads the served file and probes
//! nothing.
//!
//! An `in_review` node with no claim is out of scope: a PR waiting on a
//! merge is the board's undriven-PR queue, not a lost worker.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::claims::{ClaimRecord, ClaimState};
use crate::paths::AgentsHome;

pub const INTERVAL: Duration = Duration::from_secs(300);
/// A node reads Uncovered this long before a task is filed for it.
pub const GRACE_MS: i64 = 30 * 60 * 1000;
/// An open recovery task older than this stops counting as Recovering.
pub const RECOVERY_BOUND_MS: i64 = 120 * 60 * 1000;
/// The reader refuses a verdict older than this.
pub const STALE_S: i64 = 900;
const LANE: &str = "node-liveness";
const FILE: &str = "node-liveness.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Live {
        basis: String,
    },
    Waiting {
        kind: &'static str,
        bound_ms: i64,
        owner: String,
    },
    Recovering {
        since_ms: i64,
    },
    Uncovered {
        reason: String,
    },
}

/// What one sweep knows about one node. Every fact is read before the
/// verdict runs, so the verdict itself is pure.
#[derive(Debug, Default)]
pub struct Facts {
    pub now_ms: i64,
    /// The node's `node:` claim and its classified state, when it has one.
    pub claim: Option<(ClaimRecord, ClaimState, String)>,
    pub same_machine: bool,
    /// The remote machine's beat is fresh and a session row names the
    /// claim's session.
    pub beacon_live: bool,
    /// The bound of a current `<watching>` intent of the claim's session.
    pub watch_bound_ms: Option<i64>,
    /// The `reconcile:<id>` lease: its expiry, and whether it still holds.
    pub reconcile: Option<(i64, bool)>,
    /// When the open `node-liveness` task for this node was filed.
    pub task_since_ms: Option<i64>,
}

pub fn verdict(f: &Facts) -> Verdict {
    if let Some((rec, state, basis)) = &f.claim {
        if f.same_machine && *state == ClaimState::Live {
            return Verdict::Live {
                basis: basis.clone(),
            };
        }
        if !f.same_machine && f.beacon_live {
            return Verdict::Live {
                basis: "fleet-beacon".into(),
            };
        }
        if let Some(bound) = f.watch_bound_ms.filter(|b| *b > f.now_ms) {
            return Verdict::Waiting {
                kind: "watching",
                bound_ms: bound,
                owner: rec.session_id.clone().unwrap_or_else(|| rec.holder.clone()),
            };
        }
        // This machine cannot prove a remote session alive; it can only see
        // that the remote holder keeps renewing.
        if !f.same_machine {
            if let Some(exp) = rec.expires_at.filter(|e| *e > f.now_ms) {
                return Verdict::Waiting {
                    kind: "offhost-lease",
                    bound_ms: exp,
                    owner: rec.host.clone(),
                };
            }
        }
    }
    if let Some((exp, true)) = f.reconcile {
        return Verdict::Waiting {
            kind: "reconcile-pending",
            bound_ms: exp,
            owner: "fno backlog advance".into(),
        };
    }
    if let Some(since) = f.task_since_ms {
        if f.now_ms - since < RECOVERY_BOUND_MS {
            return Verdict::Recovering { since_ms: since };
        }
    }
    let reason = match (&f.claim, f.reconcile) {
        (Some((_, _, _)), _) if !f.same_machine => "offhost-lease-expired".to_string(),
        (Some((_, state, basis)), _) => format!("claim-{}:{basis}", state_word(*state)),
        (None, Some((_, false))) => "reconcile-pending-expired".to_string(),
        (None, _) => "no-claim".to_string(),
    };
    Verdict::Uncovered { reason }
}

fn state_word(state: ClaimState) -> &'static str {
    match state {
        ClaimState::Free => "free",
        ClaimState::Live => "live",
        ClaimState::Suspect => "suspect",
        ClaimState::Stale => "stale",
    }
}

fn verdict_json(id: &str, v: &Verdict, uncovered_since: Option<i64>) -> Value {
    match v {
        Verdict::Live { basis } => json!({"id": id, "verdict": "live", "basis": basis}),
        Verdict::Waiting {
            kind,
            bound_ms,
            owner,
        } => {
            json!({"id": id, "verdict": "waiting", "kind": kind, "bound_ms": bound_ms, "owner": owner})
        }
        Verdict::Recovering { since_ms } => {
            json!({"id": id, "verdict": "recovering", "since_ms": since_ms, "owner": LANE})
        }
        Verdict::Uncovered { reason } => json!({
            "id": id, "verdict": "uncovered", "reason": reason,
            "uncovered_since_ms": uncovered_since,
        }),
    }
}

pub fn served_path(home: &AgentsHome) -> PathBuf {
    home.root().join(FILE)
}

/// When each node first read Uncovered, from the previous served file.
fn previous_uncovered(path: &Path) -> HashMap<String, i64> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(doc) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    doc["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            Some((
                n["id"].as_str()?.to_string(),
                n["uncovered_since_ms"].as_i64()?,
            ))
        })
        .collect()
}

/// What the sweep does with one verdict. Pure, so the file-once and close
/// contract is testable without a store.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    File,
    Close,
}

pub fn action(v: &Verdict, uncovered_since_ms: i64, now_ms: i64, task_open: bool) -> Action {
    match v {
        Verdict::Uncovered { .. } if now_ms - uncovered_since_ms >= GRACE_MS && !task_open => {
            Action::File
        }
        Verdict::Live { .. } | Verdict::Waiting { .. } if task_open => Action::Close,
        _ => Action::None,
    }
}

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < INTERVAL)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Err(error) = run_pass(&home) {
            eprintln!("node-liveness: {error}");
        }
    });
}

/// One sweep: read every fact once, judge every node, act, serve.
pub fn run_pass(home: &AgentsHome) -> Result<Value, String> {
    let now_ms = crate::claims::now_ms();
    let claims = crate::claims::list(None, None, true)?;
    let mut node_claims: BTreeMap<String, ClaimRecord> = BTreeMap::new();
    let mut reconcile: HashMap<String, ClaimRecord> = HashMap::new();
    for rec in claims {
        if let Some(id) = rec.key.strip_prefix("node:") {
            node_claims.insert(id.to_string(), rec);
        } else if let Some(id) = rec.key.strip_prefix("reconcile:") {
            reconcile.insert(id.to_string(), rec);
        }
    }
    let rows = crate::graph_store::read_rows_where(
        &crate::graph_get::default_graph_path(),
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "id",
                    "status",
                    "completed_at",
                    "superseded_by",
                    "deferred_at",
                    "deferred_kind",
                ]
                .map(str::to_string)
                .to_vec(),
            ),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = Vec::new();
    let mut terminal = std::collections::HashSet::new();
    for row in &rows {
        let Some(id) = crate::graph_store::entry_id(row) else {
            continue;
        };
        if crate::graph_store::is_terminal_entry(row) || row["deferred_at"].is_string() {
            terminal.insert(id.to_string());
        } else if row["status"].as_str() == Some("in_progress") {
            ids.push(id.to_string());
        }
    }
    ids.extend(node_claims.keys().cloned());
    ids.extend(reconcile.keys().cloned());
    ids.sort();
    ids.dedup();
    ids.retain(|id| !terminal.contains(id));

    let (witness, _) = crate::claim_commands::default_session_witness();
    let witness: crate::claims::SessionWitness<'_> = &witness;
    let watches = crate::watch_expiry::current_watch_bounds(home, now_ms).unwrap_or_default();
    let beacon = beacon_sessions(now_ms);
    let store = crate::provider_cap::questions_path(home);
    let open: HashMap<String, i64> = crate::fleet_task::open_tasks(&store)?
        .into_iter()
        .filter(|t| t.lane == LANE)
        .map(|t| {
            (
                t.key,
                chrono::DateTime::parse_from_rfc3339(&t.ts)
                    .map(|at| at.timestamp_millis())
                    .unwrap_or(now_ms),
            )
        })
        .collect();
    for (id, _) in open.iter().filter(|(id, _)| terminal.contains(*id)) {
        let _ = crate::fleet_task::close(&store, LANE, id, "", "node closed", LANE);
    }
    let path = served_path(home);
    let previous = previous_uncovered(&path);

    let mut nodes = Vec::new();
    let mut uncovered_without_recovery = 0;
    for id in &ids {
        let claim = node_claims.get(id).map(|rec| {
            let (state, basis) = crate::claims::classify_with_basis_and_exclusivity(
                rec,
                Some(now_ms),
                &|pid| crate::claims::probe_pid(pid),
                None,
                Some(witness),
            );
            (rec.clone(), state, basis.to_string())
        });
        let same_machine = claim.as_ref().map_or(true, |(rec, _, _)| {
            crate::claims::is_same_machine(&rec.host, rec.machine_id.as_deref())
        });
        let session = claim
            .as_ref()
            .and_then(|(rec, _, _)| rec.session_id.clone());
        let facts = Facts {
            now_ms,
            beacon_live: !same_machine && session.as_ref().is_some_and(|s| beacon.contains(s)),
            watch_bound_ms: session.as_ref().and_then(|s| watches.get(s).copied()),
            reconcile: reconcile.get(id).map(|rec| {
                let held = crate::claims::classify(rec, Some(now_ms)) != ClaimState::Stale;
                (rec.expires_at.unwrap_or(now_ms), held)
            }),
            task_since_ms: open.get(id).copied(),
            claim,
            same_machine,
        };
        let v = verdict(&facts);
        let since = matches!(v, Verdict::Uncovered { .. })
            .then(|| previous.get(id).copied().unwrap_or(now_ms));
        match action(&v, since.unwrap_or(now_ms), now_ms, open.contains_key(id)) {
            Action::File => file_recovery(home, &store, id, &v, &facts),
            Action::Close => {
                let _ = crate::fleet_task::close(&store, LANE, id, "", "node covered", LANE);
            }
            Action::None => {}
        }
        // A young task reads Recovering, so an Uncovered node past grace has
        // no recovery: none was filed yet, or the one filed outlived its bound.
        if since.is_some_and(|at| now_ms - at >= GRACE_MS) {
            uncovered_without_recovery += 1;
        }
        nodes.push(verdict_json(id, &v, since));
    }
    let doc = json!({
        "measured_at_ms": now_ms,
        "nodes": nodes,
        "uncovered_without_recovery": uncovered_without_recovery,
    });
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(doc)
}

/// Sessions a fresh remote machine reports, by session id.
fn beacon_sessions(now_ms: i64) -> std::collections::HashSet<String> {
    let Ok(Some(remote)) = crate::store_remote::configured() else {
        return Default::default();
    };
    let Ok(fleet) = crate::fleet_beacon::read(&remote) else {
        return Default::default();
    };
    let now_s = now_ms / 1000;
    let fresh: std::collections::HashSet<&str> = fleet
        .machines
        .iter()
        .filter(|m| now_s - m.beat_at <= crate::fleet_beacon::DEFAULT_FRESH_S)
        .map(|m| m.machine_id.as_str())
        .collect();
    fleet
        .sessions
        .iter()
        .filter(|s| fresh.contains(s.machine_id.as_str()))
        .flat_map(|s| [s.session.clone(), s.session_id.clone()])
        .flatten()
        .collect()
}

fn file_recovery(home: &AgentsHome, store: &Path, id: &str, v: &Verdict, f: &Facts) {
    let Verdict::Uncovered { reason } = v else {
        return;
    };
    let clear = if f.reconcile.is_some_and(|(_, held)| !held) {
        "re-run `fno backlog advance` on its merged blocker".to_string()
    } else {
        format!("/fno:target {id}")
    };
    let text = format!(
        "node-liveness: node {id} has no live run, no wait and no recovery ({reason}) \
         past the {}m grace. Clear it with {clear}.",
        GRACE_MS / 60_000
    );
    let filed = crate::fleet_task::file_once(store, LANE, id, "", &text, Some(&clear), Some(id));
    if !matches!(filed, Ok(crate::fleet_task::Filed::New(_))) || !f.same_machine {
        return;
    }
    // Balanced auto: resume the node's own worker once per filing, never on
    // another machine's claim.
    if let Some(name) = restartable_worker(home, id) {
        let row = crate::boot_revival::revive(home, &name);
        eprintln!(
            "node-liveness: {id}: restart {name}: {} ({})",
            row.outcome, row.reason
        );
    }
}

/// The node's newest registry worker with a session to resume, unless a
/// stop someone ran through fno still stands on it.
fn restartable_worker(home: &AgentsHome, id: &str) -> Option<String> {
    let registry = crate::state::load_registry(&home.registry_json()).ok()?;
    registry
        .entries
        .iter()
        .filter(|e| e.node.as_deref() == Some(id))
        .filter(|e| {
            e.harness_session_id
                .as_deref()
                .is_some_and(|s| s.len() >= 8)
        })
        .filter(|e| !(matches!(e.status, crate::AgentStatus::Exited) && e.stop.is_some()))
        .last()
        .map(|e| e.name.clone())
}

/// `fno agents status --liveness --nodes [--json]`: read the served verdict.
/// Exit 0 every node covered, 1 a node Uncovered with no open recovery,
/// 2 the verdict is missing or older than [`STALE_S`].
pub fn run_reader(rest: &[String], home: &AgentsHome) -> i32 {
    let json_out = rest.iter().any(|a| a == "--json" || a == "-J");
    let path = served_path(home);
    let doc = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let Some(mut doc) = doc else {
        eprintln!(
            "fno-agents: liveness --nodes: no verdict at {} (the daemon's node-liveness arm writes it)",
            path.display()
        );
        return 2;
    };
    reader_exit(&mut doc, crate::claims::now_ms(), json_out)
}

fn reader_exit(doc: &mut Value, now_ms: i64, json_out: bool) -> i32 {
    let age_s = (now_ms - doc["measured_at_ms"].as_i64().unwrap_or(0)) / 1000;
    doc["measured_age_s"] = json!(age_s);
    let uncovered = doc["uncovered_without_recovery"].as_i64().unwrap_or(0);
    if json_out {
        println!("{doc}");
    } else {
        println!("node liveness: measured {age_s}s ago, {uncovered} uncovered without recovery");
        for n in doc["nodes"].as_array().into_iter().flatten() {
            if n["verdict"] != "live" {
                let why = ["reason", "kind"]
                    .iter()
                    .find_map(|k| n[*k].as_str())
                    .unwrap_or("");
                println!(
                    "  {} {} {why}",
                    n["id"].as_str().unwrap_or(""),
                    n["verdict"].as_str().unwrap_or("")
                );
            }
        }
    }
    if age_s > STALE_S {
        eprintln!("fno-agents: liveness --nodes: verdict is {age_s}s old (bound {STALE_S}s)");
        return 2;
    }
    i32::from(uncovered > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(host: &str, session: &str, expires_at: i64) -> ClaimRecord {
        let mut r: ClaimRecord = serde_json::from_value(json!({
            "key": "node:x-1", "holder": "target-session:s", "acquired_at": 0,
            "pid": null, "host": host,
        }))
        .unwrap();
        r.session_id = Some(session.into());
        r.expires_at = Some(expires_at);
        r
    }

    #[test]
    fn verdict_needs_a_live_run_a_bounded_wait_or_a_young_recovery() {
        let now = 10 * RECOVERY_BOUND_MS;
        let here = |state| Facts {
            now_ms: now,
            claim: Some((rec("h", "s", now + 1), state, "pid-absent".into())),
            same_machine: true,
            ..Default::default()
        };
        assert_eq!(
            verdict(&here(ClaimState::Live)),
            Verdict::Live {
                basis: "pid-absent".into()
            }
        );
        // A dead pid alone is evidence, never liveness.
        assert_eq!(
            verdict(&here(ClaimState::Suspect)),
            Verdict::Uncovered {
                reason: "claim-suspect:pid-absent".into()
            }
        );
        let mut watching = here(ClaimState::Suspect);
        watching.watch_bound_ms = Some(now + 5);
        assert!(
            matches!(verdict(&watching), Verdict::Waiting { kind: "watching", bound_ms, .. } if bound_ms == now + 5)
        );
        watching.watch_bound_ms = Some(now - 5);
        assert!(matches!(verdict(&watching), Verdict::Uncovered { .. }));
        let mut recovering = here(ClaimState::Stale);
        recovering.task_since_ms = Some(now - 1);
        assert_eq!(
            verdict(&recovering),
            Verdict::Recovering { since_ms: now - 1 }
        );
        recovering.task_since_ms = Some(now - RECOVERY_BOUND_MS);
        assert!(matches!(verdict(&recovering), Verdict::Uncovered { .. }));
    }

    #[test]
    fn offhost_claims_read_the_beacon_then_the_lease_and_reconcile_expiry_is_loud() {
        let now = 1_000_000;
        let mut f = Facts {
            now_ms: now,
            claim: Some((
                rec("iMac", "s", now + 60_000),
                ClaimState::Suspect,
                "offhost".into(),
            )),
            same_machine: false,
            beacon_live: true,
            ..Default::default()
        };
        assert_eq!(
            verdict(&f),
            Verdict::Live {
                basis: "fleet-beacon".into()
            }
        );
        f.beacon_live = false;
        assert!(
            matches!(verdict(&f), Verdict::Waiting { kind: "offhost-lease", owner, .. } if owner == "iMac")
        );
        f.claim = Some((
            rec("iMac", "s", now - 1),
            ClaimState::Stale,
            "ttl-expired".into(),
        ));
        assert_eq!(
            verdict(&f),
            Verdict::Uncovered {
                reason: "offhost-lease-expired".into()
            }
        );
        let expired = Facts {
            now_ms: now,
            reconcile: Some((now - 1, false)),
            ..Default::default()
        };
        assert_eq!(
            verdict(&expired),
            Verdict::Uncovered {
                reason: "reconcile-pending-expired".into()
            }
        );
    }

    #[test]
    fn sweep_files_once_past_grace_closes_when_covered_and_the_reader_refuses_stale() {
        let uncovered = Verdict::Uncovered {
            reason: "no-claim".into(),
        };
        assert_eq!(action(&uncovered, 0, GRACE_MS - 1, false), Action::None);
        assert_eq!(action(&uncovered, 0, GRACE_MS, false), Action::File);
        assert_eq!(action(&uncovered, 0, GRACE_MS, true), Action::None);
        assert_eq!(
            action(
                &Verdict::Live {
                    basis: "live".into()
                },
                0,
                0,
                true
            ),
            Action::Close
        );
        let mut doc = json!({"measured_at_ms": 0, "nodes": [], "uncovered_without_recovery": 1});
        assert_eq!(reader_exit(&mut doc, 1_000, true), 1);
        doc["uncovered_without_recovery"] = json!(0);
        assert_eq!(reader_exit(&mut doc, (STALE_S + 1) * 1000, true), 2);
    }
}
