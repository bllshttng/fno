//! The client's promote door. A `--promote` spawn resolves its team, checks
//! the grantor's authority, plans occupancy and seeds the lead turn before
//! launch, on every lane, through the same kinds the Python door calls:
//! `role-resolve`, `role-grant`, `role-settle` and `spawn-team`. After a
//! launch, the `spawn-team` settle op stamps the team under the registry
//! lock. The door decides nothing new.

use crate::claims::CanonicalDisposition;
use crate::paths::AgentsHome;
use crate::spawn_context::OwnedDisposition;
use serde_json::{json, Value};

/// The Python door's one-shot refusal (cli.py), byte for byte.
pub const ONE_SHOT_REFUSAL: &str = "--promote needs a session that outlives the grant; \
     headless is a one-shot that exits after one answer, so its role would be orphaned \
     at birth. Use --as pane or --as thread.";

/// A planned promotion: the settle payload and the gate's succession facts.
#[derive(Debug, Clone)]
pub struct Promotion {
    ask: Value,
    scope: String,
    hand_off: bool,
    caller_session: Option<String>,
}

/// The calling session as the grant check reads it, and its session id.
/// The kinds follow Python `calling_agent_row`: an identity fno cannot place
/// is `unreadable`, an identity with no row is `unregistered`, no identity
/// at all is an attended `human`.
fn caller(home: &AgentsHome) -> (Value, Option<String>) {
    let get = |k: &str| std::env::var(k).ok();
    let ident = crate::spawn_context::resolve_self_identity(&get, None, None, home);
    let stamp = crate::spawn_context::parse_canonical_stamp(&get).2;
    let session = ident.session_id.clone().filter(|s| !s.is_empty());
    let placed = !matches!(
        ident.disposition,
        OwnedDisposition::Ambiguous | OwnedDisposition::Invalid | OwnedDisposition::Contradiction
    ) && !(session.is_none()
        && matches!(
            stamp,
            CanonicalDisposition::NameOnly | CanonicalDisposition::Complete
        ));
    if !placed {
        return (json!({"kind": "unreadable"}), session);
    }
    let (Some(sid), Some(harness)) = (session.clone(), ident.harness.clone()) else {
        let stray = get("FNO_AGENT_SELF").is_some_and(|v| !v.trim().is_empty());
        return (
            json!({"kind": if stray { "unreadable" } else { "human" }}),
            None,
        );
    };
    match crate::state::load_registry(&home.registry_json()) {
        Err(_) => (json!({"kind": "unreadable"}), Some(sid)),
        Ok(registry) => match registry.find_by_session(&harness, &sid) {
            None => (json!({"kind": "unregistered"}), Some(sid)),
            Some(row) => {
                let row = serde_json::to_value(row).unwrap_or(Value::Null);
                let mut out = json!({"kind": "agent"});
                for key in ["name", "status", "role_scope"] {
                    out[key] = row[key].clone();
                }
                (out, Some(sid))
            }
        },
    }
}

fn refuse(text: &str) -> i32 {
    eprintln!("--promote: {text}");
    2
}

/// Plan a `--promote` spawn before launch. Rewrites `params` (the seeded
/// message, the drawn name, the role the codex lane stamps at mint) and
/// `name`; a refusal prints the Python door's text and answers exit 2.
pub fn plan(
    params: &mut Value,
    name: &mut String,
    substrate: &str,
) -> Result<Option<Promotion>, i32> {
    let scopes: Vec<String> = params["promote"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if scopes.is_empty() {
        return Ok(None);
    }
    if matches!(substrate, "headless" | "cloud") {
        eprintln!("{ONE_SHOT_REFUSAL}");
        return Err(2);
    }
    let resolved =
        crate::territory::resolve_role_kind(&json!({"scopes": scopes})).map_err(|e| refuse(&e))?;
    if let Some(refusal) = resolved["refusal"].as_str() {
        return Err(refuse(refusal));
    }
    let (level, scope) = (
        resolved["level"].clone(),
        resolved["scope"].as_str().unwrap_or("").to_string(),
    );
    let hand_off = params["hand_off"] == true;
    let home = AgentsHome::from_env();
    let (caller, caller_session) = caller(&home);
    let stray = if caller["kind"] == "unreadable" && caller_session.is_none() {
        std::env::var("FNO_AGENT_SELF")
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        String::new()
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let check = crate::territory::role_grant_kind(&json!({
        "op": "check", "requested": scope, "caller": caller,
        "allow_terminal_recovery": true, "allow_succession": hand_off, "stray_agent_self": stray,
        "projects": crate::org_board::project_map(&cwd).unwrap_or_default(),
        "entries": crate::territory::graph_entries(&cwd).unwrap_or_default(),
    }))
    .map_err(|e| {
        refuse(&format!(
            "cannot verify the grantor's authority: the grant check did not answer ({e})"
        ))
    })?;
    if let Some(refusal) = check["refusal"].as_str() {
        return Err(refuse(refusal));
    }
    let rows = crate::state::load_registry(&home.registry_json()).map_err(|e| {
        refuse(&format!(
            "cannot decide role occupancy: the registry could not be read ({e})"
        ))
    })?;
    let redraw = name.trim().eq_ignore_ascii_case("redraw");
    let caller_name = caller["name"].as_str().map(str::to_string);
    let mut settle = json!({
        "kind": "role-settle", "scope": scope, "succession": hand_off,
        "proposed_name": if redraw { Value::Null } else { json!(name) }, "redraw": redraw,
        "caller": match &caller_name { Some(n) => json!({"kind": "agent", "name": n}), None => json!({"kind": "human"}) },
        "exclude_name": Value::Null,
        "rows": serde_json::to_value(&rows.entries).unwrap_or_default(),
    });
    if let Some(reason) = params["take_over"].as_str() {
        settle["take_over"] = json!({"reason": reason});
    }
    let occupancy = crate::team_settle::resolve(&settle)
        .map_err(|e| refuse(&format!("cannot decide role occupancy: {e}")))?;
    if let Some(refusal) = occupancy["refusal"].as_str() {
        return Err(refuse(refusal));
    }
    if hand_off {
        if let Some(drawn) = occupancy["draw"]["name"].as_str().filter(|n| !n.is_empty()) {
            *name = drawn.to_string();
            params["name"] = json!(drawn);
        }
    }
    let harness = params["provider"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            crate::provider::infer_dispatch_provider(|k| std::env::var(k).ok()).to_string()
        });
    let seeded = crate::team_spawn::resolve(&json!({
        "op": "seed", "message": params["message"].as_str().unwrap_or(""),
        "level": level, "scope": scope, "revive": false, "harness": harness,
    }))
    .map_err(|e| refuse(&e))?;
    params["message"] = seeded["message"].clone();
    params["role_level"] = level.clone();
    params["role_scope"] = json!(scope);
    Ok(Some(Promotion {
        ask: json!({"plan": occupancy, "caller": caller_name, "level": level, "scope": scope,
            "typed": seeded["typed"], "harness": harness}),
        scope,
        hand_off,
        caller_session,
    }))
}

/// The gate input a promoted spawn carries: the caller session, and the
/// succession scope when it hands its team off.
pub fn gate_input(promotion: Option<&Promotion>) -> crate::spawn_gate::GateInput {
    crate::spawn_gate::GateInput {
        caller_session: promotion.and_then(|p| p.caller_session.clone()),
        succession_scope: promotion.filter(|p| p.hand_off).map(|p| p.scope.clone()),
        ..Default::default()
    }
}

/// Settle a launched promoted spawn with the `spawn-team` settle op, against
/// the row the lane wrote. A failed launch settles nothing. A failure after
/// launch prints the remedy and keeps the exit: the worker runs.
pub fn settle(promotion: Option<&Promotion>, exit: i32, name: &str) -> i32 {
    let Some(promotion) = promotion.filter(|_| exit == 0) else {
        return exit;
    };
    let home = AgentsHome::from_env();
    let row = crate::state::load_registry(&home.registry_json())
        .ok()
        .and_then(|r| r.find(name).cloned());
    let mut ask = promotion.ask.clone();
    ask["op"] = json!("settle");
    ask["name"] = json!(name);
    if let Some(row) = &row {
        let session = [
            row.harness_session_id.as_deref(),
            row.cc_session_id.as_deref(),
            Some(row.short_id.as_str()),
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .unwrap_or("");
        ask["session_id"] = json!(session);
        ask["cwd"] = json!(row.cwd);
        ask["grantor"] = json!(row.spawned_by_session.as_deref().unwrap_or("human"));
    }
    let notices = match row.map(|_| crate::team_spawn::resolve(&ask)) {
        Some(Ok(answer)) => answer["notices"].as_array().cloned().unwrap_or_default(),
        failed => vec![json!(format!(
            "spawn: promotion settlement failed after launch: {}; {} runs without a confirmed \
             role. Grant it with fno agents org promote.",
            match failed {
                Some(Err(e)) => e,
                _ => "no registry row was found".to_string(),
            },
            crate::claude_ask::py_repr(name)
        ))],
    };
    for note in notices.iter().filter_map(Value::as_str) {
        eprintln!("{note}");
    }
    exit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_shot_promote_refuses_before_any_read() {
        for substrate in ["headless", "cloud"] {
            let mut params = json!({"promote": ["x-1"]});
            let mut name = "w".to_string();
            assert_eq!(plan(&mut params, &mut name, substrate).unwrap_err(), 2);
        }
    }

    #[test]
    fn only_a_hand_off_carries_the_succession_scope() {
        let mut p = Promotion {
            ask: json!({}),
            scope: "x-1".into(),
            hand_off: false,
            caller_session: Some("s".into()),
        };
        assert_eq!(gate_input(Some(&p)).succession_scope, None);
        assert_eq!(gate_input(Some(&p)).caller_session.as_deref(), Some("s"));
        p.hand_off = true;
        assert_eq!(
            gate_input(Some(&p)).succession_scope.as_deref(),
            Some("x-1")
        );
        assert_eq!(gate_input(None).caller_session, None);
    }
}
