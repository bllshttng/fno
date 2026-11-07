//! The family-2 pre-birth guard for a node-bearing spawn: the one leg of the
//! `node dispatch refused:` decision, the `dispatch:<id>` reservation and the
//! `node:<id>` handover claim. The Python door reaches it through the
//! `spawn-guard` op of `backlog advance --stdin`.
//!
//! Four steps, in this order: read the verdict (`observe_node_claim`), try
//! recovery on a suspect node claim, take the reservation under the
//! dispatcher's own holder, then take the node claim under the worker's
//! handover holder. A refusal is a JSON object, so the stderr line and the
//! `spawn-guard` JSON stay one vocabulary.
//!
//! Recovery reuses the reaper's judge. Its roster-backed abandonment arms
//! have no Rust leg yet and degrade fail-closed, so a suspect
//! `target-session:` claim the roster could prove finished still keeps
//! refusing as `suspect-claim` with its remedy line. A wedge is never
//! cleared on an unmeasured holder.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use crate::backlog::advance::{observe_node_claim, DispatchClaimObservation};
use crate::claims::{AcquireOpts, AcquireOutcome, ClaimState, BLUEPRINT_HOLDER_PREFIX};

/// Holder prefix of a reservation the spawn door writes. Recovery of a
/// `dispatch:` key is scoped to it: `fno backlog advance` reserves the same
/// key as `advance:<pid>` and spawns without `--node`, so that reservation
/// is the only barrier its booting worker has.
pub const SPAWN_CLI_HOLDER_PREFIX: &str = "spawn-cli:";
/// The launch-window holder of the node claim; the worker names it back.
pub const HANDOVER_HOLDER_PREFIX: &str = "spawn-handover:";
/// The holder `fno do target init` claims under (`truth_status._HOLDER_PREFIX`).
const SESSION_HOLDER_PREFIX: &str = "target-session:";
/// The reservation is the window for ONE process to fork a worker.
pub const RESERVATION_TTL_MS: i64 = 180_000;
/// The handover claim must outlive the launch-to-init gap, or the node reads
/// free mid-launch. An expired claim is provably dead, so a dead launch
/// self-clears.
pub const HANDOVER_TTL_MS: i64 = 900_000;
/// The env keys a node-bearing spawn sets or clears together.
pub const PROVENANCE_KEYS: [&str; 4] =
    ["FNO_NODE", "FNO_SLUG", "FNO_PLAN", "FNO_NODE_CLAIM_HOLDER"];
/// Buckets where force-release advice is honest: recovery ran, nobody was
/// found, and the claim is still there.
const REMEDIABLE_BUCKETS: [&str; 3] = ["release-failed", "suspect", "suspect_unprobed"];
/// The recovery bucket for a holder PROVED alive: benign dedup, not a wedge.
const HOLDER_ALIVE: &str = "live";
/// `fno.graph.statuses.UNMEASURABLE_LABEL_MARK`: the worked overlay's mark on
/// a row whose liveness it could not measure.
const UNMEASURABLE_LABEL_MARK: &str = "(unmeasurable:";

/// One guard call. `root` pins the claims root for the reservation half
/// (tests); production passes `None` and the keys route to the global root.
pub struct GuardRequest<'a> {
    pub node_id: &'a str,
    pub holder: &'a str,
    pub ttl_ms: i64,
    pub no_reserve: bool,
    pub cwd: Option<&'a str>,
    pub handover_holder: Option<&'a str>,
    pub root: Option<&'a Path>,
    /// The spawning process the keys name; `None` names this one. A caller
    /// that runs the guard in a subprocess passes its own pid, or the keys
    /// would name a process that exits the moment the guard answers.
    pub pid: Option<u32>,
    /// The session phase this spawn binds its worker under, when the caller
    /// resolved one. A think or blueprint phase turns on the plan-fresh
    /// gate; work phases pass `None`.
    pub phase: Option<&'a str>,
}

/// The verdict object and its exit code. Exit 0 for every answered verdict,
/// 3 for a fail-closed guard error.
pub fn decide(req: &GuardRequest) -> (Value, i32) {
    decide_with_occupancy(req, None)
}

fn decide_with_occupancy(
    req: &GuardRequest,
    node_occupancy: Option<&crate::spawn_gate::NodeOccupancy>,
) -> (Value, i32) {
    let node_key = format!("node:{}", req.node_id);
    // One read feeds the whole verdict. With `store.remote_url` set every
    // read is a round trip to the primary, and a slow primary times the
    // caller out long before a handful of repeat reads finish.
    let (state, record) = match crate::claims::status_checked(&node_key, None) {
        Ok(read) => read,
        // A row that will not decode is a corrupted claim. A primary that
        // did not answer is an outage, and repair advice would mislead.
        Err(error) if !error.starts_with("remote store ") => (ClaimState::Corrupted, None),
        Err(error) => {
            let detail = format!("{node_key} claim is unreadable: {error}; not dispatching");
            return (
                json!({"verdict": "error", "reason": "claims-unavailable", "detail": detail}),
                3,
            );
        }
    };
    if state == ClaimState::Corrupted {
        let detail = format!(
            "node:{} claim is corrupted; force-release or repair before dispatching",
            req.node_id
        );
        return (json!({"verdict": "corrupted", "detail": detail}), 0);
    }
    let mut seen = observe(
        req,
        !req.no_reserve,
        Some((state, record.clone())),
        node_occupancy,
    );
    let mut common = common_fields(req, &seen);
    if is_deferral(&seen.action) {
        return (refused(&seen.action, common), 0);
    }
    if seen.blocks_dispatch() {
        // A LIVE claim is benign dedup. A SUSPECT one is a wedge, and only
        // the wedge is worth recovering. A --no-reserve call is a probe and
        // takes no recovery: the real spawn that follows recovers or refuses.
        let recover = state == ClaimState::Suspect && !req.no_reserve;
        if recover {
            if let (Some(prior), _) = reclaim_if_provably_dead(&node_key, None) {
                emit_reaped_abandoned(req.node_id, &prior, &seen.truth_status);
                seen = observe(req, false, None, node_occupancy);
                common = common_fields(req, &seen);
                if is_deferral(&seen.action) {
                    return (refused(&seen.action, common), 0);
                }
            }
        }
        if seen.blocks_dispatch() {
            // Re-read only after recovery ran: a node re-claimed by a live
            // worker meanwhile must not render as a wedge with force-release
            // advice. With no recovery, the first read is still the truth.
            let now = if recover {
                crate::claims::status(&node_key, None).0
            } else {
                state
            };
            return (
                blocked(req, &seen, common, &node_key, now, record.as_ref()),
                0,
            );
        }
    }
    if let Some(session_phase) = req.phase {
        match plan_fresh(req.node_id, session_phase) {
            Some(PlanFresh::Refuse { detail, fields }) => {
                let mut out = Map::new();
                out.insert("verdict".into(), json!("refused"));
                out.insert("reason".into(), json!("plan-fresh"));
                for (k, v) in fields {
                    out.insert(k, json!(v));
                }
                out.insert("detail".into(), json!(detail));
                return (Value::Object(out), 0);
            }
            Some(PlanFresh::Warn(line)) => eprintln!("{line}"),
            None => {}
        }
    }
    if req.no_reserve {
        return (json!({"verdict": "dispatchable"}), 0);
    }
    reserve_and_hand_over(req)
}

fn observe(
    req: &GuardRequest,
    emit_events: bool,
    claim_read: Option<(ClaimState, Option<crate::claims::ClaimRecord>)>,
    node_occupancy: Option<&crate::spawn_gate::NodeOccupancy>,
) -> DispatchClaimObservation {
    let (worked_nodes, worked_error) = match node_occupancy {
        Some(Ok(map)) => (Some(map.clone()), None),
        Some(Err(error)) => (None, Some(error.clone())),
        None => (None, None),
    };
    observe_node_claim(
        req.node_id,
        req.cwd,
        !req.no_reserve,
        emit_events,
        claim_read,
        worked_nodes,
        worked_error,
        None,
    )
}

fn is_deferral(action: &str) -> bool {
    action == "auto-deferred" || action == "defer-failed"
}

fn common_fields(req: &GuardRequest, seen: &DispatchClaimObservation) -> Map<String, Value> {
    let mut common = Map::new();
    common.insert("holder".into(), json!(seen.holder));
    common.insert("truth_status".into(), json!(seen.truth_status));
    common.insert(
        "init_reached".into(),
        json!(init_reached(req.node_id, &seen.holder, req.cwd)),
    );
    if !seen.worker.is_empty() {
        common.insert("worker".into(), json!(seen.worker));
    }
    common
}

fn refused(action: &str, common: Map<String, Value>) -> Value {
    let mut out = Map::new();
    out.insert("verdict".into(), json!("refused"));
    out.insert("reason".into(), json!(action));
    out.extend(common);
    Value::Object(out)
}

/// The refusal for a node something already holds: a worker row, an
/// authority outage, or a claim that is live, unproven or wedged.
fn blocked(
    req: &GuardRequest,
    seen: &DispatchClaimObservation,
    common: Map<String, Value>,
    node_key: &str,
    state: ClaimState,
    record: Option<&crate::claims::ClaimRecord>,
) -> Value {
    // A launch-window holder reads SUSPECT for its whole TTL by construction
    // (it carries no pid; the TTL bounds it), so it is never a wedge.
    let in_launch_window = seen.holder.starts_with(HANDOVER_HOLDER_PREFIX);
    let wedged = state == ClaimState::Suspect && !in_launch_window;
    let block = seen.block_reason.clone().unwrap_or_default();
    let block_wins = !block.is_empty() && !block.starts_with("held:");
    let mut out = Map::new();
    if !block_wins
        && !seen.worker.is_empty()
        && seen.verdict != "ours"
        && seen.verdict != "foreign_live"
    {
        // The worker ROW is the occupant whenever the claim is not, so the
        // receipt names the row and the remedy peeks it. The evidence the
        // overlay read first is the node's open graph execute row, so the
        // refusal names that row too: a bare `claim release` leaves it open,
        // and the roster row it crosschecks can outlive the work by hours.
        let token = worker_token(&seen.worker);
        let first = token.split(',').next().unwrap_or_default();
        let unmeasured = seen.worker.contains(UNMEASURABLE_LABEL_MARK);
        let rows = crate::backlog::worked::open_execute_row_ids(req.node_id);
        let mut remedy =
            format!("fno agents peek {first}; if its run is finished, fno agents stop {first}");
        if unmeasured {
            remedy.push_str(&format!(
                "; liveness was never measured for this row, so read \
                 fno agents claim status node:{} too",
                req.node_id
            ));
        }
        if !rows.is_empty() {
            let list: Vec<String> = rows.iter().map(|(h, sid)| format!("{h}:{sid}")).collect();
            let reaps: Vec<String> = rows
                .iter()
                .map(|(h, sid)| {
                    format!(
                        "fno backlog session reap-open {} --phase execute --harness {} --session-id {}",
                        req.node_id, h, sid
                    )
                })
                .collect();
            remedy.push_str(&format!(
                "; the occupied row is the node's open graph execute row {}. \
                 Once the peek proves the run is over, close it with `{}`",
                list.join(", "),
                reaps.join("`, then `")
            ));
        }
        out.insert("verdict".into(), json!("already-running"));
        out.insert("reason".into(), json!("worker-row"));
        out.insert("worker".into(), json!(seen.worker));
        out.insert("truth_status".into(), json!(seen.truth_status));
        if unmeasured {
            out.insert("worker_unmeasured".into(), json!(true));
        }
        if !rows.is_empty() {
            out.insert("graph_execute_rows".into(), json!(rows));
        }
        out.insert("remedy".into(), json!(remedy));
        return Value::Object(out);
    }
    // An authority outage is NOT occupancy.
    if block_wins {
        let why = seen
            .worked_error
            .clone()
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| block.clone());
        out.insert("verdict".into(), json!("unknown"));
        out.insert("reason".into(), json!(block));
        out.insert(
            "detail".into(),
            json!(format!(
                "could not check node:{} is running: {why}; retry",
                req.node_id
            )),
        );
        out.extend(common);
        return Value::Object(out);
    }
    let init = common.get("init_reached") == Some(&Value::Bool(true));
    let reason = if wedged {
        "suspect-claim"
    } else if init {
        "live-claim"
    } else {
        "unproven-claim"
    };
    out.insert("verdict".into(), json!("already-running"));
    out.insert("reason".into(), json!(reason));
    if wedged && req.no_reserve {
        out.insert("recovery".into(), json!("not-attempted"));
    }
    if wedged && !req.no_reserve {
        out.insert("remedy".into(), json!(remedy_for(node_key)));
    }
    // A planner occupant means a think or blueprint is writing the node on
    // another machine: the refusal names the doc, its date, the host and
    // the target door, so the second planner never re-seats.
    if seen.holder.starts_with(BLUEPRINT_HOLDER_PREFIX) {
        let facts = plan_doc_facts(req.node_id);
        let host = record.map(|r| r.host.as_str());
        for (k, v) in planner_fields(facts.as_ref(), req.node_id, &seen.holder, host) {
            out.insert(k, json!(v));
        }
    }
    out.extend(common);
    Value::Object(out)
}

/// Take `dispatch:<id>`, then the handover `node:<id>`. Every refusal past
/// the reservation hands back the reservation this call took.
pub fn reserve_and_hand_over(req: &GuardRequest) -> (Value, i32) {
    let node_key = format!("node:{}", req.node_id);
    let res_key = format!("dispatch:{}", req.node_id);
    let mine = [(res_key.clone(), req.holder.to_string())];
    let reserve = || {
        crate::claims::acquire(
            &res_key,
            req.holder,
            opts(
                req,
                req.ttl_ms,
                format!("bg-dispatch reservation for {}", req.node_id),
            ),
        )
    };
    let mut recovered = false;
    match reserve() {
        AcquireOutcome::Acquired(_) => {}
        AcquireOutcome::HeldByOther {
            holder: held_by, ..
        } => {
            // A dead spawner's reservation blocks nothing, but clearing it is
            // only safe when this call replaces it with the node claim.
            // Exactly one retry: losing twice means the contention is real.
            let (cleared, bucket) = if req.handover_holder.is_some() {
                reclaim_if_provably_dead(&res_key, req.root)
            } else {
                (None, "no-replacement-barrier".to_string())
            };
            recovered = cleared.is_some();
            if cleared.is_none() {
                let mut out = Map::new();
                out.insert("verdict".into(), json!("already-running"));
                out.insert("reason".into(), json!("reservation-held"));
                out.insert("holder".into(), json!(held_by));
                if REMEDIABLE_BUCKETS.contains(&bucket.as_str()) {
                    out.insert("remedy".into(), json!(remedy_for(&res_key)));
                }
                return (Value::Object(out), 0);
            }
            match reserve() {
                AcquireOutcome::Acquired(_) => {}
                AcquireOutcome::HeldByOther { holder, .. } => {
                    let holder = if holder.is_empty() {
                        "unknown".to_string()
                    } else {
                        holder
                    };
                    return (
                        json!({
                            "verdict": "already-running",
                            "reason": "reservation-held",
                            "holder": holder,
                            "remedy": remedy_for(&res_key),
                        }),
                        0,
                    );
                }
                AcquireOutcome::Error(e) => return reservation_error(req, &mine, &res_key, &e),
            }
        }
        AcquireOutcome::Error(e) => return reservation_error(req, &mine, &res_key, &e),
    }
    // Visibility barrier: the acquisition is not authoritative until the
    // exact holder is observable. A peer that won a lagged race launches.
    let post = crate::claims::status(&res_key, req.root)
        .1
        .map(|r| r.holder)
        .unwrap_or_default();
    if post != req.holder {
        let holder = if post.is_empty() {
            "unknown".to_string()
        } else {
            post
        };
        return (
            json!({"verdict": "already-running", "reason": "duplicate-claim", "holder": holder}),
            0,
        );
    }
    let mut out = Map::new();
    out.insert("verdict".into(), json!("dispatchable"));
    out.insert("reservation_key".into(), json!(res_key));
    out.insert("reservation_holder".into(), json!(req.holder));
    let Some(handover) = req.handover_holder else {
        return (Value::Object(out), 0);
    };
    // THE node claim, so the node reads worked from the moment it is
    // dispatched rather than from whenever the worker reaches its own init.
    // The spawn client exits within seconds of the launch, so its pid here
    // read `pid-absent`, a death basis, long before init re-acquired. The
    // claim carries no pid: the TTL bounds it and the pane probe proves death.
    let reason = format!("spawn handover window for {}", req.node_id);
    let handover_opts = AcquireOpts {
        pid: None,
        pid_unavailable: true,
        ..opts(req, HANDOVER_TTL_MS, reason)
    };
    match crate::claims::acquire(&node_key, handover, handover_opts) {
        AcquireOutcome::Acquired(_) => {
            out.insert("node_claim_key".into(), json!(node_key));
            out.insert("node_claim_holder".into(), json!(handover));
        }
        AcquireOutcome::HeldByOther { holder, .. } => {
            // Somebody else holds the node: a session that claimed it through
            // its own init is invisible to the reservation.
            release_claims(&mine, req.root);
            let race_holder = if holder.is_empty() {
                "unknown".to_string()
            } else {
                holder
            };
            let reason = if init_reached(req.node_id, &race_holder, req.cwd) {
                "live-claim"
            } else {
                "unproven-claim"
            };
            let detail = format!(
                "node:{} is held (claim held by {race_holder}); no worker launched",
                req.node_id
            );
            return (
                json!({
                    "verdict": "already-running",
                    "reason": reason,
                    "holder": race_holder,
                    "detail": detail,
                }),
                0,
            );
        }
        AcquireOutcome::Error(e) => {
            if recovered {
                // Nothing protects the launch window: the dead spawner's
                // reservation went and the claim that replaces it did not land.
                release_claims(&mine, req.root);
                let detail = format!(
                    "recovered a dead spawner's {res_key} but could not take node:{} ({e}); \
                     refusing rather than launching with neither barrier held",
                    req.node_id
                );
                return (json!({"verdict": "error", "detail": detail}), 3);
            }
            // Visibility is best-effort here: the reservation already stops
            // the double dispatch.
            out.insert("node_claim_error".into(), json!(e));
        }
    }
    (Value::Object(out), 0)
}

fn opts(req: &GuardRequest, ttl_ms: i64, reason: String) -> AcquireOpts {
    AcquireOpts {
        pid: req.pid,
        ttl_ms: Some(ttl_ms),
        reason: Some(reason),
        root: req.root.map(Path::to_path_buf),
        events_dir: req.root.map(Path::to_path_buf),
        ..Default::default()
    }
}

fn reservation_error(
    req: &GuardRequest,
    mine: &[(String, String)],
    res_key: &str,
    error: &str,
) -> (Value, i32) {
    release_claims(mine, req.root);
    let detail = format!("could not acquire dispatch reservation {res_key} ({error})");
    (json!({"verdict": "error", "detail": detail}), 3)
}

/// Force-release KEY when its holder is PROVABLY dead. Returns
/// `(prior_holder, bucket)`; the holder is `None` whenever nothing was
/// cleared, and the bucket says why. A read or probe failure clears nothing.
fn reclaim_if_provably_dead(key: &str, root: Option<&Path>) -> (Option<String>, String) {
    let unreadable = || (None, "unreadable".to_string());
    let Ok(dir) = crate::claims_root::claims_dir(key, root) else {
        return unreadable();
    };
    let Ok(Some(claim)) = crate::claim_store::read(key, root) else {
        return unreadable();
    };
    let Ok(row) = crate::claim_cli::reap::keyed_fresh_row(&dir, key) else {
        return unreadable();
    };
    let field = |name: &str| row.get(name).and_then(Value::as_str).unwrap_or_default();
    if key.starts_with("dispatch:") {
        // The reservation's own predicate. LIVENESS FIRST: a live holder is
        // benign dedup whoever wrote it. A registry-served live verdict names
        // the dispatcher's session, which outlives the holder by design, so
        // only the spawn-cli shape may clear on its dead pid.
        if field("state") == "live" {
            if field("session_basis") != "registry-served-live" || claim.pid.is_some_and(pid_alive)
            {
                return (None, HOLDER_ALIVE.to_string());
            }
        } else if field("bucket") == "offhost" {
            return (None, "offhost".to_string());
        }
        if !claim.holder.starts_with(SPAWN_CLI_HOLDER_PREFIX) {
            return (None, "foreign-reservation".to_string());
        }
    } else {
        match crate::claim_cli::reap::judge_key(&claim, &row) {
            Ok((true, _)) => {}
            Ok((false, bucket)) => return (None, bucket),
            Err(_) => return (None, "unprobed".to_string()),
        }
    }
    let pid = claim
        .pid
        .map(|p| p.to_string())
        .unwrap_or_else(|| "None".to_string());
    let reason = format!(
        "holder {} (pid {pid}) proven dead at dispatch",
        claim.holder
    );
    match crate::claim_store::force_release_observed_in_directory(&dir, key, &reason, &claim) {
        Ok(released) if released.get("archived").and_then(Value::as_bool) == Some(true) => {
            (Some(claim.holder), String::new())
        }
        Ok(_) => (None, "contended".to_string()),
        Err(_) => (None, "release-failed".to_string()),
    }
}

fn pid_alive(pid: i32) -> bool {
    !matches!(
        crate::claims::probe_pid(pid),
        crate::claims::PidProbe::Absent
    )
}

/// Record a self-clearing recovery on the claim-observed stream, so the log
/// tells "it recovered itself" from "nothing ever tried".
fn emit_reaped_abandoned(node_id: &str, prior_holder: &str, truth_status: &str) {
    crate::backlog::advance::advance_emit(
        crate::backlog::advance::EVENT_CLAIM_OBSERVED,
        json!({
            "node_id": node_id,
            "claim_verdict": "dead_predecessor",
            "claim_state": "suspect",
            "holder": prior_holder,
            "truth_status": truth_status,
            "action": "reaped-abandoned",
        }),
        None,
    );
}

/// True when a `fno target init` PROVABLY took this node claim. Two positive
/// markers: the init holder shape, or a manifest under CWD binding this key
/// AND naming the observed holder. Any read fault answers false.
pub fn init_reached(node_id: &str, holder: &str, cwd: Option<&str>) -> bool {
    if holder.starts_with(SESSION_HOLDER_PREFIX) {
        return true;
    }
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else {
        return false;
    };
    if holder.is_empty() {
        return false;
    }
    let Some(raw) = crate::target_orient::read_manifest(Path::new(cwd)) else {
        return false;
    };
    raw.get("target_claim_key").map(String::as_str) == Some(format!("node:{node_id}").as_str())
        && raw.get("target_claim_holder").map(String::as_str) == Some(holder)
}

/// Bare worker names, comma-joined, no spaces. Both shell consumers read the
/// `worker=` token up to the first space.
pub fn worker_token(worker: &str) -> String {
    worker
        .split(',')
        .filter_map(|part| part.split_whitespace().next())
        .collect::<Vec<_>>()
        .join(",")
}

/// The two commands that clear KEY, safest first. A reservation is not
/// reapable inside its TTL, so it gets the force-release line only.
pub fn remedy_for(key: &str) -> String {
    if key.starts_with("dispatch:") {
        return format!("  Clear it:  fno agents claim release {key} --force --reason '<why>'");
    }
    format!(
        "  Clear it:  fno agents claim reap --apply      \
         (takes it only if no live worker is on the node)\n  \
         Override:  fno agents claim release {key} --force --reason '<why>'"
    )
}

/// A node's plan doc, as far as this machine reads it: the carried path, its
/// date, its frontmatter status.
#[derive(Debug)]
pub(crate) struct PlanDocFacts {
    pub path: String,
    pub date: Option<String>,
    pub status: Option<String>,
}

/// The node row's plan doc, as far as this machine reads it. `None` when the
/// row carries no plan_path.
pub(crate) fn plan_doc_facts(node_id: &str) -> Option<PlanDocFacts> {
    let rows = read_node_rows_in(&crate::backlog::settings::graph_path(), node_id).ok()?;
    let row = rows.iter().find(|r| {
        r.get("archived_at").filter(|v| !v.is_null()).is_none()
            && r.get("id").and_then(Value::as_str) == Some(node_id)
    })?;
    let path = row
        .get("plan_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())?
        .to_string();
    let text = std::fs::read_to_string(&path).ok();
    let date = text
        .as_deref()
        .and_then(|t| frontmatter_value(t, "created"))
        .or_else(|| filename_date(Path::new(&path)));
    let status = text.as_deref().and_then(|t| frontmatter_value(t, "status"));
    Some(PlanDocFacts { path, date, status })
}

/// One `key: value` line from a leading frontmatter block, clamped to the
/// same window the node-doc finder reads.
fn frontmatter_value(text: &str, key: &str) -> Option<String> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---").unwrap_or(rest.len());
    let block: String = rest[..end].chars().take(2000).collect();
    block.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(key)
            .then(|| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
            .filter(|v| !v.is_empty())
    })
}

/// The `YYYYMMDD-` filename prefix as a date, the fallback when no
/// frontmatter date reads.
fn filename_date(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let head = name.get(..8)?;
    head.chars()
        .all(|c| c.is_ascii_digit())
        .then(|| format!("{}-{}-{}", &head[0..4], &head[4..6], &head[6..8]))
}

/// Age of a `YYYY-MM-DD` date in days.
fn plan_doc_age_days(date: &str) -> Option<i64> {
    let doc = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some((chrono::Utc::now().date_naive() - doc).num_days())
}

/// The plan-fresh gate: a refusal when the node's plan doc already answers
/// the phase, a warning when a think passes on age.
#[derive(Debug)]
pub(crate) enum PlanFresh {
    /// The doc already answers: refuse with these fields and detail.
    Refuse {
        detail: String,
        fields: Vec<(String, String)>,
    },
    /// A stale doc a think passes: print this one line.
    Warn(String),
}

/// How fresh a plan doc must be to refuse a second think. One constant, so
/// a later ruling retunes the window in one place.
const PLAN_FRESH_WINDOW_DAYS: i64 = 7;

/// The gate. `Some` when the phase's plan doc already answers it.
pub(crate) fn plan_fresh(node_id: &str, session_phase: &str) -> Option<PlanFresh> {
    if session_phase != "think" && session_phase != "blueprint" {
        return None;
    }
    let facts = plan_doc_facts(node_id)?;
    plan_fresh_for_facts(session_phase, &facts)
}

/// The rule, per phase. A think refuses a young doc and warns past the
/// window; a blueprint refuses only a `status: ready` doc, which is the
/// target door's input, never blueprint's.
fn plan_fresh_for_facts(phase: &str, facts: &PlanDocFacts) -> Option<PlanFresh> {
    let fields = |date: Option<&str>| {
        let mut fields = vec![("plan".to_string(), facts.path.clone())];
        if let Some(date) = date {
            fields.push(("plan_date".to_string(), date.to_string()));
        }
        fields
    };
    match (phase, facts.status.as_deref(), facts.date.as_deref()) {
        ("blueprint", Some("ready"), _) => Some(PlanFresh::Refuse {
            detail: "the node's plan doc is already ready; run the target door".to_string(),
            fields: fields(facts.date.as_deref()),
        }),
        ("blueprint", _, _) => None,
        ("think", _, Some(date)) => {
            let age = plan_doc_age_days(date)?;
            if age < PLAN_FRESH_WINDOW_DAYS {
                Some(PlanFresh::Refuse {
                    detail: format!(
                        "the node's plan doc is {age} day(s) old ({date}); a second think \
                         waits past the 7-day window"
                    ),
                    fields: fields(Some(date)),
                })
            } else {
                Some(PlanFresh::Warn(format!(
                    "note: node plan doc from {date} ({path}); think proceeds past \
                     the 7-day window",
                    path = facts.path
                )))
            }
        }
        ("think", _, None) => None,
        _ => None,
    }
}

/// The planner-occupancy fields a refusal names: the node's plan doc, its
/// date, the holder's host, and the next command. A planner claim means a
/// think or blueprint is writing the node on another machine; the second
/// planner's move is the target door, never a second seat.
pub(crate) fn planner_fields(
    facts: Option<&PlanDocFacts>,
    node_id: &str,
    holder: &str,
    host: Option<&str>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(host) = host.filter(|h| !h.is_empty()) {
        out.push(("host".into(), host.to_string()));
    }
    if let Some(facts) = facts {
        out.push(("plan".into(), facts.path.clone()));
        if let Some(date) = facts.date.as_deref() {
            out.push(("plan_date".into(), date.to_string()));
        }
        out.push(("next".into(), format!("/fno:target {node_id}")));
    } else {
        out.push(("next".into(), format!("wait for {holder} or peek it")));
    }
    out
}
/// and `detail=` last: both shell consumers and `advance_dispatch` /
/// `dispatch_launch` read the tokens, never the prose.
pub fn refusal_line(node: &str, verdict: &Value) -> String {
    let text = |key: &str| {
        verdict
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    let reason = text("reason").or(text("verdict")).unwrap_or("unknown");
    let prior = text("holder")
        .map(|h| format!(" prior_holder={h}"))
        .unwrap_or_default();
    let mut worker = text("worker")
        .map(|w| format!(" worker={}", worker_token(w)))
        .unwrap_or_default();
    if verdict.get("worker_unmeasured") == Some(&Value::Bool(true)) {
        worker.push_str(" worker_unmeasured=true");
    }
    let planner: String = ["plan", "plan_date", "host", "next"]
        .iter()
        .filter_map(|k| {
            text(k).map(|v| {
                let shown = if v.chars().any(char::is_whitespace) {
                    crate::claude_ask::py_repr(&v)
                } else {
                    v.to_string()
                };
                format!(" {k}={shown}")
            })
        })
        .collect();
    let detail = text("detail")
        .map(|d| format!(" detail={}", crate::claude_ask::py_repr(d)))
        .unwrap_or_default();
    format!(
        "node dispatch refused: node={node} verdict={} reason={reason}{prior}{worker}{planner}; \
         no worker launched{detail}",
        text("verdict").unwrap_or("None")
    )
}

/// Release every claim a failed dispatch took. Swallowed, never silent: a
/// release that quietly no-ops leaves the key held for its whole TTL.
pub fn release_claims(claims: &[(String, String)], root: Option<&Path>) {
    for (key, holder) in claims {
        if let Err(e) = crate::claims::release(key, holder, root, root) {
            eprintln!(
                "WARNING: could not release {key} held by {holder} ({e}); it stays held \
                 until its TTL expires. Free it with: fno agents claim release {key} \
                 --holder {holder}"
            );
        }
    }
}

/// The keys one node-bearing launch holds, and the facts its stamps need.
/// Dropped before [`settle`] marks a launch it releases the keys, so every
/// exit between the guard and a proven launch hands them back: the gate's
/// refusal, a lane failure, an early return.
pub struct SpawnHold {
    claims: Vec<(String, String)>,
    root: Option<PathBuf>,
    launched: bool,
    node: String,
    phase: String,
    message: String,
    one_shot: bool,
}

impl Drop for SpawnHold {
    fn drop(&mut self) {
        if !self.launched {
            release_claims(&self.claims, self.root.as_deref());
        }
    }
}

/// Settle the hold on the spawn's exit. A launch opens the node's sessions
/// row and stamps its launch edge. A thread keeps its keys: the reservation
/// runs out its TTL and the worker's own init takes over the node claim. A
/// one-shot hands them back, because its worker has already exited and
/// nobody inherits them.
pub fn settle(hold: &mut Option<SpawnHold>, exit: i32, name: &str) {
    let Some(hold) = hold.as_mut() else {
        return;
    };
    if exit != 0 {
        return;
    }
    let graph = crate::backlog::settings::graph_path();
    open_session_row(&graph, hold, name);
    stamp_launch_edge(&graph, &hold.node);
    hold.launched = !hold.one_shot;
}

/// Open the node's sessions row for the spawned worker. Its identity is the
/// worker's own harness session id from the registry row. A thread binds that
/// id after the row exists, so the owed row parks on the registry row and
/// opens at the worker's first SessionStart. A provenance miss never fails
/// the spawn.
fn open_session_row(graph: &Path, hold: &SpawnHold, name: &str) {
    let node = hold.node.as_str();
    let grant = (hold.phase == "execute").then(|| merge_grant(&hold.message));
    let registry = crate::paths::AgentsHome::from_env().registry_json();
    let row = crate::state::load_registry(&registry)
        .ok()
        .and_then(|r| r.find(name).cloned());
    let harness = row
        .as_ref()
        .map(|r| r.harness_name().to_string())
        .unwrap_or_default();
    let session = row
        .as_ref()
        .and_then(|r| r.harness_session_id.clone())
        .filter(|s| !s.is_empty() && !harness.is_empty());
    let Some(session) = session else {
        let payload = json!({"name": name, "phase": hold.phase, "merge_grant": grant});
        if let Err(e) = crate::pending_session_row::park(&payload) {
            eprintln!("spawn: park skipped for {node}: {e}");
        }
        return;
    };
    let effort = row.as_ref().and_then(|r| r.effort.clone());
    let started = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let store = crate::backlog::api::Store::new(graph);
    match crate::backlog::api::session_open_parked(
        &store,
        node,
        &hold.phase,
        &harness,
        &session,
        effort.as_deref(),
        grant,
        &started,
    ) {
        Ok(true) => {}
        Ok(false) => eprintln!(
            "spawn: session row open skipped for {node} (node not in graph); the row was \
             not written. Skipped."
        ),
        Err(e) => eprintln!("spawn: session row open skipped for {node}: {}", e.0),
    }
}

/// The spawner's own merge-grant verdict for an execute-phase worker: the
/// explicit refusal first, then the standing config. The false is recorded
/// too, so an absent grant reads as "nobody resolved", never as a grant.
fn merge_grant(message: &str) -> Value {
    let recorded_by = std::env::var("FNO_AGENT_SELF")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "spawn".to_string());
    let recorded_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let (approved, source) = if crate::merge_posture::message_carries_no_merge(message) {
        (false, "no-merge-flag")
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let granted = crate::agents_config::auto_merge_enabled(&cwd)
            && crate::agents_config::auto_merge_grant_dispatches(&cwd);
        (granted, if granted { "config" } else { "none" })
    };
    json!({
        "approved": approved,
        "source": source,
        "recorded_by": recorded_by,
        "recorded_at": recorded_at,
    })
}

/// Record who launched the node's worker, on the node itself. No proven
/// parent session, no write. The spawn seam scrubs the ambient identity
/// before it execs this client, so the usual producer of the edge is the
/// launch record the parent's own hook writes and the child's SessionStart
/// claims; this stamp covers a client that still sees its parent.
fn stamp_launch_edge(graph: &Path, node: &str) {
    let (session, harness, cwd) = crate::claims::ambient_parent_edge();
    let Some(session) = session else {
        return;
    };
    if let Err(e) =
        crate::launch_record::stamp_node(graph, node, &session, harness.as_deref(), cwd.as_deref())
    {
        eprintln!("spawn: launch edge not recorded on {node}: {e}");
    }
}

/// The provenance this process exported for its spawn. A pane child inherits
/// the mux server's env, never this one's, so the pane wrapper reads it here.
static EXPORTED: std::sync::Mutex<Vec<(&'static str, String)>> = std::sync::Mutex::new(Vec::new());

/// The `(key, value)` pairs the last guarded spawn in this process exported.
pub fn exported_provenance() -> Vec<(&'static str, String)> {
    EXPORTED.lock().map(|g| g.clone()).unwrap_or_default()
}

/// The session phases a node's sessions row may name (`SESSION_PHASES`).
const SESSION_PHASES: [&str; 5] = ["blueprint", "execute", "review", "ship", "think"];

/// The phase a node spawn binds its worker under: the explicit
/// `--session-phase` (with the retired `do` alias), else the seed command's.
fn spawn_phase(params: &Value, node: &str) -> Result<String, i32> {
    let listed = format!("{:?}", SESSION_PHASES).replace('"', "'");
    if let Some(given) = params.get("session_phase").and_then(Value::as_str) {
        let phase = if given == "do" { "execute" } else { given };
        if !SESSION_PHASES.contains(&phase) {
            eprintln!(
                "--session-phase must be one of {listed} (got {})",
                crate::claude_ask::py_repr(phase)
            );
            return Err(2);
        }
        return Ok(phase.to_string());
    }
    let message = params.get("message").and_then(Value::as_str).unwrap_or("");
    match crate::spawn_phase::seed_phase(message) {
        Some(phase) => Ok(phase.to_string()),
        None => {
            eprintln!(
                "refusing --node {node}: the message command names no session phase, so no \
                 worker would be bound to the node. Pass --session-phase <one of {listed}>. \
                 No worker launched."
            );
            Err(2)
        }
    }
}

/// The pane door: the same guard around the pane launch, judged from the
/// launch dir the pane spine will resolve (an explicit `--cwd`, else the
/// node's project).
pub fn run_guarded_pane(
    params: &Value,
    name: &str,
    caller_session: Option<&str>,
    launch: impl FnOnce() -> i32,
) -> i32 {
    let caller = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = match params.get("cwd").and_then(Value::as_str) {
        Some(explicit) => PathBuf::from(explicit),
        None => match crate::node_seed::spawn_node_cwd(params, &caller) {
            crate::node_seed::SpawnNodeCwd::Project(project) => project,
            _ => caller,
        },
    };
    let mut hold = match guard_client_spawn(params, name, &cwd, caller_session) {
        Ok(hold) => hold,
        Err(code) => return code,
    };
    let code = launch();
    settle(&mut hold, code, name);
    code
}

/// `FNO_NODE`/`FNO_SLUG`/`FNO_PLAN` for a node spawn (`mux_spawn.resolve_provenance`).
/// The graph read normalizes a slug to its id and fills an absent slug or
/// plan. On a read failure a node that is not a well-formed id is dropped,
/// so a slug never leaks into `FNO_NODE`.
pub fn resolve_provenance(
    node: &str,
    slug: Option<&str>,
    plan: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut node = Some(node.to_string());
    let mut slug = slug.map(str::to_string);
    let mut plan = plan.map(str::to_string);
    let raw = node.clone().unwrap_or_default();
    if slug.is_none() || plan.is_none() || !crate::backlog::node_ref::has_node_id_prefix(&raw) {
        match read_node_rows(&raw) {
            Ok(rows) => {
                let found = rows.iter().find(|r| {
                    r.get("archived_at").filter(|v| !v.is_null()).is_none()
                        && (r.get("id").and_then(Value::as_str) == Some(raw.as_str())
                            || r.get("slug").and_then(Value::as_str) == Some(raw.as_str()))
                });
                if let Some(row) = found {
                    let field = |k: &str| {
                        row.get(k)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    if !field("id").is_empty() {
                        node = Some(field("id"));
                    }
                    if slug.is_none() {
                        slug = Some(field("slug"));
                    }
                    if plan.is_none() {
                        plan = Some(field("plan_path"));
                    }
                }
            }
            Err(_) => {
                if !crate::backlog::node_ref::is_wellformed_node_id(&raw) {
                    node = None;
                }
            }
        }
    }
    [("FNO_NODE", node), ("FNO_SLUG", slug), ("FNO_PLAN", plan)]
        .into_iter()
        .filter_map(|(k, v)| v.filter(|v| !v.is_empty()).map(|v| (k, v)))
        .collect()
}

/// The node's rows by id first; the whole table only when the id read finds
/// nothing, which is the slug case. The `graph` param keeps the read
/// env-free, so a test passes a temp graph and no race.
fn read_node_rows_in(graph: &Path, node: &str) -> Result<Vec<Value>, String> {
    let read = |id_in: Option<Vec<String>>| {
        crate::graph_store::read_rows_where_strict(
            &graph,
            &crate::backlog::RowQuery {
                filter: crate::backlog::api::NodeFilter {
                    id_in,
                    ..Default::default()
                },
                fields: Some(
                    ["id", "slug", "plan_path", "archived_at"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                ),
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())
    };
    let rows = read(Some(vec![node.to_string()]))?;
    if rows.is_empty() {
        read(None)
    } else {
        Ok(rows)
    }
}

/// The graph the guard reads, as [`read_node_rows_in`] wants it.
fn read_node_rows(node: &str) -> Result<Vec<Value>, String> {
    read_node_rows_in(&crate::backlog::settings::graph_path(), node)
}

/// The client door's guard for a node-bearing spawn. It clears the inherited
/// provenance, resolves this spawn's, runs the guard, and exports the worker
/// env. `Err` carries the exit code; its stderr is already printed. `Ok(None)`
/// is a spawn with no `--node`, which takes no key.
pub fn guard_client_spawn(
    params: &Value,
    name: &str,
    cwd: &Path,
    caller_session: Option<&str>,
) -> Result<Option<SpawnHold>, i32> {
    // Set or cleared together, never merged with what this process inherited:
    // a worker spawning a child must not pass down its own node.
    for key in PROVENANCE_KEYS {
        std::env::remove_var(key);
    }
    if let Ok(mut exported) = EXPORTED.lock() {
        exported.clear();
    }
    let param = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    // An explicit --node is guarded even when it is empty: it then resolves
    // to nothing and refuses, rather than launching with no key held.
    let Some(raw_node) = params.get("node").and_then(Value::as_str) else {
        return Ok(None);
    };
    let node = raw_node.trim();
    let provenance = if node.is_empty() {
        Vec::new()
    } else {
        resolve_provenance(node, param("slug"), param("plan"))
    };
    let Some(guarded) = provenance
        .iter()
        .find(|(k, _)| *k == "FNO_NODE")
        .map(|(_, v)| v.clone())
    else {
        eprintln!(
            "refusing unresolved --node {}: cannot run the shared family-2 dispatch \
             guard; no worker launched",
            crate::claude_ask::py_repr(raw_node)
        );
        return Err(2);
    };
    // The Python door guards first and then execs this client for its codex
    // and opencode lanes with the same --node. The reservation then names the
    // parent, so a second judgment would refuse the spawn the parent just
    // admitted. The parent owns the keys and the stamps; this carries the env.
    let parent = format!(
        "{SPAWN_CLI_HOLDER_PREFIX}{}",
        std::os::unix::process::parent_id()
    );
    let reserved = crate::claims::status(&format!("dispatch:{guarded}"), None).1;
    if reserved.is_some_and(|r| r.holder == parent) {
        for (key, value) in &provenance {
            std::env::set_var(key, value);
        }
        if let Ok(mut exported) = EXPORTED.lock() {
            *exported = provenance;
        }
        return Ok(None);
    }
    let phase = spawn_phase(params, node)?;
    let pid = std::process::id();
    let holder = format!("{SPAWN_CLI_HOLDER_PREFIX}{pid}");
    // The worker's name is the whole proof of successorship; an empty one
    // falls back to this dispatch's pid, which is at least not guessable.
    let handover = if name.is_empty() {
        format!("{HANDOVER_HOLDER_PREFIX}pid-{pid}")
    } else {
        format!("{HANDOVER_HOLDER_PREFIX}{name}")
    };
    let cwd = cwd.to_string_lossy().into_owned();
    let node_occupancy =
        crate::backlog::worked::reachable_workers_by_node_excluding(caller_session);
    let (verdict, exit) = decide_with_occupancy(
        &GuardRequest {
            node_id: &guarded,
            holder: &holder,
            ttl_ms: RESERVATION_TTL_MS,
            no_reserve: false,
            cwd: Some(cwd.as_str()),
            handover_holder: Some(handover.as_str()),
            root: None,
            pid: None,
            phase: Some(phase.as_str()),
        },
        Some(&node_occupancy),
    );
    if verdict.get("verdict").and_then(Value::as_str) != Some("dispatchable") {
        eprintln!("{}", refusal_line(&guarded, &verdict));
        if let Some(remedy) = verdict.get("remedy").and_then(Value::as_str) {
            eprintln!("{remedy}");
        }
        return Err(if exit == 0 { 2 } else { exit });
    }
    let text = |k: &str| verdict.get(k).and_then(Value::as_str).map(str::to_string);
    let mut hold = SpawnHold {
        claims: Vec::new(),
        root: None,
        launched: false,
        node: guarded.clone(),
        phase,
        message: params
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        one_shot: params.get("substrate").and_then(Value::as_str) == Some("headless"),
    };
    if let (Some(key), Some(holder)) = (text("reservation_key"), text("reservation_holder")) {
        hold.claims.push((key, holder));
    }
    let mut env = provenance;
    if let (Some(key), Some(holder)) = (text("node_claim_key"), text("node_claim_holder")) {
        hold.claims.push((key, holder.clone()));
        // The worker proves successorship by naming this holder back. It
        // rides the environment, never the command line.
        env.push(("FNO_NODE_CLAIM_HOLDER", holder));
    } else if let Some(error) = text("node_claim_error") {
        eprintln!(
            "note: node:{guarded} claim not taken at dispatch ({error}); the worker claims it at init"
        );
    }
    for (key, value) in &env {
        std::env::set_var(key, value);
    }
    if let Ok(mut exported) = EXPORTED.lock() {
        *exported = env;
    }
    Ok(Some(hold))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(root: &'a Path, holder: &'a str, handover: Option<&'a str>) -> GuardRequest<'a> {
        GuardRequest {
            node_id: "x-1",
            holder,
            ttl_ms: RESERVATION_TTL_MS,
            no_reserve: false,
            cwd: None,
            handover_holder: handover,
            root: Some(root),
            pid: None,
            phase: None,
        }
    }

    fn holder_of(root: &Path, key: &str) -> Option<String> {
        crate::claims::status(key, Some(root)).1.map(|r| r.holder)
    }

    #[test]
    fn planner_refusal_names_doc_date_host_and_target() {
        // AC4: the planner-held refusal carries the doc, its date, the holder's
        // host and the target door, in one line a shell can read.
        let facts = super::PlanDocFacts {
            path: "/plans/20261102-somewhere-x-1.md".into(),
            date: Some("2026-11-02".into()),
            status: None,
        };
        let fields = super::planner_fields(
            Some(&facts),
            "x-1",
            "blueprint-session:s9",
            Some("other-host"),
        );
        let mut verdict = serde_json::Map::new();
        verdict.insert("verdict".into(), serde_json::json!("already-running"));
        verdict.insert("reason".into(), serde_json::json!("live-claim"));
        verdict.insert("holder".into(), serde_json::json!("blueprint-session:s9"));
        for (k, v) in fields {
            verdict.insert(k, serde_json::json!(v));
        }
        let line = super::refusal_line("x-1", &serde_json::Value::Object(verdict));
        assert!(
            line.contains("plan=/plans/20261102-somewhere-x-1.md"),
            "{line}"
        );
        assert!(line.contains("plan_date=2026-11-02"), "{line}");
        assert!(line.contains("host=other-host"), "{line}");
        assert!(line.contains("/fno:target x-1"), "{line}");
    }

    #[test]
    fn planner_refusal_without_a_doc_points_at_the_holder() {
        let fields = super::planner_fields(None, "x-1", "blueprint-session:s9", None);
        assert_eq!(
            fields.iter().find(|(k, _)| k == "next").unwrap().1,
            "wait for blueprint-session:s9 or peek it"
        );
    }

    #[test]
    fn plan_fresh_gates_think_by_age_and_blueprint_by_status() {
        // AC5: a think refuses a doc inside the window and warns past it; a
        // blueprint refuses only a ready doc. A doc with no date gates nothing.
        let today = chrono::Utc::now().date_naive().to_string();
        let old = (chrono::Utc::now().date_naive() - chrono::Duration::days(30)).to_string();
        let facts = |date: Option<&str>, status: Option<&str>| super::PlanDocFacts {
            path: "/plans/20261101-somewhere-x-1.md".into(),
            date: date.map(str::to_string),
            status: status.map(str::to_string),
        };
        let refuse = |f: &super::PlanDocFacts| match super::plan_fresh_for_facts("think", f) {
            Some(super::PlanFresh::Refuse { fields, .. }) => fields,
            other => panic!("expected refuse, got {other:?}"),
        };
        let fresh = facts(Some(&today), None);
        let fields = refuse(&fresh);
        assert!(fields
            .iter()
            .any(|(k, v)| k == "plan" && v.ends_with("somewhere-x-1.md")));
        assert!(fields.iter().any(|(k, v)| k == "plan_date" && *v == today));

        let stale = facts(Some(&old), None);
        match super::plan_fresh_for_facts("think", &stale) {
            Some(super::PlanFresh::Warn(line)) => assert!(line.contains("somewhere-x-1.md")),
            other => panic!("expected warn, got {other:?}"),
        }

        let ready = facts(Some(&today), Some("ready"));
        assert!(refuse_bp(&ready).is_some());
        let design = facts(Some(&today), Some("design"));
        assert!(super::plan_fresh_for_facts("blueprint", &design).is_none());
        let undated = facts(None, None);
        assert!(super::plan_fresh_for_facts("think", &undated).is_none());
    }

    fn refuse_bp(f: &super::PlanDocFacts) -> Option<super::PlanFresh> {
        super::plan_fresh_for_facts("blueprint", f)
    }

    #[test]
    fn a_free_node_reserves_and_takes_the_handover_claim() {
        // AC1: dispatch:<id> under spawn-cli:<pid>, node:<id> under the
        // worker's handover holder.
        let tmp = tempfile::tempdir().unwrap();
        let (out, exit) = reserve_and_hand_over(&request(
            tmp.path(),
            "spawn-cli:1",
            Some("spawn-handover:w"),
        ));
        assert_eq!(exit, 0);
        assert_eq!(out["verdict"], "dispatchable");
        assert_eq!(out["reservation_key"], "dispatch:x-1");
        assert_eq!(out["node_claim_holder"], "spawn-handover:w");
        assert_eq!(
            holder_of(tmp.path(), "dispatch:x-1").as_deref(),
            Some("spawn-cli:1")
        );
        assert_eq!(
            holder_of(tmp.path(), "node:x-1").as_deref(),
            Some("spawn-handover:w")
        );
        // The handover outlives the spawn client: it carries no pid, so a dead
        // client never reads as pid-absent; the TTL alone bounds it.
        let record = crate::claims::status("node:x-1", Some(tmp.path()))
            .1
            .unwrap();
        assert!(record.pid_unavailable && record.pid.is_none(), "{record:?}");
        let gone = |_| crate::claims::PidProbe::Absent;
        assert_eq!(
            crate::claims::classify_with_basis(&record, None, &gone),
            (crate::claims::ClaimState::Suspect, "pid-unavailable")
        );
    }

    #[test]
    fn two_racing_spawns_launch_exactly_one() {
        // AC4: the second dispatcher meets the first one's live reservation.
        let tmp = tempfile::tempdir().unwrap();
        let first = reserve_and_hand_over(&request(
            tmp.path(),
            "spawn-cli:1",
            Some("spawn-handover:a"),
        ));
        let second = reserve_and_hand_over(&request(
            tmp.path(),
            "spawn-cli:2",
            Some("spawn-handover:b"),
        ));
        assert_eq!(first.0["verdict"], "dispatchable");
        assert_eq!(second.0["verdict"], "already-running");
        assert_eq!(second.0["reason"], "reservation-held");
        assert_eq!(second.0["holder"], "spawn-cli:1");
        assert!(second.0.get("remedy").is_none(), "{}", second.0);
        assert_eq!(
            holder_of(tmp.path(), "node:x-1").as_deref(),
            Some("spawn-handover:a")
        );
    }

    #[test]
    fn a_held_node_refuses_and_hands_back_the_reservation() {
        // AC2: a node somebody else claimed refuses, and the refusal keeps
        // no key of its own.
        let tmp = tempfile::tempdir().unwrap();
        let held = crate::claims::acquire(
            "node:x-1",
            "worker:t-x",
            AcquireOpts {
                ttl_ms: Some(RESERVATION_TTL_MS),
                // A foreign session: the ambient one would let a lead running
                // this test delegate the claim to the handover holder.
                identity: Some(("s-foreign-holder".into(), "claude".into())),
                root: Some(tmp.path().to_path_buf()),
                events_dir: Some(tmp.path().to_path_buf()),
                ..Default::default()
            },
        );
        assert!(matches!(held, AcquireOutcome::Acquired(_)), "{held:?}");
        let (out, exit) = reserve_and_hand_over(&request(
            tmp.path(),
            "spawn-cli:1",
            Some("spawn-handover:w"),
        ));
        assert_eq!(exit, 0);
        assert_eq!(out["verdict"], "already-running");
        assert_eq!(out["reason"], "unproven-claim");
        assert_eq!(out["holder"], "worker:t-x");
        assert_eq!(holder_of(tmp.path(), "dispatch:x-1"), None);
    }

    #[test]
    fn a_hold_dropped_before_launch_releases_both_keys() {
        // AC3: the gate refused after the guard passed; both keys go back.
        let tmp = tempfile::tempdir().unwrap();
        let (out, _) = reserve_and_hand_over(&request(
            tmp.path(),
            "spawn-cli:1",
            Some("spawn-handover:w"),
        ));
        assert_eq!(out["verdict"], "dispatchable");
        let claims = vec![
            ("dispatch:x-1".to_string(), "spawn-cli:1".to_string()),
            ("node:x-1".to_string(), "spawn-handover:w".to_string()),
        ];
        let hold = |claims, launched| SpawnHold {
            claims,
            root: Some(tmp.path().to_path_buf()),
            launched,
            node: "x-1".into(),
            phase: "execute".into(),
            message: String::new(),
            one_shot: false,
        };
        drop(hold(claims.clone(), true));
        assert!(holder_of(tmp.path(), "dispatch:x-1").is_some());
        drop(hold(claims, false));
        assert_eq!(holder_of(tmp.path(), "dispatch:x-1"), None);
        assert_eq!(holder_of(tmp.path(), "node:x-1"), None);
    }

    #[test]
    fn the_refusal_line_keeps_reason_first_and_detail_last() {
        let live = json!({"verdict": "already-running", "reason": "live-claim",
                          "holder": "target-session:s1"});
        assert_eq!(
            refusal_line("x-1", &live),
            "node dispatch refused: node=x-1 verdict=already-running reason=live-claim \
             prior_holder=target-session:s1; no worker launched"
        );
        let row = json!({"verdict": "already-running", "reason": "worker-row",
                         "worker": "t-a (unmeasurable: x), t-b", "worker_unmeasured": true,
                         "detail": "it's held"});
        assert_eq!(
            refusal_line("x-1", &row),
            "node dispatch refused: node=x-1 verdict=already-running reason=worker-row \
             worker=t-a,t-b worker_unmeasured=true; no worker launched detail=\"it's held\""
        );
        let error = json!({"verdict": "error", "detail": "probe failed"});
        assert!(refusal_line("x-1", &error).contains(" reason=error; "));
    }

    #[test]
    fn an_unreachable_primary_refuses_as_an_outage_not_a_corrupted_claim() {
        let dead = crate::store_remote::test_primary::dead();
        let shared = crate::claim_store::directory(None).unwrap();
        crate::claim_store::route_to_primary(Some((dead.clone(), shared)));
        let tmp = tempfile::tempdir().unwrap();
        let (out, exit) = decide(&request(tmp.path(), "spawn-cli:1", None));
        crate::claim_store::route_to_primary(None);
        assert_eq!(exit, 3, "{out}");
        assert_eq!(out["reason"], "claims-unavailable");
        let detail = out["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains(dead.url()) && detail.contains("store.remote_url"),
            "{detail}"
        );
        assert!(refusal_line("x-1", &out).contains(" reason=claims-unavailable; "));
    }
}
