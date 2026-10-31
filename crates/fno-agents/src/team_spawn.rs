//! Thread promotion and the registry effects shared by spawn carriers.

use crate::claude_ask::emit_event;
use crate::lead_state::{self, ManifestRefusal, ManifestSpec};
use crate::paths::AgentsHome;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub fn resolve(payload: &Value) -> Result<Value, String> {
    match payload.get("op").and_then(Value::as_str) {
        Some("seed") => {
            let message = payload["message"].as_str().unwrap_or("");
            let scope = payload["scope"].as_str().filter(|s| !s.is_empty());
            let typed = payload["level"].is_u64()
                && scope.is_some()
                && payload["revive"].as_bool() != Some(true);
            let message = if typed {
                let lead = crate::provider::render_command_seed(
                    &format!("/fno:lead {}", scope.unwrap()),
                    payload["harness"].as_str().unwrap_or("claude"),
                );
                format!("{lead}\n{message}")
            } else {
                message.to_owned()
            };
            Ok(json!({"message": message, "typed": typed}))
        }
        Some("apply") => apply(payload),
        Some("journal") => Ok(journal(payload)),
        Some("receipt") => Ok(receipt(payload)),
        Some("validate") => Ok(json!({"refusal": validate(payload)})),
        Some("manifest") => Ok(manifest_op(payload)),
        Some("arm") => Ok(arm_op(payload)),
        Some("record") => Ok(json!({"notices": record(payload, &events_path(payload))})),
        Some("missions") => {
            let scope = payload["scope"].as_str().unwrap_or("");
            let (armed, notices) = arm_missions(scope, &events_path(payload));
            Ok(json!({"armed": armed, "notices": notices}))
        }
        Some("settle") => Ok(settle(payload)),
        _ => Err(concat!(
            "spawn-team: expected seed|apply|journal|receipt|validate",
            "|manifest|arm|record|missions|settle"
        )
        .into()),
    }
}

fn manifest_spec(payload: &Value) -> Result<ManifestSpec, String> {
    serde_json::from_value(payload.clone()).map_err(|e| format!("lead manifest payload: {e}"))
}

/// A caller-named path, else the agents home the Rust runtime resolves.
fn home_path(payload: &Value, key: &str, default: impl FnOnce(&AgentsHome) -> PathBuf) -> PathBuf {
    match payload[key].as_str() {
        Some(path) => PathBuf::from(path),
        None => default(&AgentsHome::from_env()),
    }
}

fn events_path(payload: &Value) -> PathBuf {
    home_path(payload, "events", crate::client_commands::trace_events_path)
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_default()
}

/// `config.lead.enabled`, unless the caller already resolved it.
fn lead_enabled(payload: &Value) -> bool {
    payload["lead_enabled"]
        .as_bool()
        .unwrap_or_else(|| lead_state::lead_loop_enabled(&cwd()))
}

/// Manifests live in the space of the role row's cwd.
fn state_root(payload: &Value, owner_cwd: Option<&str>) -> PathBuf {
    if let Some(root) = payload["state_root"].as_str() {
        return PathBuf::from(root);
    }
    let cwd = owner_cwd
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(cwd);
    crate::paths::space_dir(&cwd)
}

/// One manifest at an explicit path.
fn manifest_op(payload: &Value) -> Value {
    let written = manifest_spec(payload).and_then(|spec| {
        let path = payload["path"]
            .as_str()
            .ok_or_else(|| "lead manifest: path must be a string".to_string())?;
        Ok(lead_state::write_lead_manifest(
            Path::new(path),
            &spec,
            payload["force"] == true,
        ))
    });
    match written {
        Ok(Ok(fields)) => json!({"fields": fields}),
        Ok(Err(ManifestRefusal::Exists(m))) => json!({"exists": m}),
        Ok(Err(ManifestRefusal::Failed(m))) | Err(m) => json!({"error": m}),
    }
}

/// Python `arm_lead_manifest`, once the caller has read the holder row.
fn arm_op(payload: &Value) -> Value {
    let armed = manifest_spec(payload).and_then(|spec| {
        let root = state_root(payload, spec.owner_cwd.as_deref());
        lead_state::arm_manifest(&root, &spec, lead_enabled(payload))
    });
    match armed {
        Ok(path) => json!({"path": path.map(|p| p.display().to_string())}),
        Err(m) => json!({"error": m}),
    }
}

/// Settle a launched promoted spawn: stamp the successor's role under the
/// registry lock, arm its lead manifest, journal the grant, arm the scope's
/// epics and answer the receipt notices. A failure after launch leaves the
/// worker running with no role and answers the remedy.
fn settle(payload: &Value) -> Value {
    let registry = home_path(payload, "registry", AgentsHome::registry_json);
    match stamp(&registry, payload) {
        Ok(answer) => {
            let mut notices = record(&answer, &events_path(payload));
            notices.extend(
                receipt(&answer)["notices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.as_str().map(str::to_owned)),
            );
            json!({"notices": notices})
        }
        Err(e) => {
            let name = crate::claude_ask::py_repr(payload["name"].as_str().unwrap_or(""));
            json!({"notices": [format!(
                "spawn: promotion settlement failed after launch: {e}; {name} runs \
                 without a confirmed role. Grant it with fno agents org promote."
            )]})
        }
    }
}

/// The locked registry write. A decision this op cannot apply declines and
/// leaves every row as it was.
fn stamp(registry: &Path, payload: &Value) -> Result<Value, String> {
    let name = payload["name"].as_str().unwrap_or("");
    let session = payload["session_id"].as_str().unwrap_or("");
    let mut answer = payload.clone();
    answer["found"] = json!(false);
    answer["armed"] = Value::Null;
    answer["arm_error"] = json!("");
    crate::state::update_registry(registry, |doc| -> Result<Value, String> {
        let rows = doc
            .entries
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let decided = apply(&json!({
            "scope": payload["scope"], "plan": payload["plan"], "successor": name,
            "rows": rows, "stamp": true, "level": payload["level"], "grantor": payload["grantor"],
            "successor_identity": {"harness": payload["harness"], "session_id": session,
                "cwd": payload["cwd"]},
        }))
        .and_then(|decision| staged(&doc.entries, &rows, &decision).map(|s| (decision, s)));
        let (outcome, vacated) = match decided {
            Ok((decision, (entries, vacated))) => {
                doc.entries = entries;
                (decision["outcome"].clone(), vacated)
            }
            Err(_) => (json!("declined"), Vec::new()),
        };
        answer["outcome"] = outcome;
        answer["vacated"] = json!(vacated);
        let successor = doc.entries.iter().find(|r| {
            r.name == name
                && [
                    r.harness_session_id.as_deref(),
                    r.cc_session_id.as_deref(),
                    Some(r.short_id.as_str()),
                ]
                .into_iter()
                .flatten()
                .any(|s| !s.is_empty() && s == session)
        });
        answer["found"] = json!(successor.is_some());
        let Some(row) = successor else {
            return Ok(answer.clone());
        };
        let Some(role_scope) = row.role_scope.clone().filter(|s| !s.is_empty()) else {
            return Ok(answer.clone());
        };
        if answer["outcome"] != "declined" {
            let spec = ManifestSpec {
                scope: role_scope.clone(),
                harness_session_id: Some(session.to_owned()),
                owner_cwd: Some(row.cwd.clone()),
                role_level: row.role_level.map(|l| json!(l)),
                role_scope: Some(role_scope),
                role_grantor: row.role_grantor.clone(),
                model: row.requested_model.clone(),
                harness: row.harness.clone(),
                ..Default::default()
            };
            match lead_state::arm_manifest(
                &state_root(payload, Some(row.cwd.as_str())),
                &spec,
                lead_enabled(payload),
            ) {
                Ok(path) => answer["armed"] = json!(path.is_some()),
                Err(e) => {
                    answer["armed"] = json!(false);
                    answer["arm_error"] = json!(e);
                }
            }
        }
        Ok(answer.clone())
    })
    .map_err(|e| e.to_string())?
}

/// The rows after `decision`'s updates, and each vacated row (as it stood
/// before the write) with its cause. Errs before any row changes.
fn staged(
    entries: &[crate::state::RegistryEntry],
    rows: &[Value],
    decision: &Value,
) -> Result<(Vec<crate::state::RegistryEntry>, Vec<Value>), String> {
    let merged = |i: usize, fields: &Value| -> Result<Value, String> {
        let mut row = rows
            .get(i)
            .ok_or("spawn-team: row index out of bounds")?
            .clone();
        for (key, value) in fields
            .as_object()
            .ok_or("spawn-team: fields must be an object")?
        {
            row[key.as_str()] = value.clone();
        }
        Ok(row)
    };
    let mut out = entries.to_vec();
    for (index, fields) in decision["updates"]
        .as_object()
        .ok_or("spawn-team: missing updates")?
    {
        let i: usize = index.parse().map_err(|_| "spawn-team: invalid row index")?;
        out[i] = serde_json::from_value(merged(i, fields)?).map_err(|e| e.to_string())?;
    }
    let vacated = decision["vacated"]
        .as_array()
        .ok_or("spawn-team: missing vacated")?
        .iter()
        .map(|v| {
            let i = v[0].as_u64().ok_or("spawn-team: invalid row index")? as usize;
            Ok(json!([merged(i, &v[2])?, v[1]]))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((out, vacated))
}

/// Journal one committed spawn write: a vacate line per cleared holder, a
/// reown line per team child that followed the role, the grant line, then
/// the scope's missions on a grant.
fn record(payload: &Value, events: &Path) -> Vec<String> {
    let answer = journal(payload);
    for vacated in payload["vacated"].as_array().into_iter().flatten() {
        if let ("succession", Some(session)) = (
            vacated[1].as_str().unwrap_or(""),
            vacated[0]["harness_session_id"].as_str(),
        ) {
            crate::retired_crown::write(
                &AgentsHome::from_env(),
                session,
                payload["scope"].as_str().unwrap_or(""),
                "succession",
            );
        }
    }
    for event in answer["events"].as_array().into_iter().flatten() {
        let fields: Vec<(&str, Value)> = event["data"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        emit_event(events, event["kind"].as_str().unwrap_or(""), &fields);
    }
    if answer["arm_missions"] != true {
        return Vec::new();
    }
    arm_missions(payload["scope"].as_str().unwrap_or(""), events).1
}

/// Set mission_active on every open epic container in a granted scope: an
/// epic with an owner is a mission, or no drain loop sees its children. A
/// childless epic is skipped, because the mission command refuses it as not a
/// container and the flag would stand forever. Answers the ids newly armed,
/// or None and the warning when the graph write failed.
fn arm_missions(scope: &str, events: &Path) -> (Option<Vec<String>>, Vec<String>) {
    match arm_open_epics(scope, events) {
        Ok(armed) => (Some(armed), Vec::new()),
        Err(e) => (
            None,
            vec![format!(
                "role: WARNING: mission arming failed for scope {} ({e}); \
                 arm it with: fno backlog advance --epic <id>",
                crate::claude_ask::py_repr(scope)
            )],
        ),
    }
}

fn arm_open_epics(scope: &str, events: &Path) -> Result<Vec<String>, String> {
    // An external tracker owns the node types; the local graph cannot say
    // which member is an epic.
    if crate::graph_get::external_backend_selected() {
        return Ok(Vec::new());
    }
    let entries = crate::backlog::read_entries(&crate::backlog::settings::graph_path())?;
    let mut armed = Vec::new();
    for member in scope.split(',').map(str::trim).filter(|m| !m.is_empty()) {
        let Some(entry) = entries.iter().find(|e| e["id"] == member) else {
            continue;
        };
        let open = !matches!(entry["status"].as_str(), Some("done" | "superseded"));
        let container = entries
            .iter()
            .any(|e| e["parent"] == member && e["contained_in"] != member);
        if entry["type"] == "epic"
            && open
            && container
            && crate::backlog::advance_cli::mission_active(member, true)?
        {
            emit_event(
                events,
                "mission_activated",
                &[("epic_id", json!(member)), ("source", json!("role"))],
            );
            armed.push(member.to_owned());
        }
    }
    Ok(armed)
}

fn validate(payload: &Value) -> Option<String> {
    let level = &payload["level"];
    let scope = &payload["scope"];
    let level_repr = payload["level_repr"].as_str().unwrap_or("None");
    let scope_repr = payload["scope_repr"].as_str().unwrap_or("None");
    if level.is_null() && scope.is_null() {
        return None;
    }
    if level.is_null() || scope.is_null() {
        return Some(format!(
            "a role needs both level and scope; got level={level_repr} scope={scope_repr}"
        ));
    }
    if payload["level_is_int"] != true {
        return Some(format!("role level must be an int 0..2; got {level_repr}"));
    }
    let Some(level) = level
        .as_str()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|l| *l <= 2)
    else {
        return Some(format!("role level must be 0..2 (0 several projects, 1 one project, 2 one epic); got {level_repr}"));
    };
    let Some(scope) = scope
        .as_str()
        .filter(|s| payload["scope_is_str"] == true && !s.trim().is_empty())
    else {
        return Some(format!(
            "role scope must be a nonblank id; got {scope_repr}"
        ));
    };
    let mut members: Vec<&str> = scope
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    members.sort_unstable();
    members.dedup();
    let canonical = members.join(",");
    if canonical != scope {
        return Some(format!("role scope must be canonical (sorted, deduped, no blank members); got {scope_repr}, want {}", crate::claude_ask::py_repr(&canonical)));
    }
    if members.len() > 1 && level != 0 && level != 2 {
        return Some(format!("a scope naming {} members is level 0 (a portfolio of projects) or 2 (a set of epics), not {level}", members.len()));
    }
    let projects: Vec<&str> = payload["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if level == 0 && members.len() > 1 && projects.is_empty() {
        return Some(format!("level 0 is a portfolio of PROJECTS, but no member of {scope_repr} resolves to a configured project"));
    }
    if level == 2 && !projects.is_empty() {
        return Some(format!(
            "level 2 is a SET OF EPICS, but {} resolve(s) to a configured project",
            projects.join(", ")
        ));
    }
    None
}

fn apply(payload: &Value) -> Result<Value, String> {
    let rows = payload["rows"]
        .as_array()
        .ok_or("spawn-team: rows must be an array")?;
    let successor_index = if payload["stamp"].as_bool() == Some(true) {
        let identity = crate::team_identity::resolve(&json!({
            "rows": rows,
            "expect": {"name": payload["successor"],
                "harness_session_id": payload["successor_identity"]["session_id"]},
        }))?;
        if identity["matched"] != true {
            return Ok(json!({"outcome": "declined", "updates": {}, "vacated": [],
                "successor_index": null}));
        }
        identity["index"].as_u64().map(|i| i as usize)
    } else {
        None
    };
    let mut settlement = payload.clone();
    if successor_index.is_some() {
        // A carrier may stamp at mint, before the seed turn runs. Its own
        // verified successor is not a competing holder during settlement.
        settlement["exclude_name"] = payload["successor"].clone();
    }
    let answer = crate::team_settle::resolve(&settlement)?;
    let outcome = answer["outcome"]
        .as_str()
        .ok_or("spawn-team: missing outcome")?;
    let mut updates = Map::new();
    let mut vacated = Vec::new();
    for (field, cause) in [
        ("clear_terminal_rows", "holder_terminal"),
        ("vacate_rows", "succession"),
    ] {
        for index in answer[field]
            .as_array()
            .ok_or("spawn-team: missing row indexes")?
        {
            let i = index.as_u64().ok_or("spawn-team: invalid row index")? as usize;
            rows.get(i).ok_or("spawn-team: row index out of bounds")?;
            updates.insert(
                i.to_string(),
                json!({"role_level": null,
                "role_scope": null, "role_grantor": null}),
            );
            vacated.push(json!([i, cause, {}]));
        }
    }
    reown(rows, &answer, &mut updates, &mut vacated)?;
    if let Some(i) = successor_index {
        let stamp = if outcome == "declined" {
            json!({"role_level": null, "role_scope": null, "role_grantor": null})
        } else {
            json!({"role_level": payload["level"], "role_scope": payload["scope"],
                "role_grantor": payload["grantor"]})
        };
        updates
            .entry(i.to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap()
            .extend(stamp.as_object().unwrap().clone());
    }
    Ok(
        json!({"outcome": outcome, "updates": updates, "vacated": vacated,
        "successor_index": successor_index}),
    )
}

fn reown(
    rows: &[Value],
    answer: &Value,
    updates: &mut Map<String, Value>,
    vacated: &mut Vec<Value>,
) -> Result<(), String> {
    if answer["reown_owner"].is_null() {
        return Ok(());
    }
    for index in answer["reown_rows"]
        .as_array()
        .ok_or("spawn-team: missing reown indexes")?
    {
        let i = index.as_u64().ok_or("spawn-team: invalid reown index")? as usize;
        let row = rows.get(i).ok_or("spawn-team: reown index out of bounds")?;
        // Reown moves an existing owner only. A block forked onto a
        // provenance-less row has no origin, and the typed registry reader
        // rejects it.
        let Some(provenance) = row["spawn_provenance"].as_object() else {
            continue;
        };
        let mut provenance = provenance.clone();
        provenance.insert("owner".into(), answer["reown_owner"].clone());
        updates.entry(i.to_string()).or_insert_with(|| json!({}))["spawn_provenance"] =
            json!(provenance);
        vacated.push(json!([i, "reowned", {"spawn_provenance": provenance}]));
    }
    Ok(())
}

fn journal(payload: &Value) -> Value {
    let mut events = Vec::new();
    for vacated in payload["vacated"].as_array().into_iter().flatten() {
        let row = &vacated[0];
        let cause = vacated[1].as_str().unwrap_or("");
        let (kind, data) = if cause == "reowned" {
            (
                "agent_team_reowned",
                json!({"scope": payload["scope"],
                "successor": payload["name"], "child": row["name"]}),
            )
        } else {
            (
                "agent_role_vacated",
                json!({"scope": payload["scope"],
                "level": row["role_level"], "holder": row["name"],
                "holder_session": row["harness_session_id"], "grantor": row["role_grantor"],
                "cause": cause, "successor": if cause == "succession" {payload["name"].clone()} else {Value::Null}}),
            )
        };
        events.push(json!({"kind": kind, "data": data}));
    }
    let armed = matches!(payload["outcome"].as_str(), Some("granted" | "succeeded"));
    if armed {
        events.push(json!({"kind": "agent_promoted", "data": {
            "name": payload["name"], "level": payload["level"],
            "scope": payload["scope"], "grantor": payload["grantor"],
            "vacated_scope": null, "vacated_level": null, "stranded_subordinates": []}}));
    }
    json!({"events": events, "arm_missions": armed})
}

fn receipt(payload: &Value) -> Value {
    let scope = payload["scope"].as_str().unwrap_or("");
    let name = payload["name"].as_str().unwrap_or("");
    let mut notices = Vec::new();
    if payload["found"] != true {
        notices.push(format!("spawn: promotion over {scope:?} NOT applied: no matching registry identity for {name:?}; grant it with `fno agents org promote`"));
    } else if payload["outcome"] == "declined" {
        notices.push(format!("spawn: promotion declined (scope {scope:?} already held by a live row); the worker launched without a role."));
    } else {
        if payload["outcome"] == "succeeded" {
            let mut vacated: Vec<&str> = payload["vacated"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|v| v[1] == "succession")
                .filter_map(|v| v[0]["name"].as_str())
                .collect();
            vacated.sort_unstable();
            vacated.dedup();
            let tail = if vacated.contains(&payload["caller"].as_str().unwrap_or("")) {
                " You no longer hold it."
            } else {
                ""
            };
            notices.push(format!(
                "spawn: promotion over {scope:?} transferred from {} to {name} (succession).{tail}",
                vacated.join(", ")
            ));
        }
        if payload["armed"] == false {
            let error = payload["arm_error"].as_str().unwrap_or("");
            let reason = if error.is_empty() {
                "; lead loop disabled".to_owned()
            } else {
                format!(": {error}")
            };
            notices.push(format!("spawn: promotion over {scope:?} recorded, but the lead loop manifest was NOT armed{reason}"));
        }
        let typed = if payload["typed"] == true {
            "lead typed"
        } else {
            "lead NOT typed"
        };
        notices.push(format!("spawn: promotion over {scope:?} recorded; {typed}"));
    }
    json!({"notices": notices})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_decisions_match_captured_answers() {
        let fixtures: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/team_spawn.json"
        )))
        .unwrap();
        for case in fixtures["cases"].as_array().unwrap() {
            assert_eq!(
                resolve(&case["payload"]).unwrap(),
                case["answer"],
                "{}",
                case["payload"]
            );
        }
    }

    #[test]
    fn rebound_successor_cannot_clear_the_predecessor() {
        let answer = resolve(&json!({"op": "apply", "stamp": true, "scope": "epic-a",
            "successor": "successor", "successor_identity": {"session_id": "original"},
            "rows": [{"name": "successor", "harness_session_id": "rebound"},
                {"name": "predecessor", "role_level": 2, "role_scope": "epic-a"}]}))
        .unwrap();
        assert_eq!(answer["outcome"], "declined");
        assert_eq!(answer["updates"], json!({}));
        assert_eq!(answer["vacated"], json!([]));
    }

    #[test]
    fn reown_never_forks_provenance_onto_a_bare_row() {
        let (mut updates, mut vacated) = (Map::new(), Vec::new());
        let answer = json!({"reown_rows": [0], "reown_owner": {"kind": "session"}});
        reown(
            &[json!({"name": "w5"})],
            &answer,
            &mut updates,
            &mut vacated,
        )
        .unwrap();
        assert!(updates.is_empty() && vacated.is_empty());
    }
}
