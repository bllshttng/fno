//! The check-in's blueprint reading: which unplanned nodes to blueprint and
//! which to route straight to target. Extracted from lead_checkin.rs under
//! the file budget's shrink rule; the beat's print block reads the JSON this
//! module returns. Each unplanned row routes through the lifecycle command
//! table (`dispatch.blueprint_floor`), so the beat's starts list can never
//! disagree with the routing the dispatch doors answer.
use serde_json::{json, Value};

use std::collections::HashMap;
use std::path::Path;

/// The unplanned rows this team must not dispatch: node id -> the canonical
/// scope of the live team that owns it, other than this team's own. The
/// fold's owner read is the one instrument; an unread read is the caller's
/// error, never an empty map that would let another lead's node print.
pub(crate) fn other_owned_scopes(
    folded: &Result<Value, String>,
    scope: &str,
) -> Result<HashMap<String, String>, String> {
    let folded = folded.as_ref().map_err(Clone::clone)?;
    let mine = crate::territory::canonical_scope(scope);
    let scopes = folded
        .get("owned_scopes")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            folded
                .get("fold")
                .and_then(|f| f.get("owned_reason"))
                .and_then(Value::as_str)
                .unwrap_or("the fold names no owner map")
                .to_string()
        })?;
    Ok(scopes
        .iter()
        .filter_map(|(id, sc)| {
            sc.as_str()
                .filter(|s| *s != mine)
                .map(|s| (id.clone(), s.to_string()))
        })
        .collect())
}

/// The default blueprint-subagent ceiling when no provider budget applies
/// (no new config key: a registry row is barred, so the default is a named
/// Rust constant). One per lead: law d-6eb2cbbd.
pub(crate) const DEFAULT_BLUEPRINT_CEILING: usize = 1;

/// The check-in's blueprint reading: this session's live blueprint
/// subagents against the ceiling, then which unplanned nodes to start and
/// which to skip. Each unplanned row is routed through the lifecycle command
/// table first: a row whose command resolves to /blueprint is a candidate, one
/// that resolves to /target prints as target-ready (no blueprint ceiling
/// applies), and an undecidable row skips with the refusal. `floor` is the
/// operator's `dispatch.blueprint_floor`; the claim list and the session id
/// arrive through the seam (arguments, not ambient reads) so the unit tests
/// need no claims directory. A failed source is this reading's error, never
/// a zero: an unreadable claim list read as `running 0` would name starts
/// past the ceiling. Starts also wait until plans ready fall below the
/// lead's worker slots - a blueprint nobody can build is the exact spend the
/// wake meter exists to name. `other_owned` is the fold's owner read, the
/// nodes a deeper live team holds: the scope line counts owned active with
/// it, so the starts and target-ready lines answer with the same one and a
/// child of another lead's team never prints. An unread map is this
/// reading's error - an empty read would name starts in foreign territory.
/// `idea_children` counts the scope's idea-status children riding their
/// contained epics: work the selection refuses (a contained child never
/// dispatches alone) that an empty unplanned queue must not read as clean.
pub(crate) fn r_blueprint(
    board: &Result<Value, String>,
    cwd: &Path,
    session_id: Option<String>,
    claims: Result<Vec<String>, String>,
    slots: Result<usize, String>,
    floor: &str,
    other_owned: &Result<HashMap<String, String>, String>,
    idea_children: &Result<usize, String>,
) -> Result<Value, String> {
    let session_id = session_id
        .ok_or_else(|| "no session id; cannot count this lead's blueprint subagents".to_string())?;
    let holders = claims?;
    let other_owned = other_owned.as_ref().map_err(Clone::clone)?;
    let holder = format!("blueprint-session:{session_id}");
    let running = holders.iter().filter(|h| **h == holder).count();
    let board = board.as_ref()?;
    let rows = crate::lead_checkin::board_queue(board, "unplanned")?
        .get("rows")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let mut candidates: Vec<String> = Vec::new();
    let mut target_ready: Vec<String> = Vec::new();
    let mut skips: Vec<Value> = Vec::new();
    for row in &rows {
        // A row with no string id answers no command: skip it, never dispatch
        // an "unknown" node the way a silent default would invite.
        let id = match row.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                skips.push(json!({"id": null, "reason": "row carries no id"}));
                continue;
            }
        };
        if let Some(owner) = other_owned.get(&id) {
            skips.push(json!({"id": id, "reason": format!("owned by team {owner}")}));
            continue;
        }
        match crate::backlog_ready::effective_command_with_floor(row, floor) {
            Ok((command, _)) if command.as_deref() == Some("/blueprint") => candidates.push(id),
            Ok((command, _)) if command.as_deref() == Some("/target") => target_ready.push(id),
            Ok((_, note)) => skips.push(json!({"id": id, "reason": note})),
            Err(refusal) => skips.push(json!({"id": id, "reason": refusal})),
        }
    }
    let provider = std::env::var("FNO_ROUTE_PROVIDER").unwrap_or_default();
    let provider_cap = crate::spawn_gate_lanes::provider_subagents_cap(cwd, &provider);
    // A provider budget can only lower the one-per-lead ceiling, never raise it.
    let (ceiling, ceiling_source) = match provider_cap {
        Some(cap) if cap < DEFAULT_BLUEPRINT_CEILING => {
            (cap, format!("agents.provider_limits.{provider}.subagents"))
        }
        _ => (DEFAULT_BLUEPRINT_CEILING, "one per lead".to_string()),
    };
    let plans_ready = crate::lead_checkin::board_queue(board, "undispatched")
        .ok()
        .and_then(|q| q.get("count").and_then(Value::as_u64).map(|c| c as usize));
    let (pr_v, slots_v, gate_reason): (Value, Value, Option<String>) = match (plans_ready, &slots) {
        (Some(pr), Ok(s)) => {
            if pr < *s {
                (json!(pr), json!(s), None)
            } else {
                (
                    json!(pr),
                    json!(s),
                    Some(format!(
                        "plans ready {pr} / slots {s}: build slots are the bottleneck"
                    )),
                )
            }
        }
        (None, Ok(_)) => (
            Value::Null,
            slots.clone().map(|s| json!(s)).unwrap_or(Value::Null),
            Some("plans ready unmeasured: the board names no undispatched count".to_string()),
        ),
        (_, Err(e)) => (
            Value::Null,
            Value::Null,
            Some(format!("plans ready unmeasured: {e}")),
        ),
    };
    let open = if gate_reason.is_some() {
        0
    } else {
        ceiling.saturating_sub(running)
    };
    let starts: Vec<String> = candidates.iter().take(open).cloned().collect();
    match &gate_reason {
        Some(reason) => candidates
            .iter()
            .for_each(|id| skips.push(json!({"id": id, "reason": reason}))),
        None => candidates.iter().skip(open).for_each(|id| {
            skips.push(json!({"id": id, "reason": format!("at ceiling {running} of {ceiling}")}))
        }),
    };
    if rows.is_empty() && running == 0 {
        let reason = match idea_children {
            Ok(n) if *n > 0 => format!(
                "no dispatchable unplanned node in scope: {n} idea children ride their contained epics"
            ),
            _ => "no unplanned node in scope".to_string(),
        };
        skips.push(json!({ "id": null, "reason": reason }));
    }
    let idea_children_json = match idea_children {
        Ok(n) => json!(n),
        Err(e) => json!({ "error": e }),
    };
    Ok(json!({
        "running": running,
        "ceiling": ceiling,
        "ceiling_source": ceiling_source,
        "plans_ready": pr_v,
        "slots": slots_v,
        "starts": starts,
        "target_ready": target_ready,
        "idea_children": idea_children_json,
        "skips": skips,
    }))
}

/// The stop check's blueprint-ceiling reading: the same ceiling the
/// check-in's `r_blueprint` reads, against the same claims source, so a row
/// the beat skips at ceiling cannot hold the lead's stop open. `running`
/// names the running blueprints by node id; `open` is the count of
/// board-order unplanned rows that may still start. `None` on an unmeasured
/// read: the check-in fails closed so no start is named past the ceiling;
/// the stop check fails inert so no startable row reads as waiting, which
/// would end a lead with work it could have started.
pub(crate) struct BlueprintWait {
    pub(crate) running: Vec<String>,
    pub(crate) running_count: usize,
    pub(crate) ceiling: usize,
    pub(crate) open: usize,
}

/// The ceiling fold the check-in runs: a provider budget can only lower the
/// one-per-lead ceiling, never raise it. One function so the stop check and
/// the check-in cannot drift.
pub(crate) fn blueprint_ceiling(provider_cap: Option<usize>) -> usize {
    match provider_cap {
        Some(cap) if cap < DEFAULT_BLUEPRINT_CEILING => cap,
        _ => DEFAULT_BLUEPRINT_CEILING,
    }
}

/// The node ids behind this session's live `blueprint-session:` claims, read
/// from the same listing the check-in counts holders in.
pub(crate) fn running_blueprint_ids(
    rows: &[crate::claims::ClaimRecord],
    session: &str,
) -> Vec<String> {
    let holder = format!("blueprint-session:{session}");
    rows.iter()
        .filter(|r| r.holder == holder)
        .filter_map(|r| r.key.strip_prefix("node:").map(str::to_string))
        .collect()
}

/// Read the ceiling state live. `None` when the session id or the claims
/// listing is unreadable: an unmeasured ceiling never converts an
/// actionable row into a waiting one.
pub(crate) fn stop_check_reading(cwd: &Path) -> Option<BlueprintWait> {
    let (session, _) = crate::claims::resolve_identity();
    let session = session?;
    let rows = crate::claims::list(Some("node:"), None, false).ok()?;
    let running = running_blueprint_ids(&rows, &session);
    let provider = std::env::var("FNO_ROUTE_PROVIDER").unwrap_or_default();
    let provider_cap = crate::spawn_gate_lanes::provider_subagents_cap(cwd, &provider);
    let ceiling = blueprint_ceiling(provider_cap);
    let running_count = running.len();
    Some(BlueprintWait {
        running,
        running_count,
        ceiling,
        open: ceiling.saturating_sub(running_count),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A board payload naming one `unplanned` queue whose rows carry
    /// `(id, difficulty)` and, when `plans_ready` is Some, one `undispatched`
    /// queue counting it.
    fn unplanned_board(rows: &[(&str, &str)], plans_ready: Option<usize>) -> Value {
        let mut queues = vec![json!({
            "name": "unplanned", "status": "ok", "count": rows.len(),
            "rows": rows.iter().map(|(id, d)| json!({"id": id, "difficulty": d})).collect::<Vec<_>>()
        })];
        if let Some(pr) = plans_ready {
            queues.push(json!({
                "name": "undispatched", "status": "ok", "count": pr, "rows": []
            }));
        }
        json!({"queues": queues})
    }

    /// Call `r_blueprint` with a fixed provider (so the ambient
    /// FNO_ROUTE_PROVIDER cannot leak in) and a temp config dir.
    fn blueprint_reading(
        dir: &std::path::Path,
        config_toml: &str,
        board: Value,
        session_id: Option<&str>,
        holders: Vec<String>,
        slots: Result<usize, String>,
        floor: &str,
    ) -> Result<Value, String> {
        blueprint_reading_owned(
            dir,
            config_toml,
            board,
            session_id,
            holders,
            slots,
            floor,
            Ok(HashMap::new()),
            Ok(0),
        )
    }

    /// The same call with the owner map named, for the deeper-team filter,
    /// and the idea-children count named, for the epic-heavy line.
    fn blueprint_reading_owned(
        dir: &std::path::Path,
        config_toml: &str,
        board: Value,
        session_id: Option<&str>,
        holders: Vec<String>,
        slots: Result<usize, String>,
        floor: &str,
        other_owned: Result<HashMap<String, String>, String>,
        idea_children: Result<usize, String>,
    ) -> Result<Value, String> {
        // The FNO_CONFIG pin below is the SOLE config candidate for every
        // concurrent reader, so it must not interleave with another test's
        // config read (the spawn-gate reservation suite reads max_live under
        // this same lock).
        let _env_lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let fnodir = dir.join(".fno");
        std::fs::create_dir_all(&fnodir).unwrap();
        std::fs::write(fnodir.join("config.toml"), config_toml).unwrap();
        // FNO_CONFIG pins the walk to the fixture the way the lanes-cap test
        // does: a repo-root .fno/config.toml must never leak into the read.
        let prior_config = std::env::var_os("FNO_CONFIG");
        std::env::set_var("FNO_CONFIG", fnodir.join("config.toml"));
        let prior_provider = std::env::var_os("FNO_ROUTE_PROVIDER");
        std::env::set_var("FNO_ROUTE_PROVIDER", "zai");
        let reading = r_blueprint(
            &Ok(board),
            dir,
            session_id.map(str::to_string),
            Ok(holders),
            slots,
            floor,
            &other_owned,
            &idea_children,
        );
        match prior_provider {
            Some(v) => std::env::set_var("FNO_ROUTE_PROVIDER", v),
            None => std::env::remove_var("FNO_ROUTE_PROVIDER"),
        }
        match prior_config {
            Some(v) => std::env::set_var("FNO_CONFIG", v),
            None => std::env::remove_var("FNO_CONFIG"),
        }
        reading
    }

    /// The happy path: 4 unplanned candidates, running 0, ceiling capped to
    /// one per lead even under a higher provider budget: only the first id in
    /// board order starts, the rest skip at the ceiling.
    #[test]
    fn r_blueprint_starts_up_to_the_ceiling_in_board_order() {
        let dir = std::env::temp_dir().join(format!("fno-bp-happy-{}", std::process::id()));
        let reading = blueprint_reading(
            &dir,
            "[agents.provider_limits.zai]\nsubagents = 2\n",
            unplanned_board(
                &[
                    ("x-1", "high"),
                    ("x-2", "high"),
                    ("x-3", "high"),
                    ("x-4", "high"),
                ],
                Some(0),
            ),
            Some("sess-1"),
            vec![],
            Ok(4),
            "high",
        )
        .unwrap();
        assert_eq!(reading["running"], 0);
        assert_eq!(reading["ceiling"], 1);
        assert_eq!(reading["ceiling_source"], "one per lead");
        assert_eq!(reading["plans_ready"], 0);
        assert_eq!(reading["slots"], 4);
        assert_eq!(reading["starts"], json!(["x-1"]), "{reading}");
        assert_eq!(
            reading["skips"],
            json!([
                {"id": "x-2", "reason": "at ceiling 0 of 1"},
                {"id": "x-3", "reason": "at ceiling 0 of 1"},
                {"id": "x-4", "reason": "at ceiling 0 of 1"}
            ]),
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AC6: plans ready at or above slots stops every start.
    #[test]
    fn r_blueprint_skips_everything_when_build_slots_are_the_bottleneck() {
        let dir = std::env::temp_dir().join(format!("fno-bp-bottl-{}", std::process::id()));
        let reading = blueprint_reading(
            &dir,
            "",
            unplanned_board(
                &[("x-1", "high"), ("x-2", "high"), ("x-3", "high")],
                Some(5),
            ),
            Some("sess-1"),
            vec![],
            Ok(4),
            "high",
        )
        .unwrap();
        assert_eq!(reading["running"], 0);
        assert_eq!(reading["starts"], json!([]));
        assert_eq!(reading["skips"].as_array().unwrap().len(), 3);
        assert_eq!(
            reading["skips"][0]["reason"],
            "plans ready 5 / slots 4: build slots are the bottleneck",
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AC8: an unreadable undispatched queue reads plans ready unmeasured,
    /// and no candidate starts on an unmeasured gate.
    #[test]
    fn r_blueprint_skips_everything_when_plans_ready_unmeasured() {
        let dir = std::env::temp_dir().join(format!("fno-bp-unmeas-{}", std::process::id()));
        let reading = blueprint_reading(
            &dir,
            "",
            unplanned_board(&[("x-1", "high"), ("x-2", "high")], None),
            Some("sess-1"),
            std::vec![],
            Ok(4),
            "high",
        )
        .unwrap();
        assert_eq!(reading["starts"], json!([]), "{reading}");
        assert_eq!(
            reading["skips"][0]["reason"],
            "plans ready unmeasured: the board names no undispatched count",
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing session id fails the reading: it must never read as
    /// `running 0`, which would name starts past the ceiling.
    #[test]
    fn r_blueprint_fails_without_a_session_id() {
        let dir = std::env::temp_dir().join(format!("fno-bp-nosess-{}", std::process::id()));
        let reading = blueprint_reading(
            &dir,
            "",
            unplanned_board(&[("x-1", "high")], Some(0)),
            None,
            vec![],
            Ok(4),
            "high",
        );
        assert!(reading.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A declared command outranks the derived answer: an out-of-family
    /// declaration abstains into a skip, a declared /target wins over the
    /// derived /blueprint, and a declared /blueprint stays a candidate.
    #[test]
    fn r_blueprint_honors_a_declared_command_over_the_derived_answer() {
        let dir = std::env::temp_dir().join(format!("fno-bp-decl-{}", std::process::id()));
        let board = json!({"queues": [
            {"name": "unplanned", "status": "ok", "count": 3,
             "rows": [
                {"id": "x-think", "difficulty": "high", "dispatch_command": "/fno:think"},
                {"id": "x-tgt", "difficulty": "low", "dispatch_command": "/target"},
                {"id": "x-bp", "difficulty": "low", "dispatch_command": "/fno:blueprint"}
            ]},
            {"name": "undispatched", "status": "ok", "count": 0, "rows": []}
        ]});
        let reading =
            blueprint_reading(&dir, "", board, Some("sess-1"), vec![], Ok(4), "high").unwrap();
        assert_eq!(reading["starts"], json!(["x-bp"]), "{reading}");
        assert_eq!(reading["target_ready"], json!(["x-tgt"]), "{reading}");
        assert_eq!(reading["skips"].as_array().unwrap().len(), 1, "{reading}");
        assert_eq!(reading["skips"][0]["id"], json!("x-think"), "{reading}");
        assert!(
            reading["skips"][0]["reason"]
                .as_str()
                .unwrap_or("")
                .contains("out-of-family"),
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A child of a deeper live team never prints, on either dispatch line:
    /// on 2026-09-28 two p0 children of an L2-held epic leaked as
    /// target-ready past the outer team's beat, and a lead that trusted the
    /// line would have seated a duplicate worker in another lead's territory.
    /// The row skips naming the team that owns it.
    #[test]
    fn a_child_of_a_deeper_live_team_never_prints_as_start_or_target_ready() {
        let dir = std::env::temp_dir().join(format!("fno-bp-owned-{}", std::process::id()));
        let reading = blueprint_reading_owned(
            &dir,
            "",
            unplanned_board(
                &[("x-c1", "low"), ("x-c2", "low"), ("x-mine", "low")],
                Some(0),
            ),
            Some("sess-1"),
            vec![],
            Ok(4),
            "high",
            Ok(HashMap::from([
                ("x-c1".to_string(), "x-cccc".to_string()),
                ("x-c2".to_string(), "x-cccc".to_string()),
            ])),
            Ok(0),
        )
        .unwrap();
        assert_eq!(reading["target_ready"], json!(["x-mine"]), "{reading}");
        assert_eq!(reading["starts"], json!([]), "{reading}");
        assert_eq!(
            reading["skips"],
            json!([
                {"id": "x-c1", "reason": "owned by team x-cccc"},
                {"id": "x-c2", "reason": "owned by team x-cccc"},
            ]),
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unread owner read is the reading's error, never an empty filter:
    /// an empty map would print a foreign team's nodes as dispatchable.
    #[test]
    fn an_unread_owner_map_fails_the_blueprint_reading() {
        let dir = std::env::temp_dir().join(format!("fno-bp-unowned-{}", std::process::id()));
        let reading = blueprint_reading_owned(
            &dir,
            "",
            unplanned_board(&[("x-1", "low")], Some(0)),
            Some("sess-1"),
            vec![],
            Ok(4),
            "high",
            Err("a live team's scope does not compile: e-x: unknown".to_string()),
            Ok(0),
        );
        assert!(reading.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An epic-heavy scope names its idea children: the selection refuses
    /// every contained child, so an empty unplanned queue must not read as
    /// clean while the epics hold the most unplanned work in the scope
    /// (observed 2026-10-24: 76 idea children, beat said "no unplanned
    /// node in scope").
    #[test]
    fn an_epic_heavy_scope_names_its_idea_children() {
        let dir = std::env::temp_dir().join(format!("fno-bp-ideas-{}", std::process::id()));
        let reading = blueprint_reading_owned(
            &dir,
            "",
            unplanned_board(&[], Some(0)),
            Some("sess-1"),
            vec![],
            Ok(4),
            "high",
            Ok(HashMap::new()),
            Ok(0),
        )
        .unwrap();
        // Zero count keeps the old line; a real count names the children.
        assert_eq!(reading["idea_children"], 0);
        assert_eq!(
            reading["skips"][0]["reason"], "no unplanned node in scope",
            "{reading}"
        );

        let dir2 = std::env::temp_dir().join(format!("fno-bp-ideas2-{}", std::process::id()));
        let reading = blueprint_reading_owned(
            &dir2,
            "",
            unplanned_board(&[], Some(0)),
            Some("sess-1"),
            vec![],
            Ok(4),
            "high",
            Ok(HashMap::new()),
            Ok(76),
        )
        .unwrap();
        assert_eq!(reading["idea_children"], 76);
        assert_eq!(
            reading["skips"][0]["reason"],
            "no dispatchable unplanned node in scope: 76 idea children ride their contained epics",
            "{reading}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// The stop check folds the provider cap into the same one-per-lead
    /// ceiling the check-in reads: never raised, only lowered, and the
    /// default holds with no cap at all.
    #[test]
    fn the_stop_check_ceiling_folds_the_provider_cap_and_never_raises_the_default() {
        assert_eq!(blueprint_ceiling(None), 1);
        assert_eq!(blueprint_ceiling(Some(3)), 1);
        assert_eq!(blueprint_ceiling(Some(1)), 1);
    }

    /// Only this session's `blueprint-session:` holders count as running,
    /// and each names its node.
    #[test]
    fn running_blueprints_name_only_this_sessions_claims() {
        let mk = |key: &str, holder: &str| -> crate::claims::ClaimRecord {
            serde_json::from_value(json!({
                "key": key, "holder": holder, "acquired_at": 0, "pid": null, "host": "t"
            }))
            .unwrap()
        };
        let rows = vec![
            mk("node:x-9", "blueprint-session:s1"),
            mk("node:x-8", "blueprint-session:s2"),
            mk("node:x-7", "spawn-handover:t-w"),
        ];
        assert_eq!(running_blueprint_ids(&rows, "s1"), vec!["x-9".to_string()]);
        assert!(running_blueprint_ids(&rows, "s3").is_empty());
    }
}
