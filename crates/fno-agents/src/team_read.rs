//! `fno agents team` (the bare `fno agents org`): one read of every role,
//! its scope, its holder, and whether the registry and the graph agree. A
//! read leaf with no action of its own: the router rewrites it into
//! `status --team`, because the binary's action list is shrink-only.
//!
//! Agreement is a POSITIVE marker: `agree` is true only when the graph was
//! read and the scope checked out; an unreadable graph answers null with a
//! stated reason, and the summary counts unknowns separately. Each role also
//! names its manifest limb (path, session, `role_source`): the manifest is
//! the durable role record, the row its cache. Every reader it composes is
//! already native (rivals, the lead term op, the vacancy sweep, the scope
//! fold, the spawn gate probe) and runs in this process.

use crate::client_commands::py_repr_str;
use crate::state::RegistryEntry;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;

const TERMINAL_ROW: [&str; 4] = ["exited", "orphaned", "failed", "permanent_dead"];
const TERMINAL_PLAN: [&str; 2] = ["done", "superseded"];

const HELP: &str = "\
Usage: fno agents team [OPTIONS]

  The whole team: every live role, its scope, its holder, its grantor, and
  whether the registry and the graph agree.

  Exit 0 always: this is a read, and a caller gates on the JSON keys (agree,
  summary.disagreements, summary.unknowns, and conflicts), not the process
  status.

Options:
  -J, --json   Emit JSON instead of the table.
  -n, --nodes  Fold each role's scope nodes into its row. Implies JSON output.
  --help       Show this message and exit.
";

/// `status --team [-J] [-n]`.
pub fn run_team(args: &[String]) -> i32 {
    let (mut as_json, mut nodes) = (false, false);
    for arg in args {
        match arg.as_str() {
            "--json" | "-J" => as_json = true,
            "--nodes" | "-n" => nodes = true,
            "--help" | "-h" => {
                print!("{HELP}");
                return 0;
            }
            other => {
                eprintln!("fno agents team: no such option: {other}");
                return 2;
            }
        }
    }
    let team = gather_team();
    let statuses = registry_rows().ok().map(|rows| {
        rows.iter()
            .filter_map(|r| {
                let sid = r.harness_session_id.clone().filter(|s| !s.is_empty())?;
                Some((sid, status_of(r)))
            })
            .collect::<HashMap<_, _>>()
    });
    println!(
        "{}",
        render_team_with(team, read_fold, statuses, gate_read, as_json, nodes)
    );
    0
}

fn registry_rows() -> Result<Vec<RegistryEntry>, String> {
    crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
        .map(|r| r.entries)
        .map_err(|e| e.to_string())
}

fn status_of(row: &RegistryEntry) -> String {
    serde_json::to_value(row.status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Python `str()` of a JSON value, as the table renders a cell.
fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
    }
}

/// The members of a stored scope; a single-name scope yields one element.
fn split_scope(scope: Option<&str>) -> Vec<String> {
    scope
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn graph_index() -> Option<HashMap<String, Value>> {
    let rows = crate::graph_store::read_rows(&crate::backlog::settings::graph_path()).ok()?;
    Some(
        rows.into_iter()
            .filter_map(|row| {
                let id = row.get("id")?.as_str()?.to_string();
                (!id.is_empty()).then_some((id, row))
            })
            .collect(),
    )
}

/// Does the graph corroborate this role? `(agree, reason)`. An
/// unadjudicable role answers `(None, reason)`; only the epic rung needs the
/// graph.
fn agreement(
    level: Option<u32>,
    scope: Option<&str>,
    by_id: Option<&HashMap<String, Value>>,
) -> (Option<bool>, Option<String>) {
    let members = split_scope(scope);
    // A scope-less level is half a role: it rules no territory.
    if members.is_empty() {
        return (
            Some(false),
            Some("the row carries a role level but no scope (half a role)".into()),
        );
    }
    if level == Some(2) {
        let Some(by_id) = by_id else {
            return (None, Some("graph unreadable".into()));
        };
        // A rung-2 scope is a SET of epics: one dead member makes the whole
        // role disagree.
        for id in &members {
            let Some(entry) = by_id.get(id) else {
                return (
                    Some(false),
                    Some(format!("{} is not in the graph", py_repr_str(id))),
                );
            };
            let node_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
            if node_type != "epic" {
                let shown = if node_type.is_empty() {
                    "node"
                } else {
                    node_type
                };
                return (
                    Some(false),
                    Some(format!("{} is a {shown}, not an epic", py_repr_str(id))),
                );
            }
            if let Some(status) = entry.get("status").and_then(Value::as_str) {
                if TERMINAL_PLAN.contains(&status) {
                    return (
                        Some(false),
                        Some(format!(
                            "{} status is {} (terminal)",
                            py_repr_str(id),
                            py_repr_str(status)
                        )),
                    );
                }
            }
        }
        return (Some(true), None);
    }
    // Level 0/1: every member must resolve to a configured project.
    let unresolved: Vec<String> = members
        .into_iter()
        .filter(|m| crate::project::canonical(m).is_none())
        .collect();
    match unresolved.len() {
        0 => (Some(true), None),
        1 => (
            Some(false),
            Some(format!("{} not a configured project", unresolved[0])),
        ),
        _ => (
            Some(false),
            Some(format!(
                "{} are not configured projects",
                unresolved.join(", ")
            )),
        ),
    }
}

/// The manifest side of one role, read through the lead term op. An
/// unreadable read keeps the row side.
fn manifest_limb(scope: Option<&str>, row: &RegistryEntry) -> Map<String, Value> {
    let mut limb = Map::new();
    limb.insert("manifest_path".into(), Value::Null);
    limb.insert("manifest_session".into(), Value::Null);
    limb.insert("role_source".into(), "row".into());
    let scope = scope.unwrap_or("");
    if scope.trim().is_empty() || row.cwd.trim().is_empty() {
        return limb;
    }
    let Ok(state) = crate::lead_ops::overlay_kind(&json!({
        "kind": "lead", "op": "term", "scope": scope, "owner_cwd": row.cwd,
    })) else {
        return limb;
    };
    if truthy(state.get("error")) {
        return limb;
    }
    for key in ["manifest_session", "manifest_path"] {
        limb.insert(key.into(), state.get(key).cloned().unwrap_or(Value::Null));
    }
    if state.get("split") == Some(&Value::Bool(true)) {
        limb.insert("role_source".into(), "split".into());
    } else if state.get("team_on_manifest") == Some(&Value::Bool(true)) {
        limb.insert("role_source".into(), "both".into());
    }
    limb
}

/// Roles whose row is gone but whose manifest holds them, swept over the
/// spaces ROOT because a vanished row names no cwd.
fn manifest_only_roles(held: &[String]) -> Vec<Value> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let window = 3 * crate::lead_verdict_inputs::checkin_interval_secs(&cwd);
    crate::lead_state::org_vacancies(
        &crate::paths::spaces_root(),
        held,
        &crate::claude_roster::read_all_agents_union,
        &crate::team_reap::transcript_age_now,
        window,
        chrono::Utc::now(),
    )
    .into_iter()
    .filter(|o| !o.scope.is_empty())
    .map(|o| {
        json!({
            "holder": o.manifest_session.clone().unwrap_or_else(|| o.scope.clone()),
            "level": o.level,
            "scope": o.scope,
            "grantor": o.grantor.clone().filter(|g| !g.is_empty()).unwrap_or_else(|| "human".into()),
            "status": "manifest-only",
            "agree": Value::Null,
            "reason": "role lives on the manifest; no live registry row holds it",
            "manifest_path": o.manifest_path,
            "manifest_session": o.manifest_session,
            "role_source": "manifest",
        })
    })
    .collect()
}

/// Territory two live promoted rows double-rule, one entry per rival PAIR,
/// from the same ladder-aware rule the grant path enforces. An unavailable
/// scanner answers no conflicts with a stderr line, never a silent all-clear.
fn conflicts(live: &[&RegistryEntry]) -> Vec<Value> {
    let rows: Vec<Value> = live
        .iter()
        .map(|r| json!({"name": r.name, "role_scope": r.role_scope, "role_level": r.role_level}))
        .collect();
    let mut payload = json!({"kind": "team-rivals", "rows": rows});
    if let Some(map) = crate::project::alias_map() {
        payload["projects"] = Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect(),
        );
    }
    match crate::org_rivals::resolve(&payload).and_then(|a| {
        a.get("pairs")
            .cloned()
            .ok_or_else(|| "no pairs".to_string())
    }) {
        Ok(Value::Array(pairs)) => pairs,
        Ok(_) => Vec::new(),
        Err(e) => {
            eprintln!("team: rivalry scan unavailable ({e}); conflicts not listed");
            Vec::new()
        }
    }
}

/// The whole team: every role, its verdict, and any territorial conflict. An
/// unreadable REGISTRY nulls `roles` and every summary count rather than
/// reporting an empty team.
pub fn gather_team() -> Value {
    let rows = match registry_rows() {
        Ok(rows) => rows,
        Err(e) => {
            return json!({
                "roles": Value::Null,
                "conflicts": Value::Null,
                "registry_readable": false,
                "graph_readable": Value::Null,
                "summary": {
                    "total": Value::Null,
                    "disagreements": Value::Null,
                    "unknowns": Value::Null,
                    "splits": Value::Null,
                    "reason": format!("registry unreadable: {e}"),
                },
            })
        }
    };
    let live: Vec<&RegistryEntry> = rows
        .iter()
        .filter(|r| !TERMINAL_ROW.contains(&status_of(r).as_str()))
        .collect();
    let by_id = graph_index();
    let mut roles: Vec<Value> = Vec::new();
    let mut held: Vec<String> = Vec::new();
    for row in &live {
        let scope = row.role_scope.as_deref();
        let grantor = row
            .role_grantor
            .clone()
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| "human".into());
        let (agree, reason) = match row.role_level {
            // A half role still HOLDS its territory; surface it, never skip.
            None if scope.is_some_and(|s| !s.is_empty()) => (
                Some(false),
                Some("half a role: scope is set but level is missing".to_string()),
            ),
            None => continue,
            Some(level) => agreement(Some(level), scope, by_id.as_ref()),
        };
        let mut entry = Map::new();
        entry.insert("holder".into(), row.name.clone().into());
        entry.insert("level".into(), json!(row.role_level));
        entry.insert("scope".into(), json!(scope));
        entry.insert("grantor".into(), grantor.into());
        entry.insert("status".into(), status_of(row).into());
        entry.insert("agree".into(), json!(agree));
        entry.insert("reason".into(), json!(reason));
        entry.extend(manifest_limb(scope, row));
        roles.push(Value::Object(entry));
        if let Some(s) = scope.filter(|s| !s.trim().is_empty()) {
            held.push(s.to_string());
        }
    }
    let orphans = manifest_only_roles(&held);
    let manifest_only = orphans.len();
    roles.extend(orphans);
    let count = |pred: &dyn Fn(&Value) -> bool| roles.iter().filter(|r| pred(r)).count();
    let disagreements = count(&|r| r["agree"] == Value::Bool(false));
    let unknowns = count(&|r| r["agree"].is_null());
    let splits = count(&|r| r["role_source"] == "split");
    json!({
        "conflicts": conflicts(&live),
        "graph_readable": by_id.is_some(),
        "registry_readable": true,
        "summary": {
            // total counts ROW roles only: the census computes workers from it.
            "total": roles.len() - manifest_only,
            "manifest_only": manifest_only,
            "sweep_ran": true,
            "disagreements": disagreements,
            "unknowns": unknowns,
            "splits": splits,
        },
        "roles": roles,
    })
}

/// The scope fold over `payload`, read in this process.
fn read_fold(payload: &[Value]) -> Result<Value, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::org_fold::org_fold(
        &crate::backlog::settings::graph_path(),
        &cwd,
        None,
        &crate::paths::AgentsHome::from_env().registry_json(),
        payload,
    )
}

fn unresolved(reason: &str) -> Value {
    json!({"status": "unresolved", "reason": reason})
}

/// Fold each role's scope onto its row; any fault marks the role
/// unresolved. Returns the fold's own answer, or `{}` on a fault.
fn fold_scope_nodes(
    roles: &mut [Value],
    read: impl FnOnce(&[Value]) -> Result<Value, String>,
) -> Value {
    if roles.is_empty() {
        return json!({});
    }
    for role in roles.iter_mut() {
        let has_scope = role["scope"].as_str().is_some_and(|s| !s.trim().is_empty());
        if !has_scope || role["level"].is_null() {
            role["scope_nodes"] = unresolved("the row carries no scope or no role level");
        }
    }
    let payload: Vec<Value> = roles
        .iter()
        .filter(|r| r.get("scope_nodes").is_none())
        .map(|r| json!({"scope": r["scope"], "level": r["level"]}))
        .collect();
    if payload.is_empty() {
        return json!({});
    }
    // A fold that answers no `scope_nodes` faults the way a missing key did.
    let folded = match read(&payload).and_then(|f| {
        if f.get("scope_nodes").is_some_and(Value::is_object) {
            Ok(f)
        } else {
            Err("'scope_nodes'".to_string())
        }
    }) {
        Ok(f) => f,
        Err(e) => {
            mark_unresolved(roles, &format!("the fold could not run: {e}"));
            return json!({});
        }
    };
    for role in roles.iter_mut() {
        let row = role["scope"]
            .as_str()
            .and_then(|s| folded["scope_nodes"].get(s))
            .cloned()
            .unwrap_or_else(|| unresolved("the fold answered no row for this scope"));
        role["scope_nodes"] = row;
    }
    folded
}

fn mark_unresolved(roles: &mut [Value], reason: &str) {
    for role in roles.iter_mut() {
        if role.get("scope_nodes").is_none() {
            role["scope_nodes"] = unresolved(reason);
        }
    }
}

/// Judge each node row's session ids against the registry; returns whether
/// it read. Absent from the registry answers `live: null`, never false.
fn annotate_sessions(roles: &mut [Value], statuses: Option<&HashMap<String, String>>) -> bool {
    let empty = HashMap::new();
    let map = statuses.unwrap_or(&empty);
    for role in roles.iter_mut() {
        let Some(nodes) = role
            .get_mut("scope_nodes")
            .and_then(|f| f.get_mut("nodes"))
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for node in nodes {
            let sessions: Vec<Value> = node
                .get("sessions")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .map(|sid| {
                            let status = map.get(sid);
                            json!({
                                "id": sid,
                                "live": status.map(|s| !TERMINAL_ROW.contains(&s.as_str())),
                                "status": status,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            node["sessions"] = Value::Array(sessions);
        }
    }
    statuses.is_some()
}

/// The spawn gate's verdict: `unknown` with a reason, never a healthy default.
fn gate_read() -> Value {
    let verdict = crate::spawn_gate_command::probe_answer(
        &json!({"mode": "probe", "caller_session": crate::claims::resolve_identity().1}),
    );
    if !truthy(verdict.get("verdict")) {
        return json!({"verdict": "unknown", "reason": "the gate answered no verdict"});
    }
    verdict
}

fn blind_stuck(reason: &str) -> Value {
    json!({"unclaimed": [], "blocked": [], "unproven_claim": [], "in_review": [],
           "blind": [reason], "threshold_minutes": Value::Null})
}

/// One line. A clean read and a blind read must never look the same.
fn stuck_render(summary: &Value, gate: &Value) -> String {
    let line = summary["stuck_line"].as_str().unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    if !line.is_empty() {
        parts.push(line.to_string());
    }
    if gate["verdict"] == "refused" {
        parts.push(format!("gate refused {}", py_str(gate.get("reason"))));
    }
    // The fold already rendered its own blind reasons into `line`.
    if let Some(blind) = summary["stuck"]["blind"].as_array() {
        for r in blind {
            let r = py_str(Some(r));
            if !line.contains(r.as_str()) {
                parts.push(format!("could not answer: {r}"));
            }
        }
    }
    if parts.is_empty() {
        "stuck: nothing".to_string()
    } else {
        format!("stuck: {}", parts.join(", "))
    }
}

fn fmt_row(e: &Value) -> String {
    let agree = match &e["agree"] {
        Value::Null => "?",
        Value::Bool(true) => "yes",
        _ => "no",
    };
    let reason = if truthy(e.get("reason")) {
        format!("   {}", py_str(e.get("reason")))
    } else {
        String::new()
    };
    format!(
        "{:<16} {:<5} {:<20} {:<16} {:<14} {:<4} {:<8}{reason}",
        py_str(e.get("scope")),
        py_str(e.get("level")),
        py_str(e.get("holder")),
        py_str(e.get("grantor")),
        py_str(e.get("status")),
        agree,
        py_str(e.get("role_source")),
    )
}

/// Python `json.dumps(v, indent=2, sort_keys=True)`.
fn sorted_json(value: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|k| (k.clone(), sort(&map[k])))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    crate::operator_turns::python_json(&sort(value))
}

fn plural(n: &Value) -> &'static str {
    if n.as_i64() == Some(1) {
        ""
    } else {
        "s"
    }
}

/// The full render over a gathered team: the fold, the session judgement and
/// the gate are injected so the render is testable without live state.
pub fn render_team_with(
    mut team: Value,
    fold: impl FnOnce(&[Value]) -> Result<Value, String>,
    statuses: Option<HashMap<String, String>>,
    gate: impl FnOnce() -> Value,
    as_json: bool,
    nodes: bool,
) -> String {
    if team["roles"].as_array().is_some_and(|r| !r.is_empty()) {
        let roles = team["roles"].as_array_mut().expect("checked above");
        let folded = fold_scope_nodes(roles, fold);
        let sessions_readable = annotate_sessions(roles, statuses.as_ref());
        let gate = gate();
        let mut stuck = match folded.get("stuck") {
            Some(s) if truthy(Some(s)) => s.clone(),
            _ if folded.as_object().is_some_and(Map::is_empty) => {
                blind_stuck("the scope fold did not run")
            }
            _ => blind_stuck(
                "the fno-agents binary predates this team and returned no stuck verdict; \
                 run fno doctor --fix",
            ),
        };
        if gate["verdict"] == "unknown" {
            let reason = format!(
                "the spawn gate answered unknown: {}",
                py_str(gate.get("reason"))
            );
            if let Some(blind) = stuck["blind"].as_array_mut() {
                blind.push(reason.into());
            }
        }
        team["sessions_readable"] = sessions_readable.into();
        team["gate"] = gate;
        team["summary"]["stuck"] = stuck;
        team["summary"]["stuck_line"] = folded
            .get("stuck_line")
            .cloned()
            .unwrap_or_else(|| "".into());
        if !nodes {
            for role in team["roles"].as_array_mut().into_iter().flatten() {
                if let Some(obj) = role.as_object_mut() {
                    obj.remove("scope_nodes");
                }
            }
        }
    }
    if as_json || nodes {
        return sorted_json(&team);
    }
    let Some(roles) = team["roles"].as_array() else {
        return format!(
            "team: CANNOT READ - {}. This is not an empty team; nothing was checked.",
            py_str(team["summary"].get("reason"))
        );
    };
    if roles.is_empty() {
        return "team: no live roles".to_string();
    }
    let mut lines = vec![format!(
        "{:<16} {:<5} {:<20} {:<16} {:<14} AGREE SOURCE",
        "SCOPE", "LEVEL", "HOLDER", "GRANTOR", "STATUS"
    )];
    lines.extend(roles.iter().map(fmt_row));
    for c in team["conflicts"].as_array().into_iter().flatten() {
        let holders: Vec<String> = c["holders"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|h| py_str(Some(h)))
            .collect();
        let scope = match &c["scope"] {
            Value::String(s) => py_repr_str(s),
            other => py_str(Some(other)),
        };
        lines.push(format!(
            "\nconflicts: scope {scope} held by {} live rows ({})",
            holders.len(),
            holders.join(", ")
        ));
    }
    let s = &team["summary"];
    let mut summary = format!(
        "\nteam: {} role{}, {} disagreement{}, {} unknown{}, {} split{}",
        py_str(s.get("total")),
        plural(&s["total"]),
        py_str(s.get("disagreements")),
        plural(&s["disagreements"]),
        py_str(s.get("unknowns")),
        plural(&s["unknowns"]),
        py_str(s.get("splits")),
        plural(&s["splits"]),
    );
    if truthy(s.get("manifest_only")) {
        summary.push_str(&format!(
            ", {} manifest-only",
            py_str(s.get("manifest_only"))
        ));
    }
    lines.push(summary);
    if s.get("sweep_ran") == Some(&Value::Bool(false)) {
        lines.push(
            "orphan sweep did not run (stale or missing binary): zero manifest-only entries is \
             an absence, not a finding"
                .to_string(),
        );
    }
    if s["stuck"].is_object() {
        lines.push(stuck_render(s, &team["gate"]));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_less_level_is_half_a_role() {
        let (agree, reason) = agreement(Some(1), None, None);
        assert_eq!(agree, Some(false));
        assert!(reason.unwrap().contains("half a role"));
    }

    #[test]
    fn an_epic_rung_needs_a_readable_graph() {
        assert_eq!(
            agreement(Some(2), Some("x-a"), None),
            (None, Some("graph unreadable".to_string()))
        );
        let mut by_id = HashMap::new();
        by_id.insert("x-a".to_string(), json!({"type": "epic", "status": "done"}));
        assert_eq!(
            agreement(Some(2), Some("x-a"), Some(&by_id)).1.unwrap(),
            "'x-a' status is 'done' (terminal)"
        );
    }
}
