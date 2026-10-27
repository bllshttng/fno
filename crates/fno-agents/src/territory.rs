//! The territory fact set : one module owns territory identity,
//! membership, and the drain-target facts.
//!
//! Ownership follows the seam rule (rust-python-seam.md): the active-backlog
//! supervisor is a Rust loop that must not stop, and it owns the drain
//! decision. The whole fact set therefore lives beside it - graph membership
//! (moved here from `org_board::scope`, which calls this home), the live
//! team list from the registry cache, the workspace project map, the
//! mission's root project from the graph, and the `active_backlog` config
//! block. Python reads the territory receipt only
//! through `fno config active-backlog*`, which prints this module's output.
//!
//! Fail-closed shape: an unreadable source is `TerritoryUnknown` naming the
//! source, never an empty territory list and never a silently-drained scope.

use crate::agents_config::config_lookup;
use crate::loop_lead::{canonical_members, same_territory};
use crate::org_board::{graph_json_path, project_map};
use crate::paths::AgentsHome;
use crate::state::{load_registry, Registry, RegistryEntry};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Scope compilation (moved from org_board::scope - the board and the drain
// call the same home)
// ---------------------------------------------------------------------------

pub(crate) fn member_not_found(member: &str, entries: &[Value], absent: String) -> String {
    if entries.is_empty() {
        format!("the graph read returned 0 nodes, so {member:?} is unknown, not absent")
    } else {
        absent
    }
}

/// Compile a canonical team scope into the graph node ids it contains
/// (board.compile_scope_ids).
pub(crate) fn compile_scope_ids(
    scope: &str,
    entries: &[Value],
    projects: &Result<HashMap<String, String>, String>,
) -> Result<HashSet<String>, String> {
    compile_territory(scope, entries, projects).map(|(_, ids)| ids)
}

/// The territory-key + membership read (AC1): the canonical scope key
/// alongside the ids it contains. `Err` is the explicit unknown - a scope
/// that is neither a configured project nor a graph epic - never an empty
/// territory, so a capacity/board reader that gets `Err` stops instead of
/// reading the scope as drained or unbounded.
pub(crate) fn compile_territory(
    scope: &str,
    entries: &[Value],
    projects: &Result<HashMap<String, String>, String>,
) -> Result<(String, HashSet<String>), String> {
    use crate::org_board::s_str;
    let members: Vec<String> = scope.split(',').map(str::to_string).collect();
    let entry_by_id = |id: &str| entries.iter().find(|e| s_str(e, "id") == Some(id));
    let (rung, canonical) = resolve_role(&members, entry_by_id, entries.is_empty(), projects)?;

    if rung == 2 {
        // descendants_of: BFS over parent links, cycle-safe. A rung-2 scope
        // is a SET: the board sees the nodes under EVERY member, so the walk
        // starts from each root, not just the first. resolve_role already
        // proved every root is an epic in `entries`.
        let mut ids: HashSet<String> = HashSet::new();
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in entries {
            if let (Some(id), Some(parent)) = (s_str(e, "id"), s_str(e, "parent")) {
                children.entry(parent).or_default().push(id);
            }
        }
        let mut frontier: Vec<&str> = Vec::new();
        for root_id in canonical.split(',') {
            ids.insert(root_id.to_string());
            if let Some(next) = children.get(root_id) {
                frontier.extend(next.iter().copied());
            }
        }
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(id) = frontier.pop() {
            if !seen.insert(id) {
                continue;
            }
            ids.insert(id.to_string());
            if let Some(next) = children.get(id) {
                frontier.extend(next.iter().copied());
            }
        }
        return Ok((canonical, ids));
    }

    let projects = projects.as_ref().ok();
    let project_set: HashSet<String> = canonical
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut ids = HashSet::new();
    for e in entries {
        let Some(id) = s_str(e, "id") else {
            continue;
        };
        let project = s_str(e, "project").unwrap_or("").to_string();
        let canonical_project = projects
            .and_then(|map| map.get(project.as_str()))
            .unwrap_or(&project);
        if project_set.contains(canonical_project) {
            ids.insert(id.to_string());
        }
    }
    Ok((canonical, ids))
}

/// The org ladder: the rung a team scope implies and its canonical stored
/// form, derived together so the two cannot disagree. Rung 0 is a portfolio
/// (2+ canonical projects), 1 one project, 2 a set of epics. Aliases resolve
/// before the count, so `alpha` and its short name `a` read as one project.
/// A non-epic node is work, not territory, and is refused, as is a scope that
/// mixes projects and epics. `graph_empty` says the graph read behind
/// `entry_of` returned no rows, so a missing member is unknown, not absent.
pub(crate) fn resolve_role<'a>(
    members: &[String],
    entry_of: impl Fn(&str) -> Option<&'a Value>,
    graph_empty: bool,
    projects: &Result<HashMap<String, String>, String>,
) -> Result<(u8, String), String> {
    use crate::org_board::s_str;
    let canonical = canonical_scope(&members.join(","));
    let members: Vec<&str> = canonical.split(',').filter(|m| !m.is_empty()).collect();
    if members.is_empty() {
        return Err("a team needs a scope: name an epic or a project".to_string());
    }
    // An unreadable project map means no alias is known. A member the graph
    // names as an epic needs no map.
    let alias = |m: &str| projects.as_ref().ok().and_then(|map| map.get(m)).cloned();
    let not_found = |m: &str| {
        if graph_empty {
            return format!("the graph read returned 0 nodes, so {m:?} is unknown, not absent");
        }
        let mut text = format!(
            "{m:?} is neither a configured project nor a backlog node; \
             nothing to lead over (check for a typo)"
        );
        if let Err(e) = projects {
            text.push_str(&format!(
                ". The project map could not be read ({e}), so a project name \
                 cannot resolve on this machine"
            ));
        }
        text
    };
    let resolved: Vec<(&str, Option<String>)> = members.iter().map(|&m| (m, alias(m))).collect();
    let project_names: Vec<String> = resolved.iter().filter_map(|(_, c)| c.clone()).collect();
    let epics: Vec<&str> = resolved
        .iter()
        .filter(|(_, c)| c.is_none())
        .map(|(m, _)| *m)
        .collect();

    if epics.is_empty() {
        let scope = canonical_scope(&project_names.join(","));
        // One project named twice collapses to one project, not a portfolio.
        let rung = if scope.contains(',') { 0 } else { 1 };
        return Ok((rung, scope));
    }
    if !project_names.is_empty() {
        let named: Vec<String> = resolved
            .iter()
            .map(|&(m, ref canon)| match (canon, entry_of(m)) {
                (Some(canon), _) => format!("{canon} (a project)"),
                (None, Some(e)) if s_str(e, "type") == Some("epic") => format!("{m} (an epic)"),
                (None, Some(e)) => {
                    format!(
                        "{m} (a {}, not an epic)",
                        s_str(e, "type").unwrap_or("node")
                    )
                }
                (None, None) => format!("{m} (not a configured project or a known node)"),
            })
            .collect();
        return Err(format!(
            "a multi-scope team rules PROJECTS or EPICS, never both at once: {}. \
             Name projects only (a portfolio) or epics only (a set).",
            named.join(", ")
        ));
    }
    // One message set for a single epic and an epic set alike: a member of a
    // set that is a typo or a non-epic node gets the remedy it would get alone.
    for &m in &epics {
        match entry_of(m) {
            None => return Err(not_found(m)),
            Some(e) if s_str(e, "type") == Some("epic") => {}
            Some(e) => {
                return Err(format!(
                    "{m:?} is a {}, not an epic. Implementers get no teams - a single \
                     node is work, not a territory. If {m} IS meant to be an epic: \
                     fno backlog update {m} type=epic. Otherwise team the epic above \
                     it, or its project.",
                    s_str(e, "type").unwrap_or("node")
                ))
            }
        }
    }
    Ok((2, epics.join(",")))
}

/// The `role-resolve` spawn-overlay kind: [`resolve_role`] served to Python.
/// A payload `projects` alias map and `entries` member rows stand in for this
/// machine's reads, so a caller keeps its own sources. An absent field reads
/// `project_map` and `graph_entries` from the cwd; an unreadable graph counts
/// as an empty one. Answer: `{"level", "scope"}` or `{"refusal"}`.
pub(crate) fn resolve_role_kind(payload: &Value) -> Result<Value, String> {
    use crate::org_board::s_str;
    let scopes: Vec<String> = payload
        .get("scopes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let projects = match payload.get("projects").and_then(Value::as_object) {
        Some(map) => Ok(map
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|c| (k.clone(), c.to_string())))
            .collect()),
        None => project_map(&cwd),
    };
    let (entries, graph_empty) = match payload.get("entries").and_then(Value::as_array) {
        Some(rows) => (rows.clone(), false),
        None => {
            let rows = graph_entries(&cwd).unwrap_or_default();
            let empty = rows.is_empty();
            (rows, empty)
        }
    };
    let entry_of = |id: &str| entries.iter().find(|e| s_str(e, "id") == Some(id));
    Ok(
        match resolve_role(&scopes, entry_of, graph_empty, &projects) {
            Ok((level, scope)) => json!({"level": level, "scope": scope}),
            Err(refusal) => json!({"refusal": refusal}),
        },
    )
}

/// Does a team over `outer` strictly contain one over `inner`? A project sits
/// in a portfolio by name, an epic carries its project, and an epic set is
/// contained when every member is. An equal scope is a peer, never contained.
/// The graph stores a node's project raw (the alias it was filed under), so the
/// row's project resolves through `projects` before the comparison.
pub(crate) fn scope_contains<'a>(
    outer: &str,
    inner: &str,
    entry_of: &dyn Fn(&str) -> Option<&'a Value>,
    projects: &HashMap<String, String>,
) -> bool {
    use crate::org_board::s_str;
    let outer_members = canonical_members(outer, projects);
    let inner_members = canonical_members(inner, projects);
    if outer_members.is_empty() || inner_members.is_empty() || inner_members == outer_members {
        return false;
    }
    if inner_members.len() > 1 {
        // A portfolio inside a wider portfolio, else an epic set: epic ids never
        // equal project names, so the set is placed member by member.
        return inner_members.is_subset(&outer_members)
            || inner
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .all(|m| scope_contains(outer, m, entry_of, projects));
    }
    let name = inner_members.iter().next().expect("one member");
    if outer_members.contains(name) {
        return true;
    }
    match entry_of(name.as_str()).and_then(|e| s_str(e, "project")) {
        Some(raw) if !raw.is_empty() => {
            outer_members.contains(projects.get(raw).map(String::as_str).unwrap_or(raw))
        }
        _ => false,
    }
}

/// Why this caller may not grant a team over `requested`, or `None`: you
/// cannot hand down authority you do not hold. An attended human may grant
/// anything. An agent must hold a live team that strictly contains the
/// request; an equal scope is a transfer, legal only with succession intent. A
/// caller this check cannot place (unreadable registry, unregistered agent,
/// terminal grantor) fails closed. `allow_terminal_recovery` admits only the
/// same territory a terminal holder already holds.
fn grant_refusal<'a>(
    payload: &Value,
    entry_of: &dyn Fn(&str) -> Option<&'a Value>,
    projects: &HashMap<String, String>,
) -> Option<String> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str).unwrap_or("");
    let flag = |key: &str| payload.get(key).and_then(Value::as_bool).unwrap_or(false);
    let caller = payload.get("caller").cloned().unwrap_or(Value::Null);
    let field = |key: &str| caller.get(key).and_then(Value::as_str).unwrap_or("");
    let requested = text("requested");
    match field("kind") {
        "human" => None,
        "unreadable" => Some(match text("stray_agent_self") {
            "" => "cannot verify the grantor's authority: the agent registry could not \
                   be read, so this session is treated as an agent whose authority is \
                   unknown, not as an attended human. A role may not be granted when \
                   the grantor cannot be checked; retry, or spawn from an attended shell."
                .to_string(),
            stray => format!(
                "cannot verify the grantor's authority: this shell carries \
                 FNO_AGENT_SELF={stray} with no harness session, so it reads \
                 as an agent fno cannot resolve, not an attended human. A \
                 shell that once hosted a worker keeps that variable. If you \
                 are the human at this shell, rerun as: \
                 env -u FNO_AGENT_SELF fno agents org promote <args>."
            ),
        }),
        "unregistered" => Some(
            "cannot verify the grantor's authority: this session carries an agent \
             identity but has no registry row (it spawned before its row landed, \
             or has not run /fno-me), so it is an agent with no verified authority, \
             not an attended human. Run /fno-me to join or wait for the row, then \
             retry; or spawn from an attended shell."
                .to_string(),
        ),
        "agent" => {
            let status = field("status");
            let held = field("role_scope");
            if crate::announce::TERMINAL_STATUSES.contains(&status) {
                if flag("allow_terminal_recovery") && same_territory(held, requested, projects) {
                    return None;
                }
                return Some(format!(
                    "cannot verify the grantor's authority: its STORED status is \
                     '{status}', which is terminal, so it cannot grant a role. \
                     Re-register the grantor in its live session or use an attended shell."
                ));
            }
            if held.is_empty() {
                return Some(format!(
                    "a role is handed DOWN, and this session holds none: an unpromoted \
                     agent cannot grant one. Ask a lead whose scope contains \
                     '{requested}', or spawn from an attended shell."
                ));
            }
            if same_territory(held, requested, projects) {
                if flag("allow_succession") {
                    return None;
                }
                return Some(format!(
                    "this session already holds its own role over '{requested}'; \
                     a same-scope spawn is a transfer, not a grant. Re-run with \
                     `--hand-off`, or choose a different scope so this session keeps its role."
                ));
            }
            if !scope_contains(held, requested, entry_of, projects) {
                return Some(format!(
                    "this session's role over '{held}' neither contains nor equals \
                     '{requested}', so it cannot grant it. A grant must be a \
                     strict subset of what the grantor holds; an equal scope is allowed \
                     only as succession, which hands YOUR scope to a successor."
                ));
            }
            None
        }
        other => Some(format!(
            "cannot verify the grantor's authority: unknown caller kind {other:?}"
        )),
    }
}

/// Live rows whose team the vacated scope contained and the new one does not:
/// advisory receipt data about a re-scope. `[]` when no territory left the new
/// scope, `None` when the graph could not be read, which is not `[]`. A single
/// member that is neither a known node nor a project is listed: its containment
/// is unknowable, and one stale row must not silence every determinate answer.
fn stranded_rows<'a>(
    payload: &Value,
    entry_of: &dyn Fn(&str) -> Option<&'a Value>,
    graph_read: bool,
    projects: &HashMap<String, String>,
) -> Option<Vec<String>> {
    use crate::org_board::s_str;
    let new_scope = payload
        .get("new_scope")
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some(vacated) = payload.get("vacated").and_then(Value::as_str) else {
        return Some(Vec::new());
    };
    if same_territory(vacated, new_scope, projects) {
        return Some(Vec::new());
    }
    if !graph_read {
        return None;
    }
    let target = payload
        .get("target_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let rows = payload
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut stranded = Vec::new();
    for row in &rows {
        let name = s_str(row, "name").unwrap_or("");
        let status = s_str(row, "status").unwrap_or("");
        if name == target || crate::announce::TERMINAL_STATUSES.contains(&status) {
            continue;
        }
        let scope = s_str(row, "role_scope").unwrap_or("");
        let members: Vec<&str> = scope
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .collect();
        let unresolvable = members.len() == 1
            && entry_of(members[0]).is_none()
            && !projects.contains_key(members[0]);
        if unresolvable
            || (scope_contains(vacated, scope, entry_of, projects)
                && !scope_contains(new_scope, scope, entry_of, projects))
        {
            stranded.push(name.to_string());
        }
    }
    Some(stranded)
}

/// The `role-grant` spawn-overlay kind: the grant decision (`op: check`) and
/// the re-scope strand scan (`op: stranded`), served to Python. The payload
/// carries the alias map and the member rows the caller read; `entries: null`
/// means the graph could not be read. Answers `{"refusal"}` or `{"stranded"}`.
pub(crate) fn role_grant_kind(payload: &Value) -> Result<Value, String> {
    use crate::org_board::s_str;
    let projects: HashMap<String, String> = payload
        .get("projects")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|c| (k.clone(), c.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let entries = payload.get("entries").and_then(Value::as_array);
    let graph_read = entries.is_some();
    let rows: Vec<Value> = entries.cloned().unwrap_or_default();
    let entry_of = |id: &str| rows.iter().find(|e| s_str(e, "id") == Some(id));
    match payload.get("op").and_then(Value::as_str) {
        Some("check") => Ok(json!({"refusal": grant_refusal(payload, &entry_of, &projects)})),
        Some("stranded") => Ok(json!({
            "stranded": stranded_rows(payload, &entry_of, graph_read, &projects)
        })),
        other => Err(format!(
            "role-grant: unknown op {other:?}; expected check|stranded"
        )),
    }
}

/// The canonical comma-joined form of a raw scope spelling: members split on
/// commas, trimmed, sorted, deduped. The same normalization the registry
/// cache readers apply before dedup.
pub(crate) fn canonical_scope(scope: &str) -> String {
    let mut members: Vec<String> = scope
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    members.sort();
    members.dedup();
    members.join(",")
}

// ---------------------------------------------------------------------------
// Territory resolution
// ---------------------------------------------------------------------------

/// Why a territory read failed. Never read as an empty list.
#[derive(Debug, Clone)]
pub struct TerritoryUnknown(pub String);

impl std::fmt::Display for TerritoryUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One territory: the canonical scope key, what it contains, and where its
/// drain roots. `project`/`cwd` are "" when the territory cannot be rooted
/// (the drain assembly skips an unrootable territory; the readout still
/// renders it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Territory {
    /// The canonical comma-joined scope string.
    pub key: String,
    /// The team rung of the scope's holder: 2 for an epic set, 1 for a
    /// project (portfolio or loose), 0 when the row carried no level.
    pub rung: u8,
    /// No live team holds the scope; the drain continues regardless.
    pub leadless: bool,
    /// Epic ids at rung 2, project names at rungs 0/1.
    pub members: Vec<String>,
    /// The root project the drain journal roots at: the first member epic's
    /// own project at rung 2, the project itself at rungs 0/1.
    pub project: String,
    /// The root project's workspace path, "" when unmapped.
    pub cwd: String,
}

/// One live team row: the canonical scope, its rung, its holder's name and
/// the holder's harness session id (the team-name store binds its record by
/// session id, law d-e952ed19 - never by the mutable row name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    pub scope: String,
    pub level: u8,
    pub holder: String,
    pub holder_session: Option<String>,
}

/// The `active_backlog` config block, coerced with the Python validator's
/// fail-safe truth table (`fno.config.ActiveBacklogConfig`): a bad scalar
/// drops to its default, an invalid interval disables, an ambiguous map value
/// disables that project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveBacklogFacts {
    /// True, or the per-project map (only clear affirmatives kept).
    pub enabled: Result<bool, HashMap<String, bool>>,
    /// Parsed poll floor; `None` disables the whole feature.
    pub interval_seconds: Option<i64>,
    pub failure_limit: u32,
    pub max_concurrent: u32,
}

fn coerce_affirmative(v: &toml::Value) -> bool {
    match v {
        toml::Value::Boolean(b) => *b,
        toml::Value::String(s) => matches!(s.trim(), "true" | "yes" | "on" | "1"),
        toml::Value::Integer(i) => *i != 0,
        _ => false,
    }
}

fn coerce_positive_int(v: &toml::Value, default: u32) -> u32 {
    match v.as_integer() {
        Some(i) if i > 0 => i as u32,
        _ => default,
    }
}

/// Mirror of the Python `_parse_duration_to_seconds`: `"5m"`/`"30s"`/`"2h"`/
/// `"1d"`, a bare digit string, or an integer; `None` for zero, negative, or
/// unparseable.
pub(crate) fn parse_duration_to_seconds(v: &toml::Value) -> Option<i64> {
    match v {
        toml::Value::Integer(i) => {
            let secs = *i;
            if secs > 0 {
                Some(secs)
            } else {
                None
            }
        }
        toml::Value::String(s) => {
            let s = s.trim();
            if let Ok(secs) = s.parse::<i64>() {
                return if secs > 0 { Some(secs) } else { None };
            }
            // Byte-slice only on a known ASCII unit suffix - never on a bare
            // trailing-byte count, which can land mid multi-byte UTF-8
            // character (e.g. an operator typo like "5é") and panic.
            let (mult, digits) = if let Some(d) = s.strip_suffix('s') {
                (1, d)
            } else if let Some(d) = s.strip_suffix('m') {
                (60, d)
            } else if let Some(d) = s.strip_suffix('h') {
                (3600, d)
            } else if let Some(d) = s.strip_suffix('d') {
                (86400, d)
            } else {
                return None;
            };
            if digits.is_empty() {
                return None;
            }
            let n: i64 = digits.parse().ok()?;
            let secs = n.checked_mul(mult)?;
            if secs > 0 {
                Some(secs)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Read `active_backlog` from the layered config the same way every other
/// Rust getter does (`config_lookup`), coercing with the Python truth table.
pub fn active_backlog_facts(cwd: &Path) -> ActiveBacklogFacts {
    let mut facts = ActiveBacklogFacts {
        enabled: Ok(false),
        interval_seconds: Some(300),
        failure_limit: 3,
        max_concurrent: 1,
    };
    let Some(block) = config_lookup(cwd, &["active_backlog"]) else {
        return facts;
    };
    let table = block.as_table().cloned().unwrap_or_default();
    match table.get("enabled") {
        None => {}
        Some(toml::Value::Boolean(b)) => facts.enabled = Ok(*b),
        Some(toml::Value::Table(map)) => {
            let mut out = HashMap::new();
            for (k, v) in map {
                out.insert(k.clone(), coerce_affirmative(v));
            }
            facts.enabled = Err(out);
        }
        Some(other) => facts.enabled = Ok(coerce_affirmative(other)),
    }
    if let Some(v) = table.get("interval") {
        facts.interval_seconds = parse_duration_to_seconds(v);
    }
    if let Some(v) = table.get("failure_limit") {
        facts.failure_limit = coerce_positive_int(v, 3);
    }
    if let Some(v) = table.get("max_concurrent") {
        facts.max_concurrent = coerce_positive_int(v, 1);
    }
    facts
}

impl ActiveBacklogFacts {
    /// Fail-closed: an invalid interval disables the feature entirely.
    pub fn any_enabled(&self) -> bool {
        if self.interval_seconds.is_none() {
            return false;
        }
        match &self.enabled {
            Ok(b) => *b,
            Err(map) => map.values().any(|v| *v),
        }
    }

    pub fn is_enabled_for(&self, project: Option<&str>) -> bool {
        if self.interval_seconds.is_none() {
            return false;
        }
        match &self.enabled {
            Ok(b) => *b,
            Err(map) => match project {
                None => false,
                Some(p) => map.get(p).copied().unwrap_or(false),
            },
        }
    }
}

/// Live team scopes from the registry cache, one per DISTINCT canonical
/// scope, in scope order - the read the org and the drain share. A registry
/// read fault is `TerritoryUnknown`, never an empty team list (the daemon
/// must not read "no leads" out of an unreadable registry).
pub fn live_teams(registry_path: &Path) -> Result<Vec<Team>, TerritoryUnknown> {
    let rows: Vec<RegistryEntry> = match load_registry(registry_path) {
        Ok(Registry { entries, .. }) => entries,
        Err(e) => {
            return Err(TerritoryUnknown(format!(
                "territory: registry unreadable ({e})"
            )))
        }
    };
    let mut out: Vec<Team> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for row in rows {
        let raw = row.role_scope.as_deref().unwrap_or("").trim();
        if raw.is_empty() || !crate::spawn_gate::status_is_liveish(&row.status) {
            continue;
        }
        let canon = canonical_scope(raw);
        if canon.is_empty() || !seen.insert(canon.clone()) {
            continue;
        }
        let level = row.role_level.unwrap_or(0).clamp(0, 255) as u8;
        out.push(Team {
            scope: canon,
            level,
            holder: row.name.clone(),
            holder_session: row.harness_session_id.clone(),
        });
    }
    out.sort_by(|a, b| a.scope.cmp(&b.scope));
    Ok(out)
}

/// Which live team answers for each node: the deepest team level whose
/// scope holds the node, then the lowest canonical scope on a tie. A team
/// whose scope does not compile owns nothing; it comes back in the second
/// list with its reason, and each caller decides whether that blind spot
/// refuses.
pub(crate) fn node_owners(
    teams: &[Team],
    entries: &[Value],
    projects: &Result<HashMap<String, String>, String>,
) -> (HashMap<String, String>, Vec<(String, String)>) {
    let mut sorted: Vec<&Team> = teams.iter().collect();
    sorted.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.scope.cmp(&b.scope)));
    let mut owners: HashMap<String, String> = HashMap::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    for team in sorted {
        match compile_territory(&team.scope, entries, projects) {
            Ok((_, ids)) => {
                for id in ids {
                    owners.entry(id).or_insert_with(|| team.scope.clone());
                }
            }
            Err(e) => failures.push((team.scope.clone(), e)),
        }
    }
    (owners, failures)
}

/// The owner read the org fold, the check-in's blueprint starts, and the
/// lead board's `unplanned` queue all answer from: node id -> owning scope,
/// plus the set of live scopes. A registry fault, or a live team whose scope
/// does not compile, is an `Err` naming why - never a map that silently
/// omits that team's nodes.
pub(crate) fn owner_read(
    registry_path: &Path,
    entries: &[Value],
    projects: &Result<HashMap<String, String>, String>,
) -> Result<(HashMap<String, String>, HashSet<String>), String> {
    let live = live_teams(registry_path).map_err(|e| e.0)?;
    let (map, failed) = node_owners(&live, entries, projects);
    if !failed.is_empty() {
        return Err(format!(
            "a live team's scope does not compile: {}",
            failed
                .iter()
                .map(|(s, e)| format!("{s}: {e}"))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    Ok((map, live.into_iter().map(|c| c.scope).collect()))
}

/// The workspace project map: canonical project name -> normalized absolute
/// path, the one map `backlog maintain` re-scopes against. Best-effort: a
/// missing map contributes nothing, and the drain assembly
/// treats an unmapped territory as unrootable.
pub fn workspace_paths(cwd: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(work) = config_lookup(cwd, &["work"]).and_then(|v| v.as_table().cloned()) else {
        return out;
    };
    // Multi-workspace shape: work.workspaces.<ws>.projects[] (an array of
    // tables, the only form the Python iterator accepts).
    if let Some(workspaces) = work.get("workspaces").and_then(|v| v.as_table().cloned()) {
        for (_ws, ws_data) in &workspaces {
            let Some(projects) = ws_data.get("projects").and_then(|p| p.as_array()) else {
                continue;
            };
            for project in projects {
                let Some(table) = project.as_table() else {
                    continue;
                };
                let Some(name) = table.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                if let Some(path) = table.get("path").and_then(|p| p.as_str()) {
                    if !path.is_empty() {
                        out.entry(name.to_string()).or_insert(normalize_path(path));
                    }
                }
            }
        }
    }
    // Legacy flat shape: work.projects.<name>.path.
    if let Some(flat) = work.get("projects").and_then(|v| v.as_table().cloned()) {
        for (name, cfg) in &flat {
            let Some(path) = cfg.get("path").and_then(|p| p.as_str()) else {
                continue;
            };
            if !name.is_empty() && !path.is_empty() {
                out.entry(name.clone())
                    .or_insert_with(|| normalize_path(path));
            }
        }
    }
    out
}

/// `os.path.normpath(expanduser(raw))` for the path shapes settings carry:
/// expand a leading `~`, then collapse `.`/`..`/duplicate separators
/// lexically (no filesystem access, matching normpath).
pub(crate) fn normalize_path(raw: &str) -> String {
    // A bare "~" expands like "~/" with an empty rest, matching Python's
    // os.path.expanduser (which treats both the same), not just the
    // slash-prefixed form.
    let expanded: String = if raw == "~" || raw.starts_with("~/") {
        let rest = raw.strip_prefix('~').unwrap().trim_start_matches('/');
        match std::env::var_os("HOME") {
            Some(home) if rest.is_empty() => home.to_string_lossy().into_owned(),
            Some(home) => format!("{}/{}", home.to_string_lossy(), rest),
            None => raw.to_string(),
        }
    } else {
        raw.to_string()
    };
    let absolute = expanded.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in expanded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                // `..` above the root collapses, matching normpath.
                if !parts.is_empty() && *parts.last().unwrap() != ".." {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// Graph entries at the resolved graph store, or the unknown naming the
/// read. Through the backend switch (`graph_store::read_rows_where_strict`):
/// under sqlite the file is a frozen mirror, and a boundary answered from
/// it asserts territory the store does not recognize. Strict, because an
/// unreadable graph is unknown, never an empty list.
pub(crate) fn graph_entries(config_cwd: &Path) -> Result<Vec<Value>, TerritoryUnknown> {
    let path = graph_json_path(config_cwd);
    crate::graph_store::read_rows_where_strict(
        &path,
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "id",
                    "type",
                    "parent",
                    "project",
                    "cwd",
                    "status",
                    "completed_at",
                    "superseded_by",
                    "deferred_at",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            with_blockers: true,
            ..Default::default()
        },
    )
    .map_err(|e| {
        TerritoryUnknown(format!(
            "territory: graph unreadable ({}): {e}",
            path.display()
        ))
    })
}

/// One territory per live team scope, plus one leadless rung-1 territory per
/// workspace project no live project-rung team rules - the team list seeds
/// the mission list, machinery never needs a lead to dispatch. Territories
/// come back in canonical scope order. A rung-2 territory whose first member
/// epic has no graph entry, or whose root project is unmapped in the
/// workspace, still resolves - with `project`/`cwd` empty - so the readout
/// can name it; the drain assembly skips what it cannot root.
pub fn resolve_territories(
    config_cwd: &Path,
    registry_path: &Path,
) -> Result<Vec<Territory>, TerritoryUnknown> {
    let teams = live_teams(registry_path)?;
    let entries = graph_entries(config_cwd)?;
    Ok(territories_in(&teams, &entries, config_cwd))
}

fn territories_in(teams: &[Team], entries: &[Value], config_cwd: &Path) -> Vec<Territory> {
    let paths = workspace_paths(config_cwd);

    let epic_project = |epic_id: &str| -> Option<String> {
        entries
            .iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(epic_id))
            .and_then(|e| e.get("project").and_then(Value::as_str))
            .filter(|p| !p.is_empty())
            .map(str::to_string)
    };

    let mut territories: Vec<Territory> = Vec::new();
    let mut ruled_projects: HashSet<String> = HashSet::new();
    for team in teams {
        let members: Vec<String> = team
            .scope
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if team.level != 2 {
            ruled_projects.extend(members.iter().cloned());
        }
        // Root at the first member epic's own project; the converge core
        // fans out across projects at dispatch time.
        let (project, cwd) = if team.level == 2 {
            match members.first().and_then(|m| epic_project(m)) {
                Some(p) => (p.clone(), paths.get(&p).cloned().unwrap_or_default()),
                None => (String::new(), String::new()),
            }
        } else {
            let p = members.first().cloned().unwrap_or_default();
            let cwd = paths.get(&p).cloned().unwrap_or_default();
            (p, cwd)
        };
        territories.push(Territory {
            key: team.scope.clone(),
            rung: team.level,
            leadless: false,
            members,
            project,
            cwd,
        });
    }
    let mut workspace_projects: Vec<&String> = paths.keys().collect();
    workspace_projects.sort();
    for name in workspace_projects {
        if ruled_projects.contains(name) {
            continue;
        }
        let cwd = paths.get(name).cloned().unwrap_or_default();
        territories.push(Territory {
            key: name.clone(),
            rung: 1,
            leadless: true,
            members: vec![name.clone()],
            project: name.clone(),
            cwd,
        });
    }
    // Python-order contract: teamed territories in scope order first, then
    // the leadless loose territories in project order (the readout renders
    // rows in exactly this order).
    territories
}

/// `fno-agents territory-rows`: the AC7 projection as JSON on stdout.
/// Daemon-free, invoked by the config passthroughs and the status fold.
pub fn run_territory_rows(args: &[String]) -> i32 {
    let _ = args;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let registry = AgentsHome::from_env().registry_json();
    let rows = territory_rows(&cwd, &registry);
    println!(
        "{}",
        serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())
    );
    0
}

/// `fno-agents active-backlog-receipt`: the drain reading as JSON on stdout,
/// the object shape (`targets`/`missions`/`skip_reason`). Exit 1 with
/// the reason on stderr when a source is unreadable - `unknown` must never
/// print as an empty list. Invoked by the `fno config active-backlog`
/// passthrough and rank's dispatcher note.
pub fn run_active_backlog_receipt(args: &[String]) -> i32 {
    // The status-fanout target resolver nests here as an argv op
    // (crate::status_sinks), so it adds no fno-agents root action (shrink
    // law d-fe66560a): receipt and fanout are the drain family.
    if args.first().map(String::as_str) == Some("status-sinks") {
        return crate::status_sinks::run(&args[1..]);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let registry = AgentsHome::from_env().registry_json();
    let report = crate::active_backlog::resolve_targets_report(&cwd, &registry);
    if let Some(failure) = &report.failure {
        eprintln!("active-backlog: {failure}");
        return 1;
    }
    let reading = serde_json::json!({
        "targets": report.targets,
        "missions": report.missions,
        "skip_reason": report.skip_reason,
    });
    println!(
        "{}",
        serde_json::to_string(&reading)
            .unwrap_or_else(|_| "{\"targets\":[],\"missions\":0}".to_string())
    );
    0
}

// ---------------------------------------------------------------------------
// The readout projection (AC7)
// ---------------------------------------------------------------------------

/// The per-territory cap: `agents.max_live_per_territory`, default 4, coerced
/// like every other positive-int knob.
pub fn territory_cap(config_cwd: &Path) -> u32 {
    config_lookup(config_cwd, &["agents", "max_live_per_territory"])
        .map(|v| coerce_positive_int(&v, 4))
        .unwrap_or(4)
}

/// Live and suspect node lockfiles are the single holder answer. `false`
/// excludes claims proven stale; a failed directory scan stays unknown.
fn live_node_claims_from(
    directory: Option<std::path::PathBuf>,
) -> Result<HashSet<String>, TerritoryUnknown> {
    let directory = directory.ok_or_else(|| {
        TerritoryUnknown("territory: claims path unavailable from FNO_CLAIMS_ROOT or HOME".into())
    })?;
    let records = crate::claims::list_in_strict(&[directory], Some("node:"), false)
        .map_err(|error| TerritoryUnknown(format!("territory: claims unreadable: {error}")))?;
    Ok(records
        .into_iter()
        .filter_map(|record| record.key.strip_prefix("node:").map(str::to_string))
        .collect())
}

pub(crate) fn live_node_claims() -> Result<HashSet<String>, TerritoryUnknown> {
    live_node_claims_from(crate::claims::claims_dir_for(None))
}

pub(crate) fn live_held_in(node_ids: &HashSet<String>, held: &HashSet<String>) -> usize {
    node_ids.intersection(held).count()
}

/// One readout row per territory: scope, membership state, rung, leadless
/// state, team holder, mission, and live count against the cap. The
/// projection the status payload, the hidden config
/// command, the lead check-in, and the operational probe share, so none of them
/// can disagree. Fail-safe per row: an unreadable source shrinks that row's
/// answer (membership "unknown", live null), never the whole
/// projection.
pub fn territory_rows(config_cwd: &Path, registry_path: &Path) -> Vec<Value> {
    let cap = territory_cap(config_cwd);
    let teams = match live_teams(registry_path) {
        Ok(teams) => teams,
        Err(TerritoryUnknown(reason)) => {
            return vec![json!({"membership": "unknown", "reason": reason, "cap": cap})]
        }
    };
    let entries = match graph_entries(config_cwd) {
        Ok(entries) => entries,
        Err(TerritoryUnknown(reason)) => {
            return vec![json!({"membership": "unknown", "reason": reason, "cap": cap})]
        }
    };
    let territories = territories_in(&teams, &entries, config_cwd);
    let live_node_claims = match live_node_claims() {
        Ok(claims) => claims,
        Err(TerritoryUnknown(reason)) => {
            return vec![json!({"membership": "unknown", "reason": reason, "cap": cap})]
        }
    };
    let holders: HashMap<String, String> = teams
        .iter()
        .map(|c| (c.scope.clone(), c.holder.clone()))
        .collect();
    // Exclusive membership: a node counts for the one live team that owns
    // it - the deepest team holding it, the same rule `node_owners` gives
    // the spawn gate and the org - so a worker can never cost two
    // territories at once. An unowned node stays loose.
    let (owners, _) = node_owners(&teams, &entries, &project_map(config_cwd));

    let memberships: Vec<(&Territory, Result<(String, HashSet<String>), String>)> = territories
        .iter()
        .map(|territory| {
            (
                territory,
                compile_territory(&territory.key, &entries, &project_map(config_cwd)),
            )
        })
        .collect();

    memberships
        .into_iter()
        .map(|(territory, compiled)| {
            let (membership, ids, reason) = match compiled {
                Ok((_, ids)) => ("ok", ids, None),
                Err(reason) => ("unknown", HashSet::new(), Some(reason)),
            };
            let mut ids = ids;
            if membership == "ok" {
                if territory.leadless {
                    ids.retain(|id| !owners.contains_key(id));
                } else {
                    ids.retain(|id| owners.get(id) == Some(&territory.key));
                }
            }
            let live_count = if membership == "ok" {
                Some(live_held_in(&ids, &live_node_claims))
            } else {
                None
            };
            let mut row = json!({
                "scope": territory.key,
                "membership": membership,
                "rung": territory.rung,
                "leadless": territory.leadless,
                "holder": holders.get(&territory.key),
                "mission": if territory.rung == 2 { territory.members.first() } else { None },
                "live": live_count,
                "cap": cap,
            });
            if let Some(reason) = reason {
                row["reason"] = json!(reason);
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod moved_scope_tests {
    use super::*;
    use serde_json::json;
    fn node(id: &str, status: &str, priority: &str) -> Value {
        json!({"id": id, "slug": id, "status": status, "priority": priority, "type": "feature"})
    }

    #[test]
    fn a_non_epic_single_scope_is_refused() {
        let entries = vec![node("x-aaaa", "ready", "p1")];
        let projects = Ok(HashMap::new());
        let err = compile_scope_ids("x-aaaa", &entries, &projects).unwrap_err();
        assert!(err.contains("not an epic"), "{err}");
        let empty = compile_scope_ids("x-aaaa", &[], &projects).unwrap_err();
        assert!(
            empty.contains("returned 0 nodes") && empty.contains("unknown, not absent"),
            "{empty}"
        );
        let missing = compile_scope_ids("x-ghost", &entries, &projects).unwrap_err();
        assert!(
            missing.contains("neither a configured project nor a backlog node"),
            "{missing}"
        );
    }

    #[test]
    fn a_multi_scope_team_of_epics_compiles_as_the_union() {
        // The Python twin (lead/scope.py) rules a rung-2 SET of epics: the
        // board sees the nodes under EVERY member, not just the first. This
        // twin refused any multi-scope that was not all projects, so a set
        // team never resolved its board.
        let entries = vec![
            json!({"id": "e-1", "type": "epic", "status": "ready", "priority": "p1"}),
            json!({"id": "e-1a", "parent": "e-1", "status": "ready", "priority": "p1"}),
            json!({"id": "e-2", "type": "epic", "status": "ready", "priority": "p1"}),
            json!({"id": "e-2a", "parent": "e-2", "status": "ready", "priority": "p1"}),
            json!({"id": "x-outs", "status": "ready", "priority": "p1"}),
        ];
        let projects = Ok(HashMap::new());
        let ids = compile_scope_ids("e-2,e-1", &entries, &projects).unwrap();
        for expected in ["e-1", "e-1a", "e-2", "e-2a"] {
            assert!(ids.contains(expected), "missing {expected}: {ids:?}");
        }
        assert!(!ids.contains("x-outs"));
    }

    #[test]
    fn a_mixed_project_and_epic_multi_scope_is_refused() {
        let entries =
            vec![json!({"id": "e-1", "type": "epic", "status": "ready", "priority": "p1"})];
        let mut map = HashMap::new();
        map.insert("alpha".to_string(), "alpha".to_string());
        let err = compile_scope_ids("alpha,e-1", &entries, &Ok(map)).unwrap_err();
        assert!(err.contains("never both at once"), "{err}");
    }

    #[test]
    fn an_unreadable_project_map_is_named_in_a_refusal_not_hidden() {
        let entries = vec![json!({"id": "x-epic", "type": "epic", "status": "ready"})];
        let map_error = crate::org_board::scope::map_from_work(None).unwrap_err();
        for teaches in ["work.workspaces", "~/.fno/config.toml", "projects", "path"] {
            assert!(map_error.contains(teaches), "{teaches}: {map_error}");
        }
        let projects = Err(map_error.clone());
        let missing = compile_scope_ids("fno", &entries, &projects).unwrap_err();
        assert!(
            missing.contains("neither a configured project nor a backlog node")
                && missing.contains(&map_error),
            "{missing}"
        );
        let empty = compile_scope_ids("fno", &[], &projects).unwrap_err();
        assert!(
            empty.contains("returned 0 nodes") && empty.contains("unknown, not absent"),
            "{empty}"
        );
    }
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use serde_json::json;

    /// Interlocked, restoring env scope: holds the shared env lock and
    /// restores the variables these tests pin on drop, so an assert-panic
    /// mid-test cannot leak a pin toward a deleted tempdir into every later
    /// test in the process.
    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn take() -> Self {
            let lock = crate::claims::test_env_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let saved = ["FNO_CONFIG", "FNO_HOME", "FNO_CLAIMS_ROOT"]
                .iter()
                .map(|var| (*var, std::env::var_os(var)))
                .collect();
            Self { _lock: lock, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (var, saved) in &self.saved {
                match saved {
                    Some(v) => std::env::set_var(var, v),
                    None => std::env::remove_var(var),
                }
            }
        }
    }

    fn env_guard() -> EnvGuard {
        EnvGuard::take()
    }

    fn write_fixture(
        dir: &Path,
        config: &str,
        graph: Value,
        registry_rows: Value,
    ) -> (PathBuf, PathBuf) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("config.toml"), config).unwrap();
        crate::graph_store::seed_rows(
            &dir.join("graph.json"),
            graph["entries"].as_array().unwrap(),
        )
        .unwrap();
        let registry_path = dir.join("registry.json");
        crate::registry_store::seed_raw(&registry_path, registry_rows.to_string());
        (dir.to_path_buf(), registry_path)
    }

    const BASE_CONFIG: &str = "\
[active_backlog]
enabled = true
interval = \"5m\"
failure_limit = 3
max_concurrent = 2
[[work.workspaces.main.projects]]
name = \"alpha\"
path = \"/repo/alpha\"
";

    fn graph_fixture() -> Value {
        json!({"entries": [
            {"id": "e-1", "type": "epic", "project": "alpha", "status": "in_progress", "priority": "p1"},
            {"id": "e-1a", "parent": "e-1", "project": "alpha", "status": "ready", "priority": "p1"},
            {"id": "e-loose", "project": "alpha", "status": "ready", "priority": "p1"},
            {"id": "e-outs", "project": "beta", "status": "ready", "priority": "p1"}
        ]})
    }

    fn registry_fixture() -> Value {
        let mut v = json!({"agents": [
            {"name": "lead-a", "status": "live", "role_scope": "e-1", "role_level": 2,
             "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z"},
            {"name": "w-1", "status": "live", "node": "e-1a", "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z",
             "pid": std::process::id()},
            {"name": "w-2", "status": "exited", "node": "e-1a", "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z"}
        ]});
        v["schema_version"] = json!(crate::state::REGISTRY_SCHEMA_VERSION);
        v
    }

    fn fixture_env() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("FNO_CONFIG", tmp.path().join("config.toml"));
        std::env::set_var("FNO_HOME", tmp.path());
        std::env::set_var("FNO_CLAIMS_ROOT", tmp.path());
        let (cwd, registry) =
            write_fixture(tmp.path(), BASE_CONFIG, graph_fixture(), registry_fixture());
        (tmp, cwd, registry)
    }

    #[test]
    fn resolve_returns_teamed_and_leadless_territories_in_scope_order() {
        let _env = env_guard();
        let (_tmp, cwd, registry) = fixture_env();
        let ts = resolve_territories(&cwd, &registry).unwrap();
        let scopes: Vec<&str> = ts.iter().map(|t| t.key.as_str()).collect();
        // The rung-2 team over e-1 (rooted in alpha) plus alpha's loose
        // rung-1 territory: the team rules its descendants, not the
        // project's parentless nodes. Teams come first in scope order, then
        // the leadless loose territories sorted - the recorded Python order.
        assert_eq!(scopes, ["e-1", "alpha"]);
        assert_eq!(ts[0].rung, 2);
        assert!(!ts[0].leadless);
        assert_eq!(ts[0].members, ["e-1"]);
        assert_eq!(ts[0].project, "alpha");
        assert_eq!(ts[1].rung, 1);
        assert!(ts[1].leadless);
        assert_eq!(ts[1].project, "alpha");
        assert_eq!(ts[1].cwd, "/repo/alpha");
    }

    #[test]
    fn an_unreadable_graph_is_unknown_never_an_empty_list() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("FNO_CONFIG", tmp.path().join("config.toml"));
        std::env::set_var("FNO_HOME", tmp.path());
        let (_cwd, _) = write_fixture(
            tmp.path(),
            BASE_CONFIG,
            json!({"entries": []}),
            json!({"schema_version": 1, "agents": []}),
        );
        // The registry shares graph.db with its own root, so it lives under
        // a second root here: only the graph reads corrupt.
        let registry = tmp.path().join("registry-root").join("registry.json");
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        crate::registry_store::seed_raw(&registry, registry_fixture().to_string());
        // The store is graph.db beside the anchor name; absent reads as an
        // empty store (open creates it), so unreadable = corrupt db file.
        let db = crate::backlog::database_path(&tmp.path().join("graph.json"));
        std::fs::write(&db, b"not a database").unwrap();
        let err = resolve_territories(&tmp.path().to_path_buf(), &registry).unwrap_err();
        assert!(err.0.contains("graph unreadable"), "{err}");
        crate::paths::pin_test_claims_root(tmp.path());
        let rows = territory_rows(&tmp.path().to_path_buf(), &registry);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["membership"], "unknown");
        assert!(
            rows[0]["reason"]
                .as_str()
                .unwrap()
                .contains("graph unreadable"),
            "{rows:?}"
        );
    }

    #[test]
    fn an_unreadable_registry_is_unknown_never_no_leads() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("FNO_CONFIG", tmp.path().join("config.toml"));
        std::env::set_var("FNO_HOME", tmp.path());
        let (_cwd, _registry) = write_fixture(
            tmp.path(),
            BASE_CONFIG,
            graph_fixture(),
            json!({"agents": []}),
        );
        // A MISSING registry reads as an empty one (fail-open, the Python
        // loader's contract); a CORRUPT one is the unknown case.
        crate::registry_store::seed_raw(&tmp.path().join("registry.json"), "{not json");
        let registry = tmp.path().join("registry.json");
        let err = resolve_territories(&tmp.path().to_path_buf(), &registry).unwrap_err();
        assert!(err.0.contains("registry unreadable"), "{err}");
    }

    #[test]
    fn facts_coerce_with_the_python_truth_table() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("FNO_CONFIG", tmp.path().join("config.toml"));
        std::fs::write(
            tmp.path().join("config.toml"),
            r#"[active_backlog]
enabled = "banana"
interval = "0s"
failure_limit = "x"
max_concurrent = 0
"#,
        )
        .unwrap();
        let facts = active_backlog_facts(tmp.path());
        assert_eq!(
            facts.enabled,
            Ok(false),
            "an ambiguous scalar fails safe to off"
        );
        assert_eq!(facts.interval_seconds, None, "an invalid interval disables");
        assert_eq!(facts.failure_limit, 3, "a bad scalar drops to the default");
        assert_eq!(
            facts.max_concurrent, 1,
            "a non-positive cap drops to the default"
        );
    }

    #[test]
    fn parse_duration_to_seconds_never_panics_on_a_non_ascii_suffix() {
        // A malformed operator value ending mid multi-byte UTF-8 character
        // must fall back to None, never panic the process.
        let v = toml::Value::String("5\u{e9}".to_string());
        assert_eq!(parse_duration_to_seconds(&v), None);
        let v = toml::Value::String("5m".to_string());
        assert_eq!(parse_duration_to_seconds(&v), Some(300));
    }

    #[test]
    fn territory_rows_project_the_ac7_row_shape() {
        let _env = env_guard();
        let (tmp, cwd, registry) = fixture_env();
        // The live count reads claim lockfiles (the holder of record), so
        // the fixture seeds the claim w-1's registry row implies.
        acquire_node_claim(tmp.path(), "e-1a", Some(std::process::id()), None, false);
        let rows = territory_rows(&cwd, &registry);
        assert_eq!(rows.len(), 2);
        let loose = rows.iter().find(|r| r["scope"] == "alpha").unwrap();
        let teamed = rows.iter().find(|r| r["scope"] == "e-1").unwrap();
        assert_eq!(loose["leadless"], true);
        assert_eq!(loose["rung"], 1);
        assert_eq!(loose["live"], 0);
        assert_eq!(loose["cap"], 4);
        assert_eq!(teamed["holder"], "lead-a");
        assert_eq!(teamed["mission"], "e-1");
        assert_eq!(teamed["live"], 1, "w-1 works e-1a inside the team scope");
    }

    #[test]
    fn territory_rows_count_each_worker_in_exactly_one_row() {
        let _env = env_guard();
        let (tmp, cwd, registry) = fixture_env();
        // Seed the claims the two live workers' registry rows imply.
        acquire_node_claim(tmp.path(), "e-1a", Some(std::process::id()), None, false);
        acquire_node_claim(tmp.path(), "e-loose", Some(std::process::id()), None, false);
        std::fs::write(
            &registry,
            json!({"schema_version": crate::state::REGISTRY_SCHEMA_VERSION, "agents": [
                {"name": "lead-p", "status": "live", "role_scope": "alpha", "role_level": 1,
                 "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z"},
                {"name": "lead-a", "status": "live", "role_scope": "e-1", "role_level": 2,
                 "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z"},
                {"name": "w-1", "status": "live", "node": "e-1a", "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z",
                 "pid": std::process::id()},
                {"name": "w-loose", "status": "live", "node": "e-loose", "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z",
                 "pid": std::process::id()}
            ]})
            .to_string(),
        )
        .unwrap();
        let rows = territory_rows(&cwd, &registry);
        let loose = rows.iter().find(|r| r["scope"] == "alpha").unwrap();
        let teamed = rows.iter().find(|r| r["scope"] == "e-1").unwrap();
        assert_eq!(
            teamed["live"], 1,
            "e-1a's worker counts for e-1 only: {rows:?}"
        );
        assert_eq!(
            loose["live"], 1,
            "e-loose's worker counts for alpha only: {rows:?}"
        );
    }

    fn acquire_node_claim(
        root: &Path,
        node: &str,
        pid: Option<u32>,
        ttl_ms: Option<i64>,
        pid_unavailable: bool,
    ) -> crate::claims::ClaimRecord {
        match crate::claims::acquire(
            &format!("node:{node}"),
            "target-session:test",
            crate::claims::AcquireOpts {
                pid,
                ttl_ms,
                pid_unavailable,
                root: Some(root.to_path_buf()),
                events_dir: Some(root.to_path_buf()),
                ..Default::default()
            },
        ) {
            crate::claims::AcquireOutcome::Acquired(record) => record,
            other => panic!("node claim fixture failed: {other:?}"),
        }
    }

    fn reaped_pid() -> u32 {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn territory_rows_count_live_and_suspect_node_claims_not_registry_node_fields() {
        let _env = env_guard();
        let (tmp, cwd, registry) = fixture_env();
        std::fs::write(
            &registry,
            json!({"schema_version": crate::state::REGISTRY_SCHEMA_VERSION, "agents": [
                {"name": "lead-a", "status": "live", "role_scope": "e-1", "role_level": 2,
                 "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z"},
                {"name": "w-1", "status": "live", "node": null, "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z",
                 "pid": std::process::id()},
                {"name": "w-2", "status": "live", "node": null, "cwd": "/repo/alpha", "harness": "claude", "created_at": "2026-09-07T00:00:00Z",
                 "pid": std::process::id()}
            ]})
            .to_string(),
        )
        .unwrap();
        let live = acquire_node_claim(tmp.path(), "e-1a", Some(std::process::id()), None, false);
        let suspect = acquire_node_claim(tmp.path(), "e-1", None, Some(60_000), true);
        let stale = acquire_node_claim(tmp.path(), "e-loose", Some(reaped_pid()), None, false);
        assert_eq!(
            crate::claims::classify(&live, None),
            crate::claims::ClaimState::Live
        );
        assert_eq!(
            crate::claims::classify(&suspect, None),
            crate::claims::ClaimState::Suspect
        );
        assert_eq!(
            crate::claims::classify(&stale, None),
            crate::claims::ClaimState::Stale
        );

        let rows = territory_rows(&cwd, &registry);
        let teamed = rows.iter().find(|r| r["scope"] == "e-1").unwrap();
        let loose = rows.iter().find(|r| r["scope"] == "alpha").unwrap();
        assert_eq!(
            teamed["live"], 2,
            "live and suspect node claims count: {rows:?}"
        );
        assert_eq!(
            loose["live"], 0,
            "a stale node claim does not count: {rows:?}"
        );

        // Same fixture, the degrade arm: an unreadable claims root answers
        // unknown, never an implicit zero row.
        let (tmp, cwd, registry) = fixture_env();
        std::fs::create_dir_all(tmp.path().join(".fno")).unwrap();
        std::fs::write(tmp.path().join(".fno/claims"), "not a directory").unwrap();
        let rows = territory_rows(&cwd, &registry);
        assert_eq!(
            rows.len(),
            1,
            "unknown store must not emit a zero row: {rows:?}"
        );
        assert_eq!(rows[0]["membership"], "unknown");
        assert!(
            rows[0]["reason"].as_str().unwrap().contains("claims"),
            "{rows:?}"
        );
        assert!(rows[0]["live"].is_null(), "{rows:?}");

        // And with no root to resolve at all: unavailable, not zero.
        let error = live_node_claims_from(None).unwrap_err();
        assert!(error.0.contains("claims path unavailable"), "{error:?}");
    }
}

#[cfg(test)]
mod grant_tests {
    use super::*;
    use serde_json::json;

    fn projects() -> HashMap<String, String> {
        [("alpha", "alpha"), ("a", "alpha"), ("beta", "beta")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn graph() -> Vec<Value> {
        vec![
            json!({"id": "e-1", "type": "epic", "project": "alpha"}),
            json!({"id": "e-2", "type": "epic", "project": "a"}),
            json!({"id": "e-3", "type": "epic", "project": "beta"}),
        ]
    }

    fn contains(outer: &str, inner: &str) -> bool {
        let rows = graph();
        let entry_of = |id: &str| rows.iter().find(|e| e["id"] == id);
        scope_contains(outer, inner, &entry_of, &projects())
    }

    #[test]
    fn containment_holds_across_the_ladder_and_through_aliases() {
        assert!(contains("alpha,beta", "alpha")); // project in portfolio
        assert!(contains("alpha", "e-1")); // epic in its project
        assert!(contains("alpha", "e-2")); // epic filed under the alias
        assert!(contains("alpha,beta", "e-3"));
        assert!(!contains("beta", "e-1")); // epic of another project
        assert!(!contains("alpha", "alpha")); // a peer, not below
        assert!(contains("a,beta", "alpha")); // aliases normalize
        assert!(contains("e-1,e-2", "e-1")); // a set holds each member
        assert!(!contains("e-2,e-1", "e-1,e-2")); // a peer set
        assert!(!contains("e-1", "e-1,e-2")); // narrower holds less
        assert!(contains("alpha", "e-1,e-2")); // every member in alpha
        assert!(!contains("alpha", "e-1,e-3")); // spans two projects
        assert!(!contains("alpha", "e-ghost")); // unknown node
    }

    fn check(caller: Value, requested: &str, succession: bool) -> Option<String> {
        let answer = role_grant_kind(&json!({
            "op": "check", "requested": requested, "caller": caller,
            "allow_succession": succession, "allow_terminal_recovery": false,
            "projects": projects(), "entries": graph(),
        }))
        .unwrap();
        answer["refusal"].as_str().map(str::to_string)
    }

    #[test]
    fn the_grant_check_admits_only_what_the_grantor_holds() {
        let agent = |status: &str, scope: &str| json!({"kind": "agent", "status": status, "role_scope": scope});
        assert_eq!(check(json!({"kind": "human"}), "anything", false), None);
        assert_eq!(check(agent("busy", "alpha,beta"), "e-1", false), None);
        let outside = check(agent("busy", "beta"), "e-1", false).unwrap();
        assert!(outside.contains("neither contains nor equals"), "{outside}");
        let peer = check(agent("busy", "a"), "alpha", false).unwrap();
        assert!(peer.contains("`--hand-off`"), "{peer}");
        assert_eq!(check(agent("busy", "a"), "alpha", true), None);
        let unpromoted = check(agent("idle", ""), "e-1", false).unwrap();
        assert!(unpromoted.contains("holds none"), "{unpromoted}");
        let dead = check(agent("exited", "alpha"), "e-1", false).unwrap();
        assert!(dead.contains("'exited', which is terminal"), "{dead}");
        for kind in ["unreadable", "unregistered", "bogus"] {
            let refused = check(json!({"kind": kind}), "e-1", false).unwrap();
            assert!(
                refused.starts_with("cannot verify the grantor's authority"),
                "{refused}"
            );
        }
    }

    #[test]
    fn the_strand_scan_lists_roles_the_move_left_behind() {
        let rows = json!([
            {"name": "mover", "status": "busy", "role_scope": "beta"},
            {"name": "sub-1", "status": "busy", "role_scope": "e-1"},
            {"name": "sub-3", "status": "busy", "role_scope": "e-3"},
            {"name": "gone", "status": "exited", "role_scope": "e-1"},
            {"name": "stale", "status": "busy", "role_scope": "e-ghost"},
        ]);
        let scan = |vacated: Value, entries: Value| {
            role_grant_kind(&json!({
                "op": "stranded", "vacated": vacated, "new_scope": "beta",
                "target_name": "mover", "rows": rows, "projects": projects(),
                "entries": entries,
            }))
            .unwrap()["stranded"]
                .clone()
        };
        assert_eq!(
            scan(json!("alpha"), json!(graph())),
            json!(["sub-1", "stale"])
        );
        assert_eq!(scan(json!("alpha"), Value::Null), Value::Null);
        assert_eq!(scan(Value::Null, Value::Null), json!([]));
        assert_eq!(scan(json!("beta"), Value::Null), json!([]));
    }
}
