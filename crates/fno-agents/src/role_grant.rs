//! `fno agents role` (`fno agents org promote`): grant, re-scope or reclaim a
//! role on an existing registered session. A leaf with no action of its own:
//! the router rewrites it into `status --role`, because the binary's action
//! list is shrink-only.
//!
//! Every decision is one of the native kinds the spawn-time promote door
//! already calls: `role-resolve` derives the rung, `role-grant` checks the
//! grantor's authority and scans for stranded subordinates, `team-widen`
//! admits a self-edit, `spawn-team` arms the lead manifest and the missions,
//! `team-rescope` carries the team name. This module only sequences them and
//! writes the registry under its lock.

use crate::client_commands::{py_repr_str, read_registry_entries, resolve_entry_with_heal};
use crate::paths::AgentsHome;
use crate::spawn_promote::{calling_row, Calling};
use crate::state::RegistryEntry;
use crate::wake_read::python_json_default;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

const TERMINAL: [&str; 4] = ["exited", "orphaned", "failed", "permanent_dead"];

const HELP: &str = "\
Usage: fno agents role [OPTIONS] [HANDLE]

  Role an existing session from an attended shell, from an agent whose own
  role strictly contains the requested scope, or add to your own epic-set
  role an epic your session created.

  A row already holding a role is re-scoped rather than refused. --reclaim
  returns a transferred role to its recorded grantor and never creates a
  session.

Options:
  --scope TEXT  Territory to grant. Repeat for a multi-project portfolio or a
                set of epics; the role level is derived.
  --reclaim     Return the current holder's role to its recorded grantor.
  --help        Show this message and exit.
";

/// `status --role HANDLE --scope S... | --reclaim [HANDLE]`.
pub fn run_role(args: &[String]) -> i32 {
    let args = crate::client_commands::expand_eq(args);
    let (mut handle, mut scopes, mut reclaim) = (String::new(), Vec::new(), false);
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return 0;
            }
            "--reclaim" => reclaim = true,
            "--scope" => match it.next() {
                Some(v) => scopes.push(v),
                None => {
                    eprintln!("role: --scope requires a value");
                    return 2;
                }
            },
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("role: no such option: {s}");
                return 2;
            }
            _ if handle.is_empty() => handle = arg,
            s => {
                eprintln!("role: got unexpected extra argument ({s})");
                return 2;
            }
        }
    }
    let home = AgentsHome::from_env();
    let events = crate::client_commands::trace_events_path(&home);
    if reclaim {
        return match reclaim_role(&home, (!handle.is_empty()).then_some(handle.as_str())) {
            Ok(receipt) => {
                let fields: Vec<(&str, Value)> = ["reclaimed", "from_holder", "scope", "grantor"]
                    .into_iter()
                    .map(|k| (k, receipt[k].clone()))
                    .collect();
                crate::claude_ask::emit_event(&events, "agent_role_reclaimed", &fields);
                println!("{}", python_json_default(&receipt));
                0
            }
            Err(e) => {
                eprintln!("role reclaim: {e}");
                2
            }
        };
    }
    if handle.is_empty() || scopes.is_empty() {
        eprintln!("role: HANDLE and at least one --scope are required");
        return 2;
    }
    match promote(&home, &handle, &scopes) {
        Ok(receipt) => {
            let fields: Vec<(&str, Value)> = vec![
                ("name", receipt["promoted"].clone()),
                ("level", receipt["level"].clone()),
                ("scope", receipt["scope"].clone()),
                ("grantor", receipt["grantor"].clone()),
                ("vacated_scope", receipt["vacated_scope"].clone()),
                ("vacated_level", receipt["vacated_level"].clone()),
                (
                    "stranded_subordinates",
                    receipt["stranded_subordinates"].clone(),
                ),
            ];
            crate::claude_ask::emit_event(&events, "agent_promoted", &fields);
            println!("{}", python_json_default(&receipt));
            0
        }
        Err(e) => {
            eprintln!("role: {e}");
            2
        }
    }
}

fn status_of(row: &RegistryEntry) -> String {
    serde_json::to_value(row.status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn terminal(row: &RegistryEntry) -> bool {
    TERMINAL.contains(&status_of(row).as_str())
}

/// The row's session identity, in the precedence every team reader uses:
/// `harness_session_id`, then `cc_session_id`, then `short_id`. The manifest
/// arm, the manifest removal, the team-name carry and the lock-time identity
/// match all key on this one answer, so a re-scope never binds the role to
/// one id while the next check-in looks it up by another.
pub(crate) fn holder_session(row: &RegistryEntry) -> Option<String> {
    [
        row.harness_session_id.as_deref(),
        row.cc_session_id.as_deref(),
        Some(row.short_id.as_str()),
    ]
    .into_iter()
    .flatten()
    .find(|s| !s.is_empty())
    .map(str::to_string)
}

/// The row resolved BEFORE the lock, found again under it by name AND
/// session, so a name rebound inside the window is never stamped.
fn locked<'a>(
    rows: &'a mut [RegistryEntry],
    expect: &RegistryEntry,
) -> Option<&'a mut RegistryEntry> {
    let session = holder_session(expect);
    rows.iter_mut()
        .find(|r| r.name == expect.name && holder_session(r) == session)
}

fn projects() -> HashMap<String, String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    crate::org_board::project_map(&cwd).unwrap_or_default()
}

fn graph_entries() -> Vec<Value> {
    let cwd = std::env::current_dir().unwrap_or_default();
    crate::territory::graph_entries(&cwd).unwrap_or_default()
}

fn resolve_target(home: &AgentsHome, handle: &str) -> Result<RegistryEntry, String> {
    let registry = home.registry_json();
    let rows = read_registry_entries(&registry)?;
    let row = resolve_entry_with_heal(&rows, handle, &registry).map_err(|e| e.message())?;
    serde_json::from_value(row).map_err(|e| format!("registry row for {handle} is unreadable: {e}"))
}

/// Arm the scope's lead manifest for `target`: `Ok(path)` when armed, `Ok(None)`
/// when the lead loop is off.
fn arm_manifest(
    target: &RegistryEntry,
    scope: &str,
    level: u32,
    grantor: &str,
) -> Result<Option<String>, String> {
    let answer = crate::team_spawn::resolve(&json!({
        "op": "arm", "scope": scope,
        "harness_session_id": holder_session(target).unwrap_or_default(),
        "owner_cwd": target.cwd, "role_level": level, "role_scope": scope,
        "role_grantor": grantor, "model": target.requested_model, "harness": target.harness,
    }))?;
    if let Some(e) = answer["error"].as_str() {
        return Err(e.to_string());
    }
    Ok(answer["path"].as_str().map(str::to_string))
}

fn remove_manifest(scope: &str, owner_cwd: &str, expected_session: &str) {
    let _ = crate::lead_ops::overlay_kind(&json!({
        "kind": "lead", "op": "remove", "scope": scope, "owner_cwd": owner_cwd,
        "expected_session": expected_session,
    }));
}

/// The caller as the grant check reads it.
fn caller_payload(calling: &Calling) -> Value {
    match calling {
        Calling::Human => json!({"kind": "human"}),
        Calling::Unreadable(_) => json!({"kind": "unreadable"}),
        Calling::Unregistered(_) => json!({"kind": "unregistered"}),
        Calling::Agent(row, _) => json!({
            "kind": "agent", "status": status_of(row), "role_scope": row.role_scope,
        }),
    }
}

fn grant_refusal(
    calling: &Calling,
    scope: &str,
    projects: &HashMap<String, String>,
    entries: &[Value],
) -> Option<String> {
    let stray = match calling {
        Calling::Unreadable(None) => std::env::var("FNO_AGENT_SELF")
            .unwrap_or_default()
            .trim()
            .to_string(),
        _ => String::new(),
    };
    match crate::territory::role_grant_kind(&json!({
        "op": "check", "requested": scope, "caller": caller_payload(calling),
        "allow_terminal_recovery": false, "allow_succession": true,
        "stray_agent_self": stray, "projects": projects, "entries": entries,
    })) {
        Ok(answer) => answer["refusal"].as_str().map(str::to_string),
        Err(e) => Some(format!(
            "cannot verify the grantor's authority: the grant check did not answer ({e})"
        )),
    }
}

/// `team-widen`'s answer for a self-edit, or `{}` when it cannot answer.
fn widen_answer(scope: &str, caller: &RegistryEntry, target: &str, entries: &[Value]) -> Value {
    let wanted: Vec<String> = scope
        .split(',')
        .chain(caller.role_scope.as_deref().unwrap_or("").split(','))
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    let members: Vec<Value> = wanted
        .into_iter()
        .filter(|m| seen.insert(m.clone()))
        .map(|m| {
            entries
                .iter()
                .find(|e| e["id"].as_str() == Some(m.as_str()))
                .cloned()
                .unwrap_or(Value::Null)
        })
        .collect();
    crate::team_widen::resolve(&json!({
        "requested": scope, "target": target, "members": members,
        "caller": {
            "name": caller.name, "status": status_of(caller), "role_scope": caller.role_scope,
            "role_grantor": caller.role_grantor, "harness_session_id": caller.harness_session_id,
            "cc_session_id": caller.cc_session_id, "pid": caller.pid,
        },
    }))
    .unwrap_or_else(|_| json!({}))
}

/// Grant a role to one existing row from an attended human shell, or from an
/// agent whose own role strictly contains the requested scope. A row that
/// already holds a role is re-scoped in the same registry write, and the
/// receipt names what was vacated.
pub fn promote(home: &AgentsHome, handle: &str, scopes: &[String]) -> Result<Value, String> {
    let projects = projects();
    let entries = graph_entries();
    let resolved = crate::territory::resolve_role_kind(&json!({
        "scopes": scopes, "projects": projects, "entries": entries,
    }))?;
    if let Some(refusal) = resolved["refusal"].as_str() {
        return Err(refusal.to_string());
    }
    let level = resolved["level"].as_u64().unwrap_or(0) as u32;
    let scope = resolved["scope"].as_str().unwrap_or("").to_string();

    let calling = calling_row(home);
    let resolved_target = resolve_target(home, handle)
        .map_err(|e| format!("{e}. `fno agents list` shows every handle you can role."))?;
    let target_name = resolved_target.name.clone();
    let caller = match &calling {
        Calling::Agent(row, _) => Some(RegistryEntry::clone(row)),
        _ => None,
    };
    let denial = grant_refusal(&calling, &scope, &projects, &entries);
    let widen = match &caller {
        Some(c) => widen_answer(&scope, c, &target_name, &entries),
        None => json!({}),
    };
    let widened = widen["widen"] == true;
    let self_edit = caller.as_ref().is_some_and(|c| c.name == target_name);
    if !widened && (denial.is_some() || self_edit) {
        let hint = widen["hint"].as_str().filter(|h| !h.is_empty());
        let text: Vec<&str> = [denial.as_deref(), hint].into_iter().flatten().collect();
        return Err(if text.is_empty() {
            "role self-edit refused: the role-widen answer was unavailable".to_string()
        } else {
            text.join(" ")
        });
    }
    if widened && self_edit && widen["grantor"].as_str().is_none_or(str::is_empty) {
        return Err("role self-edit admitted with no grantor in the answer; update the fno-agents binary (`fno doctor update --rust`)".to_string());
    }
    let grantor_name = caller
        .as_ref()
        .map_or("human".to_string(), |c| c.name.clone());
    let recorded_grantor = widen["grantor"]
        .as_str()
        .filter(|g| !g.is_empty())
        .map_or(grantor_name.clone(), str::to_string);
    let same =
        |a: Option<&str>, b: &str| crate::loop_lead::same_territory(a.unwrap_or(""), b, &projects);
    if let Some(c) = &caller {
        if same(c.role_scope.as_deref(), &scope) {
            return Err(format!(
                "refusing to role {}: {} is your OWN scope, and this command only stamps the \
                 target, so it cannot hand a role over. Succession runs through `fno agents spawn \
                 --promote <scope> --hand-off` instead, which vacates you and stamps the new \
                 holder in a single registry write, so the scope is never doubly ruled and never \
                 briefly unruled.",
                py_repr_str(handle),
                py_repr_str(&scope)
            ));
        }
    }
    let granting_scope = caller.as_ref().and_then(|c| c.role_scope.clone());

    let mut receipt = Map::new();
    let mut vacated_owner = (String::new(), String::new());
    let rows_after = crate::state::update_registry(&home.registry_json(), |reg| {
        if let Some(c) = &caller {
            let live = locked(&mut reg.entries, c).map(|r| r.clone());
            if let Some(live) = &live {
                if terminal(live) {
                    return Err(format!(
                        "refusing to role {}: the grantor's STORED status is {}, which is \
                         terminal, so authority ended before the registry write. Re-register the \
                         grantor in its live session or use an attended shell.",
                        py_repr_str(&target_name),
                        py_repr_str(&status_of(live))
                    ));
                }
            }
            let live_scope = live.as_ref().and_then(|r| r.role_scope.clone());
            let held = granting_scope.as_deref().unwrap_or("");
            if !crate::loop_lead::same_territory(live_scope.as_deref().unwrap_or(""), held, &projects) {
                return Err(format!(
                    "refusing to role {}: this session's own role moved from {} to {} while the \
                     grant was in flight, so the authority it was checked against no longer holds. \
                     Re-read your role with `fno agents team`, then retry if it still contains the \
                     scope.",
                    py_repr_str(&target_name),
                    granting_scope.as_deref().map_or("None".into(), py_repr_str),
                    live_scope.as_deref().map_or("None".into(), py_repr_str)
                ));
            }
        }
        let Some(target) = locked(&mut reg.entries, &resolved_target).map(|r| r.clone()) else {
            return Err(format!(
                "no agent matching {}; the target disappeared before the grant committed. `fno \
                 agents list` shows the handles you can role.",
                py_repr_str(handle)
            ));
        };
        if terminal(&target) {
            return Err(format!(
                "refusing to role {}: its STORED status is {}, which is terminal. That is a \
                 recorded snapshot, not a live probe, so it can be stale for a session that is \
                 still running. Two ways out:\n  target is alive    run `fno agents register` IN \
                 the target session (it restamps the row idle in place), then retry\n  target \
                 really died `fno agents reconcile`, then role a live row instead; `fno agents \
                 list` shows stored status beside the computed live_status, and a disagreement \
                 means the stored one is stale",
                py_repr_str(&target.name),
                py_repr_str(&status_of(&target))
            ));
        }
        let (vacated_scope, vacated_level) = (target.role_scope.clone(), target.role_level);
        vacated_owner = (holder_session(&target).unwrap_or_default(), target.cwd.clone());
        if vacated_level.is_none() != vacated_scope.is_none() {
            return Err(format!(
                "refusing to role {}: it holds half a role (level={}, scope={}), which no legal \
                 role produces. fno agents stop the row and fno agents rm it, then role the \
                 re-registered session.",
                py_repr_str(&target.name),
                vacated_level.map_or("None".into(), |l| l.to_string()),
                vacated_scope.as_deref().map_or("None".into(), py_repr_str)
            ));
        }
        // An agent grantor's own row legitimately overlaps the delegated
        // scope; every other RIVAL live row is a second ruler.
        let holder = reg.entries.iter().find(|r| {
            r.name != target.name
                && (caller.is_none() || r.name != grantor_name)
                && !terminal(r)
                && crate::loop_lead::team_rivals_pub(
                    r.role_scope.as_deref().unwrap_or(""),
                    r.role_level,
                    &scope,
                    Some(level),
                    &projects,
                )
        });
        if let Some(holder) = holder {
            return Err(format!(
                "refusing to role {}: scope {} is already held by live row {} (holding {}). \
                 Three ways out, cheapest first:\n  re-scope the holder   fno agents org promote \
                 {} --scope <other territory>   (both sessions stay live; retry this command \
                 after)\n  holder looks dead     fno agents reconcile   (a row whose harness \
                 session is gone flips to orphaned, which frees the scope)\n  end the holder        \
                 fno agents stop {}, then retry",
                py_repr_str(&target.name),
                py_repr_str(&scope),
                py_repr_str(&holder.name),
                holder.role_scope.as_deref().map_or("None".into(), py_repr_str),
                holder.name,
                holder.name
            ));
        }
        let manifest = arm_manifest(&target, &scope, level, &recorded_grantor).map_err(|e| {
            format!(
                "refusing to role {}: lead manifest arming failed: {e}",
                py_repr_str(&target.name)
            )
        })?;
        if let Some(row) = reg.entries.iter_mut().find(|r| r.name == target.name) {
            row.role_level = Some(level);
            row.role_scope = Some(scope.clone());
            row.role_grantor = Some(recorded_grantor.clone());
        }
        receipt.insert("promoted".into(), target.name.clone().into());
        receipt.insert("level".into(), level.into());
        receipt.insert("scope".into(), scope.clone().into());
        receipt.insert("grantor".into(), recorded_grantor.clone().into());
        receipt.insert("vacated_scope".into(), json!(vacated_scope));
        receipt.insert("vacated_level".into(), json!(vacated_level));
        receipt.insert("lead_loop_armed".into(), manifest.is_some().into());
        Ok(reg.entries.clone())
    })
    .map_err(|e| format!("the registry write failed: {e}"))??;
    if let Some(session) = resolved_target.harness_session_id.as_deref() {
        crate::retired_crown::clear(home, session);
    }

    // Clear the vacated manifest BEFORE arming missions: a role killed between
    // the commit and a slow arm would otherwise leave a phantom role on disk.
    if let Some(vacated) = receipt["vacated_scope"].as_str() {
        if !crate::loop_lead::same_territory(vacated, &scope, &projects) {
            remove_manifest(vacated, &vacated_owner.1, &vacated_owner.0);
        }
    }
    let row_after = rows_after.iter().find(|r| r.name == target_name);
    let vacated = receipt["vacated_scope"].as_str().map(str::to_string);
    receipt.insert(
        "team_name".into(),
        carry_team_name(vacated.as_deref(), &scope, row_after, level).into(),
    );
    receipt.insert("missions_armed".into(), arm_missions(home, &scope));
    receipt.insert(
        "stranded_subordinates".into(),
        stranded(
            vacated.as_deref(),
            &scope,
            &target_name,
            &rows_after,
            &projects,
        ),
    );
    let delivery = if self_edit {
        "skipped: self-edit, this session already terms".to_string()
    } else {
        let harness = row_after
            .and_then(|r| r.harness.clone())
            .unwrap_or_default();
        let command = crate::backlog::dispatch_resolve::normalize_command(
            &format!("/fno:lead {scope}"),
            &harness,
        )
        .unwrap_or_else(|_| format!("/fno:lead {scope}"));
        let address = row_after
            .and_then(holder_session)
            .unwrap_or_else(|| target_name.clone());
        send_term_command(&address, &command)
    };
    receipt.insert("term_delivery".into(), delivery.into());
    Ok(Value::Object(receipt))
}

/// The team name follows the role: a recorded name moves to the landing
/// scope, an unnamed team takes the row's people-shaped name. Advisory: a
/// refusal is the receipt line naming what did not happen.
fn carry_team_name(
    vacated: Option<&str>,
    scope: &str,
    row_after: Option<&RegistryEntry>,
    level: u32,
) -> String {
    let Some(row) = row_after else {
        return "unchanged".to_string();
    };
    match crate::team_rescope::resolve(&json!({
        "kind": "team-rescope", "old_scope": vacated.unwrap_or(""), "new_scope": scope,
        "candidate": row.name, "holder_session": holder_session(row).unwrap_or_default(),
        "level": level,
    })) {
        Err(e) => format!("unavailable: {e}"),
        Ok(answer) => match answer["named"].as_str() {
            Some(named) if !named.is_empty() => named.to_string(),
            _ if answer["carried"] == true => "carried".to_string(),
            _ => answer["reason"]
                .as_str()
                .filter(|r| !r.is_empty())
                .unwrap_or("unchanged")
                .to_string(),
        },
    }
}

/// Mission-arm every open epic container in the scope: the ids newly armed,
/// or null when the graph write failed.
fn arm_missions(home: &AgentsHome, scope: &str) -> Value {
    let answer = crate::team_spawn::resolve(&json!({
        "op": "missions", "scope": scope,
        "events": crate::client_commands::trace_events_path(home),
        "registry": home.registry_json(),
    }));
    match answer {
        Ok(answer) => {
            for note in answer["notices"].as_array().into_iter().flatten() {
                if let Some(n) = note.as_str() {
                    eprintln!("{n}");
                }
            }
            answer["armed"].clone()
        }
        Err(e) => {
            eprintln!(
                "role: WARNING: mission arming failed for scope {} ({e}); arm it with: fno \
                 backlog advance --epic <id>",
                py_repr_str(scope)
            );
            Value::Null
        }
    }
}

/// Live rows whose role the vacated scope contained and the new one does
/// not: `[]` when no territory left, null when the graph could not be read.
fn stranded(
    vacated: Option<&str>,
    scope: &str,
    target: &str,
    rows: &[RegistryEntry],
    projects: &HashMap<String, String>,
) -> Value {
    let cwd = std::env::current_dir().unwrap_or_default();
    let entries = crate::territory::graph_entries(&cwd).ok();
    let payload_rows: Vec<Value> = rows
        .iter()
        .map(|r| json!({"name": r.name, "status": status_of(r), "role_scope": r.role_scope}))
        .collect();
    crate::territory::role_grant_kind(&json!({
        "op": "stranded", "vacated": vacated, "new_scope": scope, "target_name": target,
        "rows": payload_rows, "projects": projects, "entries": entries,
    }))
    .map(|a| a["stranded"].clone())
    .unwrap_or(Value::Null)
}

/// Mail the term command to the freshly promoted holder; never fails. `--raw`
/// types the payload as the operator would, so a slash arrives as a command.
/// It runs this binary's own mail leaf, never the `fno` front.
fn send_term_command(address: &str, command: &str) -> String {
    let out = std::process::Command::new(crate::mail_cli::send_raw::agents_binary())
        .args(["mail", "send", address, command, "--raw"])
        .stdin(std::process::Stdio::null())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => return format!("not delivered ({e})"),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !stdout.is_empty() {
        return stdout.lines().last().unwrap_or("").to_string();
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let detail = [stderr.as_str(), stdout.as_str()]
        .into_iter()
        .find(|s| !s.is_empty())
        .and_then(|s| s.lines().last())
        .unwrap_or("no output")
        .to_string();
    format!(
        "not delivered (rc={}: {detail})",
        out.status.code().unwrap_or(-1)
    )
}

/// Return a transferred role to its recorded grantor: the current holder is
/// cleared and the live row named by `role_grantor` receives the same
/// territory and level in one locked write.
pub fn reclaim_role(home: &AgentsHome, handle: Option<&str>) -> Result<Value, String> {
    let calling = calling_row(home);
    let holder = match (handle, &calling) {
        // The handle form is the attended operator's reach, and only that.
        (Some(_), c) if !matches!(c, Calling::Human) => {
            return Err(
                "reclaim by handle is an attended-shell action: run it without --handle \
                        inside the successor session, or from a shell holding no agent identity"
                    .to_string(),
            )
        }
        (Some(h), _) => resolve_target(home, h)?,
        (None, Calling::Agent(row, _)) => RegistryEntry::clone(row),
        (None, _) => {
            return Err(
                "reclaim needs the current promoted holder: run it inside the successor \
                        session or pass that registered handle from an attended shell"
                    .to_string(),
            )
        }
    };
    let scope = holder.role_scope.clone().filter(|s| !s.trim().is_empty());
    let (Some(level), Some(scope)) = (holder.role_level, scope) else {
        return Err("this session holds no role to reclaim".to_string());
    };
    let grantor_key = holder.role_grantor.clone().unwrap_or_default();
    if grantor_key.is_empty() || grantor_key == "human" {
        return Err(format!(
            "role over {} has no registry grantor to reclaim; a human grant cannot be returned \
             automatically",
            py_repr_str(&scope)
        ));
    }
    let projects = projects();
    let holder_name = holder.name.clone();
    let holder_key = holder_session(&holder).unwrap_or_else(|| holder_name.clone());
    let mut reclaimed_session: Option<String> = None;
    let receipt = crate::state::update_registry(&home.registry_json(), |reg| {
        let live = locked(&mut reg.entries, &holder).map(|r| r.clone());
        let Some(live) = live.filter(|r| !terminal(r)) else {
            return Err(format!(
                "cannot reclaim {}: current holder {} is no longer live",
                py_repr_str(&scope),
                py_repr_str(&holder_name)
            ));
        };
        if live.role_scope.as_deref() != Some(scope.as_str()) || live.role_level != Some(level) {
            return Err(format!(
                "cannot reclaim {}: holder {} changed its role while reclaim was in flight",
                py_repr_str(&scope),
                py_repr_str(&holder_name)
            ));
        }
        let target = reg
            .entries
            .iter()
            .find(|r| {
                r.name != holder_name
                    && [
                        Some(r.name.as_str()),
                        r.harness_session_id.as_deref(),
                        r.cc_session_id.as_deref(),
                        Some(r.short_id.as_str()),
                    ]
                    .contains(&Some(grantor_key.as_str()))
            })
            .cloned();
        let Some(target) = target else {
            return Err(format!(
                "cannot reclaim {}: recorded grantor {} has no registry row; leave the role in \
                 place and re-register the grantor before retrying",
                py_repr_str(&scope),
                py_repr_str(&grantor_key)
            ));
        };
        if terminal(&target) {
            return Err(format!(
                "cannot reclaim {}: recorded grantor {} is terminal ({}); a role cannot return \
                 to a dead row",
                py_repr_str(&scope),
                py_repr_str(&target.name),
                status_of(&target)
            ));
        }
        if target.role_scope.is_some() || target.role_level.is_some() {
            return Err(format!(
                "cannot reclaim {}: grantor {} already holds a role; refusing to replace \
                 unrelated authority",
                py_repr_str(&scope),
                py_repr_str(&target.name)
            ));
        }
        let other = reg.entries.iter().find(|r| {
            r.name != holder_name
                && r.name != target.name
                && !terminal(r)
                && crate::loop_lead::team_rivals_pub(
                    r.role_scope.as_deref().unwrap_or(""),
                    r.role_level,
                    &scope,
                    Some(level),
                    &projects,
                )
        });
        if let Some(other) = other {
            return Err(format!(
                "cannot reclaim {}: live row {} already holds overlapping territory ({})",
                py_repr_str(&scope),
                py_repr_str(&other.name),
                other
                    .role_scope
                    .as_deref()
                    .map_or("None".into(), py_repr_str)
            ));
        }
        let returned_by = holder_session(&live).unwrap_or_else(|| live.name.clone());
        let armed = arm_manifest(&target, &scope, level, &returned_by);
        reclaimed_session = target.harness_session_id.clone();
        for row in reg.entries.iter_mut() {
            if row.name == holder_name {
                row.role_level = None;
                row.role_scope = None;
                row.role_grantor = None;
            } else if row.name == target.name {
                row.role_level = Some(level);
                row.role_scope = Some(scope.clone());
                row.role_grantor = Some(returned_by.clone());
            }
        }
        let mut receipt = json!({
            "reclaimed": target.name, "from_holder": holder_name, "level": level,
            "scope": scope, "grantor": returned_by,
            "lead_loop_armed": matches!(armed, Ok(Some(_))),
        });
        if let Err(reason) = armed {
            receipt["lead_loop_unarmed_reason"] = reason.into();
        }
        Ok(receipt)
    })
    .map_err(|e| format!("the registry write failed: {e}"))??;
    if let Some(session) = reclaimed_session.as_deref() {
        crate::retired_crown::clear(home, session);
    }
    remove_manifest(&scope, &holder.cwd, &holder_key);
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str) -> RegistryEntry {
        RegistryEntry {
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn every_role_effect_keys_on_one_session_id() {
        // A fallback-id row (no harness session yet) keys on its short id for
        // the arm, the carry and the lock-time match alike.
        let mut r = row("candor");
        r.short_id = "c4nd".into();
        assert_eq!(holder_session(&r).as_deref(), Some("c4nd"));
        r.cc_session_id = Some("cc-1".into());
        assert_eq!(holder_session(&r).as_deref(), Some("cc-1"));
        r.harness_session_id = Some("hs-1".into());
        assert_eq!(holder_session(&r).as_deref(), Some("hs-1"));
    }

    #[test]
    fn a_rebound_name_is_never_the_locked_row() {
        let mut expect = row("candor");
        expect.harness_session_id = Some("old".into());
        let mut rebound = row("candor");
        rebound.harness_session_id = Some("new".into());
        let mut rows = vec![rebound];
        assert!(locked(&mut rows, &expect).is_none());
        rows[0].harness_session_id = Some("old".into());
        assert!(locked(&mut rows, &expect).is_some());
    }
}
