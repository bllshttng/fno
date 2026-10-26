//! The lead termination board read: what work and operator questions remain.

use crate::loopcheck::TerminationReason;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub(crate) struct LeadManifest {
    pub(crate) fno_id: String,
    pub(crate) scope: String,
    pub(crate) created_at: Option<String>,
    /// The teamed session the manifest names; `lead_state`'s split read keys
    /// on it. Empty on manifests written before identity fields existed.
    pub(crate) harness_session_id: Option<String>,
    /// The harness that teamed this scope, written at promotion
    /// (`FNO_HARNESS`, default `claude`); absent on manifests written before
    /// the field existed.
    pub(crate) harness: Option<String>,
    /// `pass` | `org`; absent reads as `pass`, never a third shape.
    pub(crate) shape: String,
    pub(crate) max_iterations: u64,
    pub(crate) respawn_count: u64,
    pub(crate) respawn_ceiling: u64,
    /// `span:<N>[smhd]` | `compactions:<N>`; absent reads
    /// [`crate::lead_term::DEFAULT_TERM`], undeclared.
    pub(crate) term: Option<String>,
    /// The written reason for a declared/reached term's replacement; absent
    /// on the first declaration.
    pub(crate) term_reason: Option<String>,
    /// The on-deck queue in seat order, recorded by `org checkin --queue`;
    /// empty on manifests written before the field existed.
    pub(crate) queue: Vec<String>,
}

pub(crate) fn parse_lead_manifest(content: &str) -> Option<LeadManifest> {
    let mut out = LeadManifest {
        max_iterations: 40,
        respawn_ceiling: 4,
        ..Default::default()
    };
    let mut saw_frontmatter = false;
    for line in content.lines() {
        if line.trim() == "---" {
            if saw_frontmatter {
                break;
            }
            saw_frontmatter = true;
            continue;
        }
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        let value = raw.trim().trim_matches('"').to_string();
        match key.trim() {
            "fno_id" => out.fno_id = value,
            "scope" => out.scope = value,
            "created_at" => out.created_at = Some(value),
            "harness_session_id" => {
                if !value.is_empty() && value != "null" {
                    out.harness_session_id = Some(value);
                }
            }
            "harness" => out.harness = Some(value),
            "shape" => out.shape = value,
            "term" => out.term = Some(value),
            "term_reason" => out.term_reason = Some(value),
            "queue" => {
                out.queue = raw
                    .split(',')
                    .map(|id| id.trim().trim_matches('"'))
                    .filter(|id| !id.is_empty())
                    .map(String::from)
                    .collect();
            }
            "budget_max_iterations" => {
                if let Ok(n) = value.parse::<u64>() {
                    out.max_iterations = n;
                }
            }
            "respawn_count" => {
                if let Ok(n) = value.parse::<u64>() {
                    out.respawn_count = n;
                }
            }
            "respawn_ceiling" => {
                if let Ok(n) = value.parse::<u64>() {
                    out.respawn_ceiling = n;
                }
            }
            _ => {}
        }
    }
    if saw_frontmatter && !out.fno_id.is_empty() {
        Some(out)
    } else {
        None
    }
}

pub(crate) struct StandDownGate {
    pub(crate) reading: String,
    pub(crate) message: String,
}

pub(crate) fn stand_down_gate(
    manifest: &LeadManifest,
    transcript: &Path,
    cwd: &Path,
) -> Option<StandDownGate> {
    // Same spent-line refusal the board takes: past the reserve line the
    // transcript scan would spend the drain's slice, and the gate already
    // fails open (None) on anything it cannot measure.
    if crate::loopcheck::stopgate_pre_drain_spent() {
        return None;
    }
    let session = manifest
        .harness_session_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())?;
    let capture_dir = crate::operator_turns::capture_dir(cwd)?;
    let pending = match crate::operator_turns::pending_stand_down(
        session,
        transcript,
        &capture_dir,
        chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
    ) {
        Ok(turns) => turns,
        Err(error) => {
            eprintln!("loop-check: stand-down gate skipped: {error}");
            return None;
        }
    };
    let (turn_id, excerpt) = pending.first()?;
    Some(StandDownGate {
        reading: format!("operator stand-down turn {turn_id} unacked"),
        message: format!(
            "operator stand-down turn {turn_id} is unacked: \"{excerpt}\". Answer it as a verdict on this lead, then run fno inbox operator ack {turn_id} --outcome <nothing|law:<id>|node:<id>> --why \"<your verdict>\""
        ),
    })
}

pub(crate) struct OrgBoard {
    pub(crate) actionable: i64,
    pub(crate) top_row: Option<String>,
    /// any queue on this board failed to read beyond a budget kill. The
    /// quiet branch refuses to certify a quiet board while this is true,
    /// instead of trusting a count that cannot see the blind queues. A
    /// starved queue is the board's own choice, not evidence, and never
    /// blocks.
    pub(crate) unreadable_sources: bool,
    pub(crate) actionable_ids: Vec<String>,
    pub(crate) spawn_held_ids: Vec<String>,
    /// Unplanned rows the lifecycle table routes to `/blueprint`: a plan
    /// must be written before a worker can take them. They hold the stop
    /// open only through the gate refusal or the blueprint ceiling.
    pub(crate) blueprint_held_ids: Vec<String>,
    pub(crate) blind_queues: Vec<String>,
    pub(crate) operator_question_sessions: Vec<String>,
    pub(crate) operator_questions_unreadable: bool,
}

impl OrgBoard {
    /// The not-read receipt every quiet exit carries: the board names what
    /// it did not see instead of exiting on an unseen board, and the next
    /// wake re-reads the named sources. Empty on a fully readable board.
    pub(crate) fn not_read_receipt(&self) -> String {
        if self.blind_queues.is_empty() {
            return String::new();
        }
        format!("; not read: {}", self.blind_queues.join(", "))
    }
}

fn row_identity(queue: &str, row: &Value) -> String {
    let id = row
        .get("id")
        .or_else(|| row.get("key"))
        .or_else(|| row.get("number"))
        .map(|v| v.to_string())
        .unwrap_or_else(|| row.to_string());
    format!("{queue}:{}", id.trim_matches('"'))
}

/// Whether the lifecycle table sends one planless unplanned row to
/// `/target`. An unanswerable row returns false, so it keeps the block
/// pointed at it: a row this read cannot classify is never counted as held.
fn unplanned_routes_to_target(row: &Value, floor: &str) -> bool {
    matches!(
        crate::backlog_ready::effective_command_with_floor(row, floor),
        Ok((Some(command), _)) if command == "/target"
    )
}

/// Whether the lifecycle table sends one planless unplanned row to
/// `/blueprint` (a plan must be written before any worker can take it).
/// An unanswerable row returns false, so it keeps the block pointed at it:
/// a row this read cannot classify is never counted as held.
fn unplanned_routes_to_blueprint(row: &Value, floor: &str) -> bool {
    matches!(
        crate::backlog_ready::effective_command_with_floor(row, floor),
        Ok((Some(command), _)) if command == "/blueprint"
    )
}

#[cfg(test)]
pub(crate) fn parse_org_board_value(value: &Value) -> Option<OrgBoard> {
    parse_org_board_value_with_floor(value, crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR)
}

/// [`parse_org_board_value`] under the operator's `dispatch.blueprint_floor`.
/// The floor decides which unplanned rows the lifecycle table sends to
/// `/target`, and a `/target` row waits on spawn capacity like any other.
pub(crate) fn parse_org_board_value_with_floor(value: &Value, floor: &str) -> Option<OrgBoard> {
    let actionable = value.get("actionable")?.as_i64()?;
    let mut top_row = None;
    let mut actionable_ids: Vec<String> = Vec::new();
    let mut spawn_held_ids: Vec<String> = Vec::new();
    let mut blueprint_held_ids: Vec<String> = Vec::new();
    let mut blind_queues: Vec<String> = Vec::new();
    let mut operator_question_sessions: Vec<String> = Vec::new();
    let mut operator_questions_unreadable = false;
    let mut unreadable_sources = false;
    if let Some(queues) = value.get("queues").and_then(|q| q.as_array()) {
        for queue in queues {
            let name = queue.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let status = queue.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if crate::org_board::unreadable_status(status) {
                unreadable_sources = true;
            }
            if name == "operator_question" && crate::org_board::unreadable_status(status) {
                operator_questions_unreadable = true;
            }
            if crate::org_board::not_read_status(status) {
                let err = queue.get("error").and_then(|v| v.as_str()).unwrap_or("");
                blind_queues.push(if status == "over_budget" {
                    format!("{name} not read: {err}")
                } else {
                    format!("{name} is unreadable: {err}")
                });
                continue;
            }
            if name == "operator_question" {
                operator_question_sessions.extend(
                    queue
                        .get("rows")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|row| row.get("session_id").and_then(|v| v.as_str()))
                        .map(str::to_owned),
                );
            }
            if queue.get("actionable").and_then(|v| v.as_bool()) != Some(true) {
                continue;
            }
            let spawn_held = queue.get("verb").and_then(|v| v.as_str()) == Some("/fno:target");
            for row in queue
                .get("rows")
                .and_then(|v| v.as_array())
                .unwrap_or(&vec![])
            {
                // Row-level veto: a mergeable_pr row the merge gate
                // found not-ready carries `actionable: false` and names no
                // next action, however green the listing called it.
                if row.get("actionable").and_then(|v| v.as_bool()) == Some(false) {
                    continue;
                }
                let identity = row_identity(name, row);
                if top_row.is_none() {
                    top_row = Some(identity.clone());
                }
                // The unplanned queue is labelled /fno:blueprint as a whole, but
                // the lifecycle table sends each planless row by its own
                // difficulty. A row it sends to /target waits on spawn capacity;
                // a row it sends to /blueprint holds the stop open only through
                // the gate refusal or the blueprint ceiling.
                let routes_to_target =
                    name == "unplanned" && unplanned_routes_to_target(row, floor);
                let routes_to_blueprint =
                    name == "unplanned" && unplanned_routes_to_blueprint(row, floor);
                if spawn_held || routes_to_target {
                    spawn_held_ids.push(identity.clone());
                }
                if routes_to_blueprint {
                    blueprint_held_ids.push(identity.clone());
                }
                actionable_ids.push(identity);
            }
        }
    }
    Some(OrgBoard {
        actionable,
        top_row,
        unreadable_sources,
        actionable_ids,
        spawn_held_ids,
        blueprint_held_ids,
        blind_queues,
        operator_question_sessions,
        operator_questions_unreadable,
    })
}

/// The quiet journal row both blind-board blocks emit: a blind board must
/// still advance the fire counter with its row.
pub(crate) fn lead_quiet_body(session_id: &str, actionable: i64) -> Value {
    serde_json::json!({
        "session_id": session_id,
        "actionable": actionable,
        "actionable_ids": [],
        "cleared": false,
    })
}

/// The quiet-branch block message: what the drain read measured, or the
/// error when the read failed (the sentinel count i64::MAX rides beside it).
pub(crate) fn lead_quiet_message(
    undelivered: i64,
    drain_error: Option<&crate::loop_lead::ScopeDrainError>,
) -> String {
    match drain_error {
        Some(e) => {
            format!("board quiet but scope delivery is unreadable: {e}; blocking completion")
        }
        None => format!("board quiet; {undelivered} scope nodes still undelivered (drive each scope node to done or superseded to drain)"),
    }
}

/// The quiet-branch journal row: like `lead_quiet_body`, plus the drain read
/// it measured and whether that count shrank since the last fire.
pub(crate) fn lead_undelivered_body(session_id: &str, undelivered: i64, shrank: bool) -> Value {
    serde_json::json!({
        "session_id": session_id,
        "actionable": 0,
        "undelivered": undelivered,
        "actionable_ids": [],
        "cleared": shrank,
    })
}

/// The drain-reserve journal row: what the fire's last, reserved read cost
/// against the bound it was granted and the budget it started with. The
/// reserve is silent by design; this row is its series.
pub(crate) fn drain_reserve_body(
    session_id: &str,
    scope: &str,
    bound_ms: u64,
    elapsed_ms: u64,
    remaining_budget_ms: u64,
) -> Value {
    serde_json::json!({
        "session_id": session_id,
        "scope": scope,
        "bound_ms": bound_ms,
        "elapsed_ms": elapsed_ms,
        "remaining_budget_ms": remaining_budget_ms,
    })
}

/// The stop hook's one JSON verdict line: decision, reason, and the counts
/// a wrapper reads.
pub(crate) fn lead_output(
    decision: &str,
    reason: Option<TerminationReason>,
    message: &str,
    actionable: i64,
    fires: u64,
) -> String {
    serde_json::json!({
        "driver": "lead",
        "decision": decision,
        "termination_reason": reason,
        // `reason` carries the human-readable why, distinct from the enum
        // above: a stop hook reader wants the top actionable row, not a tag.
        "reason": message,
        "message": message,
        "actionable": actionable,
        "fires": fires,
    })
    .to_string()
}

pub(crate) fn read_org_board(
    fno_bin: &str,
    cwd: &Path,
    state_path: &Path,
    registry: Option<&crate::state::Registry>,
) -> Result<OrgBoard, String> {
    let _ = fno_bin;
    // Past the reserve line the board refuses instead of reading at the 1ms
    // bound: a board that never answered is the readable, bounded outcome a
    // spent fire owes the lead, and the drain's reserve stays untouched.
    if crate::loopcheck::stopgate_pre_drain_spent() {
        return Err("lead board not read: the fire budget was spent before the board".to_string());
    }
    let opts = crate::org_board::BoardOpts {
        budget_ms: crate::loopcheck::stopgate_read_timeout().as_millis() as u64,
        max_pr_reads: crate::org_board::DEFAULT_MAX_PR_READS,
        state_path: Some(state_path.to_path_buf()),
        cwd: Some(cwd.to_path_buf()),
        registry: registry.cloned(),
    };
    let payload = crate::org_board::read_board(&opts);
    let floor = crate::agents_config::config_lookup(cwd, &["dispatch", "blueprint_floor"])
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR.to_string());
    parse_org_board_value_with_floor(&payload, &floor).ok_or_else(|| {
        "unparseable board payload: the collector returned a shape parse_org_board_value cannot read"
            .to_string()
    })
}

/// The spawn gate's read-only verdict (`fno agents gate-status --json`):
/// whether a dispatch would be admitted right now, and which constraint binds.
pub(crate) struct GateProbe {
    pub(crate) verdict: String,
    pub(crate) reason: Option<String>,
    pub(crate) message: Option<String>,
}

pub(crate) fn parse_gate_probe(payload: &Value) -> Option<GateProbe> {
    let verdict = payload.get("verdict")?.as_str()?.to_string();
    Some(GateProbe {
        verdict,
        reason: payload
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        message: payload
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

impl GateProbe {
    /// The one-line constraint statement this verdict carries, for a stop-hook
    /// message: the probe's own sentence, else its reason token, else a bare
    /// statement that dispatch capacity is gone.
    pub(crate) fn constraint(&self) -> String {
        self.message
            .clone()
            .or_else(|| self.reason.clone())
            .unwrap_or_else(|| "dispatch capacity exhausted".to_string())
    }

    pub(crate) fn owner(&self) -> String {
        match self.reason.as_deref() {
            Some("fleet-stop" | "fleet-stop-unavailable") => {
                "fno agents incident status; fno agents incident clear --reason <text>".into()
            }
            Some("max_live" | "lead_share") => {
                "live workers exiting (`fno agents gate-status` slot_rows)".into()
            }
            Some("machine-runaway") => {
                "machine runaway brake (`fno agents gate-status`); it expires on its own".into()
            }
            Some("provider_cap" | "provider_quota_lock") => {
                "provider lanes (`fno-agents provider-cap status`)".into()
            }
            _ => "fno agents gate-status".into(),
        }
    }
}

/// Ask the gate the dispatch would ask. Any failure here is `Err`: the caller
/// must fall back to today's block, never read a broken probe as saturation.
pub(crate) fn probe_dispatch_capacity(fno_bin: &str, cwd: &Path) -> Result<GateProbe, String> {
    let out = crate::loopcheck::bounded_read(
        std::ffi::OsStr::new(fno_bin),
        &["agents", "gate-status"],
        cwd,
        "spawn gate status",
        crate::loopcheck::stopgate_read_timeout(),
    )
    .map_err(|error| format!("spawn gate status failed: {}", error.render()))?;
    if !out.status.success() {
        return Err(format!("spawn gate status exited {}", out.status));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let payload: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| "spawn gate status returned no JSON".to_string())?;
    parse_gate_probe(&payload).ok_or_else(|| "spawn gate status payload unparseable".to_string())
}

/// The saturation decision for one board fire. Two independent legs hold
/// rows: a refused gate holds the spawn-routed rows, and the blueprint
/// ceiling holds the board-order tail of blueprint-routed rows past the
/// check-in's `open` count regardless of the gate. A refused gate never
/// holds a blueprint-routed row: a blueprint runs through the session's own
/// subagent surface, spends no spawn share, and starts fine at a full seat
/// count. A readable non-dispatch row keeps the block pointed at it.
/// `probe: None` (not asked, or asked and failed) only disables the refusal
/// leg; a broken probe must never convert a block into an allow. A `None`
/// blueprint reading keeps the ceiling leg inert for the same reason: an
/// unmeasured ceiling never reads as waiting.
#[derive(Debug)]
pub(crate) enum SaturationOutcome {
    /// Every readable actionable row waits: on dispatch capacity (a refused
    /// gate), on the blueprint ceiling, or on both.
    Saturated { blocked: i64 },
    /// Every readable row waits, and at least one queue was not read.
    SaturatedBlind { blocked: i64 },
    /// Waiting rows exist but a non-dispatch row is still actionable,
    /// so the block survives - pointed at that row instead.
    BlockedWithNext { next: String, blocked: i64 },
}

pub(crate) fn saturation_verdict(
    board: &OrgBoard,
    probe: Option<&GateProbe>,
    blueprint: Option<&crate::lead_checkin_blueprint::BlueprintWait>,
) -> Option<SaturationOutcome> {
    let refused = probe.is_some_and(|p| p.verdict == "refused");
    let mut waiting: Vec<String> = Vec::new();
    if refused {
        waiting.extend(board.spawn_held_ids.iter().cloned());
    }
    if let Some(bp) = blueprint {
        // Board-order tail past `open`: the mirror of the check-in's
        // take(open)/skip(open), so the stop check and the beat always
        // name the same rows as startable.
        waiting.extend(board.blueprint_held_ids.iter().skip(bp.open).cloned());
    }
    if waiting.is_empty() {
        return None;
    }
    let blocked = waiting.len() as i64;
    let next = board.actionable_ids.iter().find(|id| !waiting.contains(id));
    if let Some(next) = next {
        return Some(SaturationOutcome::BlockedWithNext {
            next: next.clone(),
            blocked,
        });
    }
    if !board.blind_queues.is_empty() {
        return Some(SaturationOutcome::SaturatedBlind { blocked });
    }
    Some(SaturationOutcome::Saturated { blocked })
}

/// What the capacity gate decided for this fire, rendered and ready for the
/// caller's two verdicts. Composition of the probe read, the pure
/// saturation verdict, and the two messages the lead block carries.
#[derive(Debug)]
pub(crate) enum CapacityGate {
    /// Every readable actionable row waits and the caller may terminate
    /// NoWork: on a refused gate (the `/fno:target` queues plus the
    /// `/blueprint`-routed unplanned rows) or on a full blueprint ceiling.
    Saturated {
        message: String,
        blocked: i64,
        fires: u64,
    },
    /// Every readable row waits while at least one queue was not read.
    SaturatedBlind { message: String, actionable: i64 },
    /// Keep the block, pointed at a non-dispatch row, with the honest split.
    Split {
        message: String,
        actionable: i64,
        fires: u64,
        journal: Value,
    },
}

/// The one-clause statement of a blueprint ceiling: what runs, what the
/// count is, and who clears it. The stop check's naming of the running
/// blueprint.
fn blueprint_ceiling_clause(bp: &crate::lead_checkin_blueprint::BlueprintWait) -> String {
    let names = if bp.running.is_empty() {
        "none running".to_string()
    } else {
        bp.running.join(", ")
    };
    format!(
        "blueprint ceiling {running} of {ceiling}: {names}; owner: the running blueprint finishing",
        running = bp.running_count,
        ceiling = bp.ceiling,
    )
}

pub(crate) fn capacity_gate(
    board: &OrgBoard,
    fno_bin: &str,
    cwd: &Path,
    session_id: &str,
    dry: u64,
    _emit: &dyn Fn(&str, Value),
    blueprint: Option<&crate::lead_checkin_blueprint::BlueprintWait>,
) -> Option<CapacityGate> {
    let probe = if board.spawn_held_ids.is_empty() {
        None
    } else {
        match probe_dispatch_capacity(fno_bin, cwd) {
            Ok(p) => Some(p),
            // A failed probe keeps today's block, never reads as saturation.
            Err(_) => None,
        }
    };
    let refused = probe.as_ref().is_some_and(|p| p.verdict == "refused");
    let verdict = saturation_verdict(board, probe.as_ref(), blueprint)?;
    // The two waiting sides, counted separately so the message names what
    // each side waits on. The ceiling side needs no probe; the capacity side
    // exists only on a refusal.
    let capacity_waiting = if refused {
        board.spawn_held_ids.len() as i64
    } else {
        0
    };
    let ceiling_waiting = blueprint
        .map(|bp| board.blueprint_held_ids.len().saturating_sub(bp.open) as i64)
        .unwrap_or(0);
    let probe = probe.as_ref();
    let constraint = probe.map(|p| p.constraint()).unwrap_or_default();
    let owner = probe.map(|p| p.owner()).unwrap_or_default();
    let ceiling_hold = blueprint
        .map(|bp| format!("waiting on the {}", blueprint_ceiling_clause(bp)))
        .unwrap_or_default();
    let capacity_only = ceiling_waiting == 0 && capacity_waiting > 0;
    let ceiling_only = capacity_waiting == 0 && ceiling_waiting > 0;
    // Single-leg holds keep the legacy count-less shape; the mixed hold
    // names each side's count, so its message prefixes a colon instead of a
    // bare count that would read as a double count.
    let (hold, mixed) = if capacity_only {
        (
            format!("waiting on dispatch capacity ({constraint}; owner: {owner})"),
            false,
        )
    } else if ceiling_only {
        (ceiling_hold.clone(), false)
    } else {
        (
            format!(
                "{capacity_waiting} waiting on dispatch capacity ({constraint}; owner: {owner}) and \
                 {ceiling_waiting} {ceiling_hold}"
            ),
            true,
        )
    };
    match verdict {
        SaturationOutcome::Saturated { blocked } => {
            let message = if ceiling_waiting == 0 {
                format!(
                    "fleet saturated: {constraint}; \
                     {blocked} actionable rows all blocked on dispatch capacity; owner: {owner}"
                )
            } else if capacity_waiting == 0 {
                format!("{blocked} actionable rows all {ceiling_hold}")
            } else {
                format!(
                    "fleet saturated: {constraint}; \
                     {capacity_waiting} actionable rows blocked on dispatch capacity \
                     (owner: {owner}) and {ceiling_waiting} {ceiling_hold}"
                )
            };
            Some(CapacityGate::Saturated {
                message,
                blocked,
                fires: dry + 1,
            })
        }
        SaturationOutcome::SaturatedBlind { blocked } => {
            let blind = board.blind_queues.join(", ");
            let prefix = if mixed {
                format!("{blocked} rows:")
            } else {
                format!("{blocked} rows")
            };
            Some(CapacityGate::SaturatedBlind {
                message: format!("{prefix} {hold}; not read: {blind}"),
                actionable: board.actionable,
            })
        }
        SaturationOutcome::BlockedWithNext { next, blocked } => {
            let blind = if board.blind_queues.is_empty() {
                String::new()
            } else {
                format!("; not read: {}", board.blind_queues.join(", "))
            };
            let prefix = if mixed {
                format!("{blocked} rows:")
            } else {
                format!("{blocked}")
            };
            let message = format!(
                "{} actionable now; next: {next}; {prefix} {hold}{blind}",
                board.actionable - blocked,
            );
            let journal = serde_json::json!({
                "session_id": session_id,
                "actionable": board.actionable,
                "actionable_now": board.actionable - blocked,
                "blocked_on_capacity": blocked,
                "waiting_owner": owner,
                "actionable_ids": board.actionable_ids,
                "blueprint_ceiling": blueprint.map(|bp| {
                    serde_json::json!({"running": bp.running_count, "ceiling": bp.ceiling})
                }),
                "cleared": false,
            });
            Some(CapacityGate::Split {
                message,
                actionable: board.actionable,
                fires: dry + 1,
                journal,
            })
        }
    }
}

/// Why a blocking branch must stop instead of blocking again.
pub(crate) struct BoundBreach {
    pub(crate) reason: TerminationReason,
    pub(crate) message: String,
    pub(crate) fires: u64,
}

/// The bounds every blocking branch of `lead_decide` owes: the manifest
/// ceiling `--max-iterations` advertises, and the dry-fire backstop. One
/// function because a branch that grew its own copy of either lost both: the
/// quiet-board return sat above both and a team with undelivered scope
/// blocked forever, never reaching the parked state that asks the operator.
/// Budget is checked first, matching the ordering the ceiling branch commits
/// to: an exhausted lead reports the reason that actually stopped it.
pub(crate) fn bound_breached(
    total: u64,
    dry: u64,
    max_iterations: u64,
    waiting_on: &str,
) -> Option<BoundBreach> {
    if total + 1 >= max_iterations {
        return Some(BoundBreach {
            reason: TerminationReason::Budget,
            message: format!(
                "{} fires reached the manifest ceiling of {max_iterations}; {waiting_on}",
                total + 1
            ),
            fires: dry,
        });
    }
    if dry + 1 >= crate::loop_lead::LEAD_DRY_FIRE_CEILING {
        return Some(BoundBreach {
            reason: TerminationReason::NoProgress,
            message: format!("{} fires with nothing cleared; {waiting_on}", dry + 1),
            fires: dry + 1,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn board_with_queues(queues: Value) -> Value {
        json!({
            "actionable": 0,
            "queues": queues,
        })
    }

    #[test]
    fn an_unreadable_questions_queue_still_blocks_completion() {
        let board = board_with_queues(json!([
            {"name": "operator_question", "status": "unreadable", "error": "exit 1: boom",
             "actionable": true, "rows": []},
        ]));
        let parsed = parse_org_board_value(&board).unwrap();
        assert!(parsed.operator_questions_unreadable);
        assert!(parsed.unreadable_sources);
    }

    #[test]
    fn a_quiet_board_of_timeouts_stays_certifiable_and_names_the_reads() {
        // A quiet board whose only failures are timeouts must allow
        // the stop. The flag stays off, and the receipt names each timed-out
        // source so the allow is a named wait, never a silent clean.
        let board = board_with_queues(json!([
            {"name": "stale_claim", "status": "over_budget",
             "error": "truth probe: batch of 34 handles timed out",
             "actionable": true, "rows": []},
            {"name": "unheld_progress", "status": "over_budget",
             "error": "truth probe: batch of 34 handles timed out",
             "actionable": true, "rows": []},
        ]));
        let parsed = parse_org_board_value(&board).unwrap();
        assert!(!parsed.unreadable_sources);
        assert_eq!(
            parsed.not_read_receipt(),
            "; not read: stale_claim not read: truth probe: batch of 34 handles timed out, \
             unheld_progress not read: truth probe: batch of 34 handles timed out"
        );
    }

    #[test]
    fn a_not_ready_mergeable_row_names_no_next_action() {
        // the stop hook offered mergeable_pr:1709 on four
        // consecutive stops while the merge gate refused that exact head.
        // The not-ready row names no next action.
        let board = board_with_queues(json!([
            {"name": "mergeable_pr", "status": "ok", "actionable": true,
             "count": 0, "rows": [
                {"number": 1709, "title": "green but held", "ready": false,
                 "actionable": false, "ready_blockers": ["review_in_flight"]},
                {"number": 1702},
            ]},
        ]));
        let parsed = parse_org_board_value(&board).unwrap();
        let top = parsed.top_row.unwrap();
        assert_eq!(top, "mergeable_pr:1702", "{top}");
        assert_eq!(parsed.actionable_ids, vec!["mergeable_pr:1702".to_string()]);
    }

    fn board_undispatched_only() -> OrgBoard {
        parse_org_board_value(&board_with_queues(json!([
            {"name": "undispatched", "status": "ok", "actionable": true,
             "verb": "/fno:target", "rows": [{"id": "x-1"}, {"id": "x-2"}]},
        ])))
        .unwrap()
    }

    fn refused_probe() -> GateProbe {
        GateProbe {
            verdict: "refused".to_string(),
            reason: Some("provider_cap".to_string()),
            message: Some("every dispatch lane at cap: zai 10/10".to_string()),
        }
    }

    fn gate_status_stub(dir: &Path) -> PathBuf {
        let payload = json!({
            "verdict": "refused",
            "reason": "max_live",
            "message": "15 live worker slots >= max_live 15",
        });
        crate::write_exec_stub(
            dir,
            "fno",
            &format!("#!/bin/sh\nprintf '%s\\n' '{payload}'\n"),
        )
    }

    #[test]
    fn a_refused_probe_with_a_non_dispatch_row_points_the_block_at_it() {
        let board = parse_org_board_value(&board_with_queues(json!([
            {"name": "undispatched", "status": "ok", "actionable": true,
             "verb": "/fno:target", "rows": [{"id": "x-1"}, {"id": "x-2"}]},
            {"name": "unplanned", "status": "ok", "actionable": true,
             "verb": "/fno:blueprint",
             "rows": [{"id": "x-3"}]},
        ])))
        .unwrap();
        let verdict =
            saturation_verdict(&board, Some(&refused_probe()), None).expect("blocked with next");
        match verdict {
            SaturationOutcome::BlockedWithNext { next, blocked } => {
                assert_eq!(next, "unplanned:x-3");
                assert_eq!(blocked, 2);
            }
            other => panic!("expected BlockedWithNext, got {other:?}"),
        }
    }

    #[test]
    fn refused_probe_saturates_unheld_progress_and_undriven_pr() {
        let board = parse_org_board_value(&json!({
            "actionable": 2,
            "queues": [
                {"name": "unheld_progress", "status": "ok", "actionable": true,
                 "verb": "/fno:target", "rows": [{"id": "x-1"}]},
                {"name": "undriven_pr", "status": "ok", "actionable": true,
                 "verb": "/fno:target", "rows": [{"number": 2398}]},
            ],
        }))
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let fno = gate_status_stub(tmp.path());
        let emit = |_: &str, _: Value| {};

        let gate = capacity_gate(
            &board,
            fno.to_str().unwrap(),
            tmp.path(),
            "lead",
            0,
            &emit,
            None,
        )
        .expect("a refused probe must classify the spawn-held queues");
        match gate {
            CapacityGate::Saturated {
                blocked, message, ..
            } => {
                assert_eq!(blocked, 2);
                assert!(message.contains("live workers exiting"), "{message}");
            }
            _ => panic!("expected all spawn-held rows to be saturated"),
        }
    }

    #[test]
    fn a_refused_probe_with_only_unplanned_low_rows_ends_the_lead_saturated() {
        // The lifecycle table sends a planless low or medium node to /target,
        // so a refused gate holds it exactly like undispatched work.
        let board = parse_org_board_value_with_floor(
            &json!({
                "actionable": 2,
                "queues": [
                    {"name": "unplanned", "status": "ok", "actionable": true,
                     "verb": "/fno:blueprint", "rows": [ // verb-wording: allow
                        {"id": "x-1", "difficulty": "low"},
                        {"id": "x-2", "difficulty": "medium"},
                    ]},
                ],
            }),
            crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR,
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let fno = gate_status_stub(tmp.path());
        let emit = |_: &str, _: Value| {};

        let gate = capacity_gate(
            &board,
            fno.to_str().unwrap(),
            tmp.path(),
            "lead",
            0,
            &emit,
            None,
        )
        .expect("a refused probe must hold unplanned low rows");
        match gate {
            CapacityGate::Saturated { blocked, .. } => assert_eq!(blocked, 2),
            _ => panic!("expected unplanned low rows to saturate, not block on a blueprint"),
        }
    }

    #[test]
    fn a_refused_gate_with_a_full_ceiling_names_both_holds() {
        // Both legs at once: the gate holds the undispatched row, the full
        // ceiling holds every blueprint row, and the saturated message names
        // each side and the running blueprint.
        let board = parse_org_board_value_with_floor(
            &json!({
                "actionable": 3,
                "queues": [
                    {"name": "undispatched", "status": "ok", "actionable": true,
                     "verb": "/fno:target", "rows": [{"id": "x-1"}]}, // verb-wording: allow
                    {"name": "unplanned", "status": "ok", "actionable": true,
                     "verb": "/fno:blueprint", "rows": [
                        {"id": "x-2", "difficulty": "high"},
                        {"id": "x-3", "difficulty": "high"},
                    ]}, // verb-wording: allow
                ],
            }),
            crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR,
        )
        .unwrap();
        let bp = crate::lead_checkin_blueprint::BlueprintWait {
            running: vec!["x-999".to_string()],
            running_count: 1,
            ceiling: 1,
            open: 0,
        };
        let tmp = tempfile::tempdir().unwrap();
        let fno = gate_status_stub(tmp.path());
        let emit = |_: &str, _: Value| {};
        let gate = capacity_gate(
            &board,
            fno.to_str().unwrap(),
            tmp.path(),
            "lead",
            0,
            &emit,
            Some(&bp),
        )
        .expect("both holds must saturate");
        match gate {
            CapacityGate::Saturated {
                blocked, message, ..
            } => {
                assert_eq!(blocked, 3);
                assert!(
                    message.contains("dispatch capacity"),
                    "the capacity side must be named: {message}"
                );
                assert!(
                    message.contains("blueprint ceiling 1 of 1") && message.contains("x-999"),
                    "the ceiling side and its running blueprint must be named: {message}"
                );
            }
            other => panic!("expected Saturated, got {other:?}"),
        }
    }

    #[test]
    fn an_open_gate_with_an_open_ceiling_keeps_the_blueprint_row_actionable() {
        // AC1-EDGE / AC2-EDGE: no refusal and no readable ceiling keeps
        // today's healthy block pointed at the blueprint row; an unmeasured
        // ceiling never converts actionable into waiting.
        let board = parse_org_board_value_with_floor(
            &json!({
                "actionable": 1,
                "queues": [
                    {"name": "unplanned", "status": "ok", "actionable": true,
                     "verb": "/fno:blueprint", "rows": [{"id": "x-2", "difficulty": "high"}]}, // verb-wording: allow
                ],
            }),
            crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR,
        )
        .unwrap();
        assert!(saturation_verdict(&board, None, None).is_none());
        assert!(saturation_verdict(&board, Some(&accepted_probe()), None).is_none());
    }

    #[test]
    fn a_ceiling_full_reading_holds_the_board_order_tail() {
        // AC2-HP tail form: ceiling 1, none running, open 1 - the first
        // blueprint row in board order stays startable, the tail waits, and
        // the message names the ceiling.
        let board = parse_org_board_value_with_floor(
            &json!({
                "actionable": 2,
                "queues": [
                    {"name": "unplanned", "status": "ok", "actionable": true,
                     "verb": "/fno:blueprint", "rows": [
                        {"id": "x-2", "difficulty": "high"},
                        {"id": "x-3", "difficulty": "high"},
                    ]}, // verb-wording: allow
                ],
            }),
            crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR,
        )
        .unwrap();
        let bp = crate::lead_checkin_blueprint::BlueprintWait {
            running: vec![],
            running_count: 0,
            ceiling: 1,
            open: 1,
        };
        let verdict = saturation_verdict(&board, Some(&accepted_probe()), Some(&bp))
            .expect("the tail past open must wait");
        match verdict {
            SaturationOutcome::BlockedWithNext { next, blocked } => {
                assert_eq!(next, "unplanned:x-2");
                assert_eq!(blocked, 1);
            }
            other => panic!("expected BlockedWithNext, got {other:?}"),
        }
        let tmp = tempfile::tempdir().unwrap();
        let fno = accepted_probe_stub(tmp.path());
        let emit = |_: &str, _: Value| {};
        let gate = capacity_gate(
            &board,
            fno.to_str().unwrap(),
            tmp.path(),
            "lead",
            0,
            &emit,
            Some(&bp),
        )
        .expect("the ceiling split must render");
        match gate {
            CapacityGate::Split { message, .. } => {
                assert!(
                    message.contains("blueprint ceiling 0 of 1"),
                    "the ceiling must be named: {message}"
                );
                assert!(message.contains("next: unplanned:x-2"), "{message}");
            }
            other => panic!("expected Split, got {other:?}"),
        }
    }

    #[test]
    fn a_ceiling_full_reading_saturates_and_names_the_running_blueprint() {
        // AC2-HP saturated form: open 0 holds every blueprint row, and the
        // message names the running blueprint by node id.
        let board = parse_org_board_value_with_floor(
            &json!({
                "actionable": 2,
                "queues": [
                    {"name": "unplanned", "status": "ok", "actionable": true,
                     "verb": "/fno:blueprint", "rows": [
                        {"id": "x-2", "difficulty": "high"},
                        {"id": "x-3", "difficulty": "high"},
                    ]}, // verb-wording: allow
                ],
            }),
            crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR,
        )
        .unwrap();
        let bp = crate::lead_checkin_blueprint::BlueprintWait {
            running: vec!["x-999".to_string()],
            running_count: 1,
            ceiling: 1,
            open: 0,
        };
        let tmp = tempfile::tempdir().unwrap();
        let fno = accepted_probe_stub(tmp.path());
        let emit = |_: &str, _: Value| {};
        let gate = capacity_gate(
            &board,
            fno.to_str().unwrap(),
            tmp.path(),
            "lead",
            0,
            &emit,
            Some(&bp),
        )
        .expect("a full ceiling must saturate");
        match gate {
            CapacityGate::Saturated {
                blocked, message, ..
            } => {
                assert_eq!(blocked, 2);
                assert!(
                    message.contains("blueprint ceiling 1 of 1") && message.contains("x-999"),
                    "the running blueprint must be named: {message}"
                );
            }
            other => panic!("expected Saturated, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_split_holds_only_spawn_rows_and_points_at_the_blueprint_row() {
        // The refusal leg holds spawn-routed rows only. A blueprint runs
        // through the session's own subagent surface, spends no spawn share,
        // and starts fine at a full seat count: a blueprint-only board with
        // an open ceiling never saturates on a worker-capacity refusal, and
        // a mixed board keeps the blueprint row as the actionable next row
        // the block points at.
        let blueprint_only = parse_org_board_value_with_floor(
            &json!({
                "actionable": 2,
                "queues": [
                    {"name": "unplanned", "status": "ok", "actionable": true,
                     "verb": "/fno:blueprint", "rows": [
                        {"id": "x-1", "difficulty": "high"},
                        {"id": "x-2", "difficulty": "high"},
                    ]}, // verb-wording: allow
                ],
            }),
            crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR,
        )
        .unwrap();
        assert!(saturation_verdict(&blueprint_only, Some(&refused_probe()), None).is_none());
        let board = parse_org_board_value_with_floor(
            &json!({
                "actionable": 3,
                "queues": [
                    {"name": "undispatched", "status": "ok", "actionable": true,
                     "verb": "/fno:target", "rows": [{"id": "x-1"}]}, // verb-wording: allow
                    {"name": "unplanned", "status": "ok", "actionable": true,
                     "verb": "/fno:blueprint", "rows": [{"id": "x-2", "difficulty": "high"}]}, // verb-wording: allow
                    {"name": "mergeable_pr", "status": "ok", "actionable": true,
                     "count": 1, "rows": [{"number": 1702}]},
                ],
            }),
            crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR,
        )
        .unwrap();
        let verdict =
            saturation_verdict(&board, Some(&refused_probe()), None).expect("blocked with next");
        match verdict {
            SaturationOutcome::BlockedWithNext { next, blocked } => {
                assert_eq!(next, "unplanned:x-2");
                assert_eq!(blocked, 1);
            }
            other => panic!("expected BlockedWithNext, got {other:?}"),
        }
    }

    fn accepted_probe() -> GateProbe {
        GateProbe {
            verdict: "accepted".to_string(),
            reason: None,
            message: None,
        }
    }

    fn accepted_probe_stub(dir: &Path) -> PathBuf {
        let payload = json!({
            "verdict": "accepted",
        });
        crate::write_exec_stub(
            dir,
            "fno",
            &format!("#!/bin/sh\nprintf '%s\\n' '{payload}'\n"),
        )
    }

    #[test]
    fn a_blind_board_with_only_spawn_held_rows_is_saturated_blind() {
        let board = parse_org_board_value(&json!({
            "actionable": 2,
            "queues": [
                {"name": "unheld_progress", "status": "ok", "actionable": true,
                 "verb": "/fno:target", "rows": [{"id": "x-1"}]},
                {"name": "undriven_pr", "status": "ok", "actionable": true,
                 "verb": "/fno:target", "rows": [{"number": 2398}]},
                {"name": "unplanned", "status": "unreadable", "actionable": true,
                 "error": "truth probe timed out", "rows": []},
            ],
        }))
        .unwrap();
        let verdict =
            saturation_verdict(&board, Some(&refused_probe()), None).expect("saturated blind");
        match verdict {
            SaturationOutcome::SaturatedBlind { blocked } => assert_eq!(blocked, 2),
            _ => panic!("expected a blind saturation verdict"),
        }
        let tmp = tempfile::tempdir().unwrap();
        let fno = gate_status_stub(tmp.path());
        let emit = |_: &str, _: Value| {};
        let gate = capacity_gate(
            &board,
            fno.to_str().unwrap(),
            tmp.path(),
            "lead",
            0,
            &emit,
            None,
        )
        .expect("the blind saturation remains a gate result");
        match gate {
            CapacityGate::SaturatedBlind {
                message,
                actionable,
            } => {
                assert_eq!(actionable, 2);
                assert!(
                    message.contains("not read: unplanned is unreadable"),
                    "{message}"
                );
                assert!(message.contains("live workers exiting"), "{message}");
            }
            _ => panic!("expected blind saturation to stay distinct from NoWork"),
        }
    }

    #[test]
    fn a_fleet_stop_probe_names_the_incident_owner() {
        let probe = GateProbe {
            verdict: "refused".into(),
            reason: Some("fleet-stop".into()),
            message: None,
        };
        let owner = probe.owner();
        assert!(owner.contains("fno agents incident status"), "{owner}");
        assert!(
            owner.contains("fno agents incident clear --reason"),
            "{owner}"
        );
    }

    #[test]
    fn a_broken_probe_never_allows_the_stop() {
        let board = board_undispatched_only();
        assert!(saturation_verdict(&board, None, None).is_none());
    }

    #[test]
    fn the_manifest_ceiling_breaches_budget_on_the_advertised_fire() {
        let b = bound_breached(39, 0, 40, "3 scope nodes still undelivered").expect("breach");
        assert_eq!(b.reason, TerminationReason::Budget);
        assert!(
            b.message.contains("ceiling of 40")
                && b.message.contains("3 scope nodes still undelivered"),
            "{}",
            b.message
        );
        assert_eq!(b.fires, 0);
    }

    #[test]
    fn the_dry_backstop_breaches_noprogress_on_the_third_quiet_fire() {
        let b = bound_breached(0, 2, 40, "waiting").expect("breach");
        assert_eq!(b.reason, TerminationReason::NoProgress);
        assert_eq!(b.fires, 3);
    }
}
