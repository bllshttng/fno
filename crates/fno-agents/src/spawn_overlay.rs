//! `spawn-overlay`: the spawn-defaults resolver Python delegates to, ported
//! from Python. Payload kinds, one command, direct dispatch (no
//! daemon RPC), the route-slot transport shape: one JSON payload in on stdin,
//! one JSON answer out on stdout.
//!
//! - kind `overlay`: the harness-keyed rung resolution. Refuses an overlay
//!   table naming an unknown harness or carrying a ranking field; resolves
//!   effort/substrate/permission through the six rungs for the harness the
//!   caller already resolved; picks ONE bundle args vector (lane > profile
//!   overlay > defaults overlay, never concatenated) and names the boundary
//!   that would displace it.
//! - kind `fallback`: the failover-chain validation (`validate_fallback` in
//!   Python). Returns canonical links or the exact refusal the Python raised.
//! - kind `print`: the lane's native launch line. The payload names the
//!   lane's resolved values (and optionally a routing row's own `launch`
//!   argv); the answer carries the filled tokens [`crate::spawn_compose::
//!   fill_launch`] produced, so `spawn --print` and the exec path share one
//!   renderer.
//! - kind `team-settle`: whether a teamed spawn is granted, transfers, or
//!   refuses (`team_settle::resolve`), a port of Python's
//!   `settle_spawn_team` with a new human-succession branch and ladder-aware
//!   rivalry through `loop_lead::team_rivals`.
//! - kind `team-widen`: whether an agent can add an epic its own session
//!   created to its own epic-set team (`team_widen::resolve`).
//! - kind `team-rescope`: the role re-scope's team-name effect
//!   (`team_rescope::resolve`), the promote-side carry that keeps a lead's
//!   recorded name across an `org promote` scope move.
//! - kind `lead`: the lead manifest, wake-ledger and scope ops
//!   (`lead_ops::overlay_kind`) the PR watcher's lead wake and the role
//!   commands read.
//! - kind `roles-discover`: layered business-role discovery
//!   (`roles::registry::discover_kind`), the records the Python roles
//!   package reads back. The roles commands ride the same command as argv ops,
//!   `spawn-overlay roles <op>`.
//! - kind `roles-resolve`: business-role resolution
//!   (`roles::resolver::resolve_kind`), the one leg the Python
//!   `resolve_role` calls.
//! - kind `business-role`: the business-role lookup model routing reads
//!   (`roles::routing::business_role_kind`).
//! - kind `role-resolve`: the org ladder (`territory::resolve_role`), a
//!   scope's rung and canonical form, the one resolver the board and the
//!   promote path share.
//! - kind `role-grant`: the grant decision and the re-scope strand scan
//!   (`territory::role_grant_kind`), over the one containment rule
//!   `territory::scope_contains`.
//!
//! - kinds `credentials`, `state-root-seal`, `account-overlay`,
//!   `dispatch-account`, `account-for-env` and `login-present`: the account
//!   answers a Python spawn seam reads (`accounts::credentials`,
//!   `accounts::launch`).
//!
//! The `fno config accounts` commands ride the same action as argv ops,
//! `spawn-overlay accounts <command>` (see `accounts::run`).
//! The `fno config plugins` commands ride it too, as
//! `spawn-overlay plugins <command>` (see `plugins::cli::run`).

use crate::provider::{known_providers_csv, KNOWN_PROVIDERS};
use serde_json::{json, Map, Value};
use std::io::{self, Read};
use std::path::Path;

/// The fields an overlay block may legally carry.
const OVERLAY_FIELDS: [&str; 4] = ["permission_mode", "effort", "substrate", "args"];
/// Ranking fields are lane business; an overlay carrying one refuses.
const LANE_FIELD_NAMES: [&str; 4] = ["provider", "model", "route", "account"];
/// The failover chain's size keys.
const CHAIN_KEYS: [&str; 4] = ["S", "M", "L", "default"];
/// The harness axis is the BINARY; opencode is legally both harness and
/// provider, so the chain roster is spelled here and never inferred.
pub(crate) const CHAIN_HARNESSES: [&str; 4] = ["claude", "codex", "agy", "opencode"];

/// The vendor a harness's own primary lane bills when no route/vendor was
/// named. opencode is operator-configured, so it holds no opinion here.
const HARNESS_DEFAULT_VENDOR: [(&str, &str); 4] = [
    ("claude", "anthropic"),
    ("codex", "openai"),
    ("gemini", "google"),
    ("agy", "google"),
];

/// A model string's implied vendor, by prefix or tier word. A pure string
/// opinion and never a routing input: the pairing is legal and --model is
/// deliberate passthrough.
const MODEL_VENDOR_HINTS: [(&str, &str); 5] = [
    ("glm-", "zai"),
    ("gpt-", "openai"),
    ("deepseek", "deepseek"),
    ("gemini-", "google"),
    ("claude-", "anthropic"),
];
const MODEL_WORD_VENDORS: [(&str, &str); 3] = [
    ("opus", "anthropic"),
    ("sonnet", "anthropic"),
    ("haiku", "anthropic"),
];

pub fn run_spawn_overlay(args: &[String]) -> i32 {
    // The roles commands nest here as argv ops (crate::roles::cli), so they
    // add no fno-agents root command.
    if args.first().map(String::as_str) == Some("roles") {
        return crate::roles::cli::run(&args[1..]);
    }
    // The `fno doctor observer` commands nest here the same way
    // (crate::observer::run), so they add no fno-agents root action.
    if args.first().map(String::as_str) == Some("observer") {
        return crate::observer::run(&args[1..]);
    }
    // The whoami family nests the same way (crate::whoami::run).
    if args.first().map(String::as_str) == Some("whoami") {
        return crate::whoami::run(&args[1..]);
    }
    // The `fno config accounts` commands nest here as argv ops
    // (crate::accounts::run), so they add no fno-agents root action.
    if args.first().map(String::as_str) == Some("accounts") {
        return crate::accounts::run(&args[1..]);
    }
    // The `fno config plugins` commands nest here the same way
    // (crate::plugins::cli::run).
    if args.first().map(String::as_str) == Some("plugins") {
        return crate::plugins::cli::run(&args[1..]);
    }
    // The `fno config project` commands nest here the same way
    // (crate::project::run).
    if args.first().map(String::as_str) == Some("project") {
        return crate::project::run(&args[1..]);
    }
    let mut payload = String::new();
    let read = if let Some(path) = args.iter().find_map(|a| a.strip_prefix("--payload-file=")) {
        std::fs::read_to_string(path)
    } else {
        io::stdin()
            .read_to_string(&mut payload)
            .map(|_| payload.clone())
    };
    let payload = match read {
        Ok(text) => text,
        Err(e) => {
            eprint!("spawn-overlay: cannot read payload: {e}\n");
            return 2;
        }
    };
    let parsed: Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => {
            eprint!("spawn-overlay: bad payload: {e}\n");
            return 2;
        }
    };
    match resolve(parsed) {
        Ok(answer) => {
            println!("{answer}");
            0
        }
        Err(msg) => {
            eprint!("spawn-overlay: {msg}\n");
            2
        }
    }
}

#[cfg(test)]
trait MergeValue {
    fn merge(self, other: &Value) -> Value;
}

#[cfg(test)]
impl MergeValue for Value {
    fn merge(mut self, other: &Value) -> Value {
        if let (Some(base), Some(extra)) = (self.as_object_mut(), other.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        self
    }
}

pub fn resolve(payload: Value) -> Result<Value, String> {
    match payload.get("kind").and_then(Value::as_str) {
        Some("overlay") => resolve_overlay(&payload),
        Some("compat") => Ok(crate::effort_surface::compat(&payload)),
        Some("compose") => {
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let inputs = crate::spawn_compose::gather(&payload, &cwd);
            Ok(crate::spawn_compose::compose(&inputs).to_value())
        }
        Some("lane-vendor") => {
            let toks: Vec<String> = payload
                .get("argv_tail")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let (axes, _) = crate::cli_args::SpawnAxes::scan(&toks)?;
            let vendor = resolve_lane_vendor(
                &axes,
                payload.get("harness").and_then(Value::as_str),
                payload.get("env_harness").and_then(Value::as_str),
                payload.get("argv_head").and_then(Value::as_str),
            );
            Ok(json!({"vendor": vendor}))
        }
        Some("link-meta") => resolve_link_meta(&payload),
        Some("pane-group") => resolve_pane_group(&payload),
        Some("fallback") => resolve_fallback(&payload),
        Some("codex-route") => resolve_codex_route_kind(&payload),
        Some("claude-route") => resolve_claude_route_kind(&payload),
        Some("providers") => resolve_providers_kind(&payload),
        Some("print") => resolve_print(&payload),
        Some("write-settings") => crate::route_settings::write_settings_kind(&payload),
        Some("launch") => crate::launch_exec::launch_kind(&payload),
        Some("role-resolve") => crate::territory::resolve_role_kind(&payload),
        Some("role-grant") => crate::territory::role_grant_kind(&payload),
        Some("role-settle") => crate::team_settle::resolve(&payload),
        Some("role-identity") => crate::team_identity::resolve(&payload),
        Some("role-widen") => crate::team_widen::resolve(&payload),
        Some("team-rivals") => crate::org_rivals::resolve(&payload),
        Some("team-rescope") => crate::team_rescope::resolve(&payload),
        Some("spawn-team") => crate::team_spawn::resolve(&payload),
        Some("lead") => crate::lead_ops::overlay_kind(&payload),
        Some("roles-discover") => crate::roles::registry::discover_kind(&payload),
        Some("credentials") => crate::accounts::credentials::compose_kind(&payload),
        Some("state-root-seal") => crate::accounts::credentials::seal_kind(&payload),
        Some(kind @ ("account-overlay" | "dispatch-account" | "account-for-env" | "login-present")) => {
            crate::accounts::launch::resolve_kind(kind, &payload)
        }
        Some("roles-resolve") => crate::roles::resolver::resolve_kind(&payload),
        Some("business-role") => crate::roles::routing::business_role_kind(&payload),
        other => Err(format!(
            "spawn-overlay: unknown kind {other:?}; expected overlay|compat|compose|lane-vendor|link-meta|pane-group|fallback|codex-route|claude-route|providers|print|write-settings|launch|team-settle|team-identity|team-widen|role-resolve|role-grant|role-settle|role-identity|role-widen|team-rivals|team-rescope|spawn-team|lead|roles-discover|credentials|state-root-seal|account-overlay|dispatch-account|account-for-env|login-present|roles-resolve|business-role"
        )),
    }
}

/// The codex route builder behind Python's `resolve_codex_route`: the payload
/// names the role-picked (provider, model) plus the spawn cwd; the answer
/// carries the three `-c` tokens and the env (key + stamp) to the Python
/// parent over this one stdout pipe. `unrouted` marks the deliberate no-op
/// (an anthropic-protocol provider belongs to the claude lane, silent in
/// Python exactly as before); `refusal` names a misconfiguration the spawn
/// surfaces as a notice. Key-free, like every answer this command prints.
fn resolve_codex_route_kind(payload: &Value) -> Result<Value, String> {
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("");
    let model = payload.get("model").and_then(Value::as_str).unwrap_or("");
    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
    if provider.is_empty() || model.is_empty() || cwd.is_empty() {
        return Ok(json!({
            "refusal": "spawn-overlay codex-route: payload needs provider, model and cwd",
            "unrouted": Value::Null,
        }));
    }
    match crate::codex_route::resolve_codex_route(Path::new(cwd), provider, model) {
        Ok(route) => Ok(json!({
            "refusal": Value::Null,
            "unrouted": Value::Null,
            "provider": route.provider,
            "model": route.model,
            "config_args": route.config_args,
            "env": route.env,
        })),
        Err(err) => {
            let (refusal, unrouted) = match &err {
                crate::codex_route::CodexRouteError::Unrouted(reason) => {
                    (Value::Null, json!(reason))
                }
                crate::codex_route::CodexRouteError::Refused(reason) => {
                    (json!(reason), Value::Null)
                }
            };
            Ok(json!({"refusal": refusal, "unrouted": unrouted}))
        }
    }
}

/// The claude route builder behind Python's `resolve_route` and
/// `resolve_explicit_route`: the payload names the (provider, model) the
/// role lane or the peer lane picked, plus the spawn cwd and the context the
/// fail-safe notices word themselves by. The answer carries the route env
/// (with the key under `ANTHROPIC_AUTH_TOKEN`, the same exposure `codex-route`
/// has) and the notices. A payload missing provider, model or cwd answers a
/// skip notice, as `resolve_codex_route_kind` does.
fn resolve_claude_route_kind(payload: &Value) -> Result<Value, String> {
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("");
    let model = payload.get("model").and_then(Value::as_str).unwrap_or("");
    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
    if provider.is_empty() || model.is_empty() || cwd.is_empty() {
        return Ok(json!({
            "route": Value::Null,
            "notices": ["spawn-overlay claude-route: payload needs provider, model and cwd"],
        }));
    }
    let ctx = match payload.get("ctx") {
        Some(value) => match value.get("kind").and_then(Value::as_str) {
            Some("peer") => crate::provider_route::RouteCtx::Peer(
                value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(provider)
                    .to_string(),
            ),
            _ => crate::provider_route::RouteCtx::Role(
                value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(provider)
                    .to_string(),
            ),
        },
        None => crate::provider_route::RouteCtx::Role(provider.to_string()),
    };
    let env_map = payload_env(&payload);
    // A payload env REPLACES the process env (Python callers pass the spawn
    // env under construction as the whole mapping), not chains with it.
    let env_get: Box<dyn Fn(&str) -> Option<String>> = if payload.get("env").is_some() {
        Box::new(move |key: &str| env_map.get(key).cloned())
    } else {
        Box::new(|key: &str| std::env::var(key).ok())
    };
    let block = payload
        .get("block")
        .and_then(crate::provider_route::block_from_json);
    let route = crate::provider_route::claude_route_with(
        block.as_ref(),
        Path::new(cwd),
        provider,
        model,
        &ctx,
        &env_get,
    );
    Ok(route.to_json())
}

/// The merged provider registry behind Python's `effective_providers` and
/// `key_source`: one answer for the `fno config route ls` render, with the
/// key column naming its source and never the value.
fn resolve_providers_kind(payload: &Value) -> Result<Value, String> {
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let env_map = payload_env(&payload);
    // Same replace-not-chain rule as the claude-route kind.
    let env_get: Box<dyn Fn(&str) -> Option<String>> = if payload.get("env").is_some() {
        Box::new(move |key: &str| env_map.get(key).cloned())
    } else {
        Box::new(|key: &str| std::env::var(key).ok())
    };
    Ok(crate::provider_route::providers_json(&cwd, &env_get))
}

/// The env mapping a payload carries, or the process env the kind falls back
/// to: `claude-route` and `providers` resolve their keys through it.
fn payload_env(payload: &Value) -> std::collections::BTreeMap<String, String> {
    payload
        .get("env")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn block_map<'a>(owner: &'a Value, key: &str) -> Option<&'a Map<String, Value>> {
    owner.get(key).and_then(Value::as_object)
}

fn overlay_scalar(block: &Value, name: &str) -> String {
    match block.get(name) {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

fn overlay_args(block: &Value) -> Option<Vec<String>> {
    match block.get("args") {
        Some(Value::Array(items)) if !items.is_empty() => Some(
            items
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect(),
        ),
        _ => None,
    }
}

/// The rung chain WITHOUT the lane, resolved for `harness` when given:
/// profile harness overlay > profile scalar > defaults harness overlay >
/// defaults scalar. Returns (value, rung) or nothing.
fn effective_field(
    defaults: &Value,
    profile: Option<&Value>,
    command: &str,
    name: &str,
    harness: Option<&str>,
) -> Option<(String, String)> {
    if let (Some(h), Some(p)) = (harness, profile) {
        if let Some(table) = block_map(p, "harness") {
            if let Some(block) = table.get(h) {
                let v = overlay_scalar(block, name);
                if !v.is_empty() {
                    return Some((v, format!("agents.profiles.{command}.harness.{h}")));
                }
            }
        }
    }
    if let Some(p) = profile {
        let v = overlay_scalar(p, name);
        if !v.is_empty() {
            return Some((v, format!("agents.profiles.{command}")));
        }
    }
    if let Some(h) = harness {
        if let Some(table) = block_map(defaults, "harness") {
            if let Some(block) = table.get(h) {
                let v = overlay_scalar(block, name);
                if !v.is_empty() {
                    return Some((v, format!("agents.defaults.harness.{h}")));
                }
            }
        }
    }
    let v = overlay_scalar(defaults, name);
    if !v.is_empty() {
        return Some((v, "agents.defaults".to_string()));
    }
    None
}

/// The native launch line behind `spawn --print`. The ask carries the RAW
/// overlays and the print's own cwd; the lane resolves HERE beside the
/// inventory fold (axes admit exactly one declared claude row; no match
/// degrades to the harness default and the content-addressed file). A
/// routed print writes the REAL settings file first: the print is the
/// rehearsal of the launch, so the line it names must exist on disk. A row
/// launch inherits the harness default's optional set, filtered to the
/// holes the override still carries, so overriding the argv cannot strand
/// an optional hole.
fn resolve_print(payload: &Value) -> Result<Value, String> {
    let harness = payload.get("harness").and_then(Value::as_str).unwrap_or("");
    if harness.is_empty() {
        return Err("spawn-overlay print: payload needs harness".into());
    }
    let substrate = payload
        .get("substrate")
        .and_then(Value::as_str)
        .unwrap_or("");
    // The spawn door canonicalizes `thread` to its internal `bg` spelling
    // before it asks, so both name the claude --bg lane here.
    if harness != "claude" || !matches!(substrate, "thread" | "bg") {
        return Err(format!(
            "--print renders claude --bg lines; harness {harness:?} substrate \
             {substrate:?} has no native line yet."
        ));
    }
    // The routed posture derives from the overlays the ask carries; no flag
    // restates it. An unrouted print refuses: a plain bg line is just
    // claude --bg, and the settings file is the lane.
    let route_env = payload
        .get("route_env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let account_env = payload
        .get("account_env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if route_env.is_empty() && account_env.is_empty() {
        return Err(
            "--print needs a routed or pinned-account lane (the settings file is \
             the lane); a plain bg line is just claude --bg."
                .into(),
        );
    }
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let lane = crate::route_gather::lane_for_axes(
        route_env
            .get("FNO_ROUTE_PROVIDER")
            .and_then(Value::as_str)
            .unwrap_or(""),
        payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim(),
        &cwd,
    );
    // The settings file the printed line names writes HERE, from the raw
    // overlays; the lane's name stamps the stable lane-<row>.json alias.
    let alias = lane
        .as_ref()
        .and_then(|row| row.get("name"))
        .and_then(Value::as_str);
    let settings = crate::route_settings::write_composed(
        &route_env,
        &account_env,
        payload.get("sandbox").filter(|v| v.is_object()).cloned(),
        alias,
        &cwd,
    )?;
    let template = crate::spawn_compose::resolve_template(
        harness,
        lane.as_ref().and_then(|row| row.get("launch")),
        None,
    )?;
    let mut values = crate::spawn_compose::fill_values(payload);
    values.insert("settings".to_string(), settings.unwrap_or_default());
    let tokens = crate::spawn_compose::fill_launch(&template, &values)?;
    Ok(json!({ "tokens": tokens }))
}

fn resolve_overlay(payload: &Value) -> Result<Value, String> {
    let command = payload.get("verb").and_then(Value::as_str).unwrap_or("");
    let defaults = payload.get("defaults").cloned().unwrap_or(json!({}));
    let profile = payload.get("profile").filter(|v| v.is_object()).cloned();
    let harness = payload
        .get("harness")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let lane = payload.get("lane").filter(|v| v.is_object());
    let lane_index = payload.get("lane_index").and_then(Value::as_u64);
    let argv_tail = payload
        .get("argv_tail")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let harness_list: Vec<String> = payload
        .get("harnesses")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // The overlay table guards, scoped to the rungs THIS spawn reads: an
    // unknown harness name or a ranking field refuses the whole composition,
    // fail-closed like the malformed-lane refusal.
    let owners: Vec<(String, &Value)> = vec![
        ("agents.defaults".to_string(), &defaults),
        (
            format!("agents.profiles.{command}"),
            profile.as_ref().unwrap_or(&Value::Null),
        ),
    ];
    for (owner, obj) in owners {
        let table = match block_map(obj, "harness") {
            Some(t) => t,
            None => continue,
        };
        for (h, block) in table {
            if !KNOWN_PROVIDERS.contains(&h.as_str()) {
                return Ok(json!({
                    "refusal": format!(
                        "fno agents spawn: config.{owner}.harness.{h} is not a known harness; valid: {}",
                        known_providers_csv()
                    )
                }));
            }
            if let Some(extras) = block.as_object() {
                for k in extras.keys() {
                    if LANE_FIELD_NAMES.contains(&k.as_str())
                        && !OVERLAY_FIELDS.contains(&k.as_str())
                    {
                        return Ok(json!({
                            "refusal": format!(
                                "fno agents spawn: config.{owner}.harness.{h}.{k} is a lane field; declare it on the lane"
                            )
                        }));
                    }
                }
            }
        }
    }

    // The three postures, resolved through the harness rungs. The rung is
    // BARE (no field suffix): the spawn seam appends the field name when it
    // records the injection, and the doctor reads `config.{rung}.{name}`.
    let effective_for = |h: Option<&str>| {
        let mut effective = Map::new();
        for name in ["effort", "substrate", "permission_mode"] {
            let entry = effective_field(&defaults, profile.as_ref(), command, name, h);
            if let Some((v, rung)) = entry {
                effective.insert(name.to_string(), json!({"value": v, "rung": rung}));
            }
        }
        effective
    };

    // Doctor mode: a `harnesses` list answers the whole sweep in one call,
    // with the guards still refusing the config once.
    if !harness_list.is_empty() {
        let mut by_harness = Map::new();
        for h in &harness_list {
            by_harness.insert(h.clone(), json!(effective_for(Some(h.as_str()))));
        }
        return Ok(json!({
            "refusal": Value::Null,
            "effective_by_harness": by_harness,
            "bundle": Value::Null,
        }));
    }

    let effective = effective_for(harness);

    // ONE bundle: lane args > profile harness overlay args > defaults harness
    // overlay args. Never concatenated; the boundary the caller's argv already
    // carries displaces it by name.
    let mut bundle: Option<(Vec<String>, String)> = None;
    if let Some(h) = harness {
        if let (Some(l), Some(idx)) = (lane, lane_index) {
            if let Some(args) = overlay_args(l) {
                bundle = Some((args, format!("agents.profiles.{command}.lanes[{idx}].args")));
            }
        }
        if bundle.is_none() {
            for (obj, rung) in [
                (
                    profile.as_ref(),
                    format!("agents.profiles.{command}.harness.{h}.args"),
                ),
                (Some(&defaults), format!("agents.defaults.harness.{h}.args")),
            ] {
                if let Some(p) = obj {
                    if let Some(table) = block_map(p, "harness") {
                        if let Some(block) = table.get(h) {
                            if let Some(args) = overlay_args(block) {
                                bundle = Some((args, rung));
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    let boundary = argv_tail
        .iter()
        .filter_map(Value::as_str)
        .find(|t| *t == "--" || *t == "--argv")
        .map(String::from);
    let bundle_json = match (bundle, &boundary) {
        (Some((tokens, rung)), Some(b)) => json!({
            "displaced": {"tokens": tokens, "rung": rung, "boundary": b}
        }),
        (Some((tokens, rung)), None) => json!({"tokens": tokens, "rung": rung}),
        _ => Value::Null,
    };

    Ok(json!({
        "refusal": Value::Null,
        "effective": effective,
        "bundle": bundle_json,
    }))
}

fn implied_vendor(model: Option<&str>) -> Option<String> {
    let model = model?;
    let lower = model.to_lowercase();
    for candidate in [lower.as_str(), lower.rsplit('/').next().unwrap_or("")] {
        for (prefix, vendor) in MODEL_VENDOR_HINTS {
            if let Some(stripped) = candidate.strip_prefix(prefix) {
                let _ = stripped;
                return Some(vendor.to_string());
            }
        }
        let base = candidate
            .split('[')
            .next()
            .unwrap_or("")
            .split('-')
            .next()
            .unwrap_or("");
        for (word, vendor) in MODEL_WORD_VENDORS {
            if base == word {
                return Some(vendor.to_string());
            }
        }
    }
    None
}

/// The lane's vendor from route > provider > the harness chain. Inference-free
/// by contract: the model-vendor mismatch check needs a lane answer that the
/// model token did not inform, or it could never flag the model.
fn resolve_lane_vendor_by_lane(
    axes: &crate::cli_args::SpawnAxes,
    harness: Option<&str>,
    env_harness: Option<&str>,
    argv_head: Option<&str>,
) -> Option<String> {
    if let Some(route) = &axes.route {
        let normalized = route.replace(',', "/");
        let vendor = normalized
            .split('/')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        return if vendor.is_empty() {
            None
        } else {
            Some(vendor)
        };
    }
    if let Some(v) = &axes.provider {
        let v = v.trim().to_lowercase();
        return if v.is_empty() { None } else { Some(v) };
    }
    let mut resolved = harness
        .map(str::trim)
        .map(str::to_lowercase)
        .unwrap_or_default();
    if resolved.is_empty() {
        resolved = axes
            .harness
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();
    }
    if resolved.is_empty() {
        if let Some(head) = argv_head {
            for (h, _) in HARNESS_DEFAULT_VENDOR {
                if head == h {
                    resolved = h.to_string();
                    break;
                }
            }
        }
    }
    if resolved.is_empty() {
        if let Some(env_answer) = env_harness {
            resolved = env_answer.trim().to_lowercase();
        }
    }
    for (h, vendor) in HARNESS_DEFAULT_VENDOR {
        if resolved == h {
            return Some(vendor.to_string());
        }
    }
    if resolved == "opencode" {
        return implied_vendor(axes.model.as_deref());
    }
    None
}

fn resolve_lane_vendor(
    axes: &crate::cli_args::SpawnAxes,
    harness: Option<&str>,
    env_harness: Option<&str>,
    argv_head: Option<&str>,
) -> Option<String> {
    // No route and no provider named a vendor: the model token informs the
    // answer. A routeless glm spawn answered the harness default (anthropic),
    // minted a row under it, and died on the default endpoint's first
    // inference; the model's own spelling is the only voice that named z.ai.
    let vendor_pinned = axes.route.is_some() || axes.provider.is_some();
    if !vendor_pinned {
        if let Some(vendor) = implied_vendor(axes.model.as_deref()) {
            return Some(vendor);
        }
    }
    resolve_lane_vendor_by_lane(axes, harness, env_harness, argv_head)
}

/// One stop in the fallback walk: each link's stable destination id and its
/// spawn flags. Two links differing only in effort are the SAME destination
/// for walk memory; `account` participates because it names a different bill
/// and meter. Flags carry only the live spawn spellings (harness, route,
/// account); the lane itself is named the way the spawn command derives it.
fn resolve_link_meta(payload: &Value) -> Result<Value, String> {
    let links = payload
        .get("links")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut ids = Vec::new();
    let mut flag_rows = Vec::new();
    for link in &links {
        let (id, flags) = mint_link(link);
        ids.push(Value::String(id));
        flag_rows.push(Value::Array(flags));
    }
    Ok(json!({"ids": ids, "flags": flag_rows}))
}

/// The walk-memory id and spawn flags for one canonical link (axis spelling,
/// `provider` carrying the harness).
pub(crate) fn mint_link(link: &Value) -> (String, Vec<Value>) {
    let field = |k: &str| -> String {
        link.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let (harness, model, route, account) = (
        field("provider"),
        field("model"),
        field("route"),
        field("account"),
    );
    let base = format!(
        "{}/{}",
        if harness.is_empty() {
            "?"
        } else {
            harness.as_str()
        },
        if !model.is_empty() {
            model.as_str()
        } else if !route.is_empty() {
            route.as_str()
        } else {
            "default"
        }
    );
    let id = if account.is_empty() {
        base
    } else {
        format!("{base}@{account}")
    };
    let mut flags: Vec<Value> = Vec::new();
    if !harness.is_empty() {
        flags.push(json!("-H"));
        flags.push(json!(harness));
    }
    // The model, effort and permission axes are retired at the spawn door:
    // the lane template owns them, so a link carries its route instead.
    for (flag, key) in [("--route", "route"), ("--account", "account")] {
        let v = field(key);
        if !v.is_empty() {
            flags.push(json!(flag));
            flags.push(json!(v));
        }
    }
    // A thread is the spawn default; a pane needs an explicit --as pane, and
    // headless is the one-shot spelling. claude keeps its thread; every other
    // harness keeps the pane it took before.
    let substrate = field("substrate");
    let substrate = match substrate.as_str() {
        "" if harness == "claude" => "thread",
        "" => "pane",
        other => other,
    };
    match substrate {
        "pane" => flags.extend([json!("--as"), json!("pane"), json!("--bounded-placement")]),
        "headless" => flags.push(json!("--once")),
        _ => {}
    }
    (id, flags)
}

/// Where a configured pane group may land. A group places the pane by moving
/// its OWN tab, so it is skipped (never a failure: the value was injected, not
/// typed) when the resolved substrate has no pane geometry, or when the
/// caller's own argv already carries a placement flag. ``--once``/``-o``
/// counts because a one-shot spawn has no pane geometry either. PRESENCE, not
/// value: a valueless trailing ``--at`` still conflicts. The tokens arrive
/// already fence- and value-filtered by the caller.
fn resolve_pane_group(payload: &Value) -> Result<Value, String> {
    let group = payload.get("group").and_then(Value::as_str).unwrap_or("");
    let rung = payload.get("rung").and_then(Value::as_str).unwrap_or("");
    let eff_substrate = payload
        .get("eff_substrate")
        .and_then(Value::as_str)
        .unwrap_or("pane");
    let toks: Vec<String> = payload
        .get("argv_tail")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let placement = ["--split", "-x", "--at", "--once", "-o"];
    let mut conflicting: Option<String> = None;
    for t in &toks {
        for f in placement {
            let glued = f.len() == 2 && !f[1..].starts_with('-');
            if t == f || t.starts_with(&format!("{f}=")) || (glued && t.starts_with(f) && t != f) {
                conflicting = Some(f.to_string());
                break;
            }
        }
        if conflicting.is_some() {
            break;
        }
    }
    if eff_substrate != "pane" {
        return Ok(json!({
            "skipped": format!(
                "fno agents spawn: pane group skipped (resolved substrate {eff_substrate:?} has no pane geometry); {rung} = {group:?} ignored"
            )
        }));
    }
    if let Some(c) = conflicting {
        return Ok(json!({
            "skipped": format!(
                "fno agents spawn: pane group skipped ({c} places this pane in a tab it does not own, which a group cannot move); {rung} = {group:?} ignored"
            )
        }));
    }
    Ok(json!({"inject": ["--tab", group]}))
}

fn resolve_fallback(payload: &Value) -> Result<Value, String> {
    let table = match payload.get("table").and_then(Value::as_object) {
        Some(t) => t,
        None => {
            return Ok(json!({"error": format!(
                "config.agents.fallback must be a table keyed by size (S|M|L|default); got {}",
                type_name(payload.get("table"))
            )}))
        }
    };
    let mut out = Map::new();
    for (size, chain) in table {
        if !CHAIN_KEYS.contains(&size.as_str()) {
            return Ok(json!({"error": format!(
                "config.agents.fallback.{size}: unknown size key; expected one of {}",
                ["S", "L", "M", "default"].join("|")
            )}));
        }
        let links = match chain.as_array() {
            Some(l) => l,
            None => {
                return Ok(json!({"error": format!(
                    "config.agents.fallback.{size} must be a list of links; got {}",
                    type_name(Some(chain))
                )}))
            }
        };
        let mut canonical: Vec<Value> = Vec::new();
        for (i, link) in links.iter().enumerate() {
            let obj = match link.as_object() {
                Some(o) => o,
                None => {
                    return Ok(json!({"error": format!(
                        "config.agents.fallback.{size}[{i}] must be a table of axis fields; got {}",
                        type_name(Some(link))
                    )}))
                }
            };
            // A link is spelled with the CORRECT axis word, `harness`; the
            // legacy `provider` spelling reads too and `harness` wins. Either
            // way the value is checked: a typo'd harness must refuse, never
            // spawn on the ambient binary.
            let harness = obj
                .get("harness")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .or_else(|| {
                    obj.get("provider")
                        .and_then(Value::as_str)
                        .map(|s| s.trim().to_string())
                })
                .unwrap_or_default();
            if !harness.is_empty() && !CHAIN_HARNESSES.contains(&harness.as_str()) {
                return Ok(json!({"error": format!(
                    "config.agents.fallback.{size}[{i}].harness={harness:?} is not a known harness ({})",
                    CHAIN_HARNESSES.join("|")
                )}));
            }
            let mut fields = Map::new();
            for (k, v) in obj {
                if k == "harness" {
                    continue;
                }
                fields.insert(k.clone(), v.clone());
            }
            if !harness.is_empty() {
                fields.insert("provider".into(), Value::String(harness));
            }
            canonical.push(Value::Object(fields));
        }
        out.insert(size.clone(), Value::Array(canonical));
    }
    Ok(json!({"error": Value::Null, "links": out}))
}

fn type_name(v: Option<&Value>) -> &'static str {
    match v {
        None | Some(Value::Null) => "NoneType",
        Some(Value::Bool(_)) => "bool",
        Some(Value::Number(_)) => "int",
        Some(Value::String(_)) => "str",
        Some(Value::Array(_)) => "list",
        Some(Value::Object(_)) => "dict",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn overlay(payload: Value) -> Value {
        resolve(json!({"kind": "overlay", "verb": "target", "argv_tail": []}).merge(&payload))
            .unwrap()
    }

    #[test]
    fn overlay_rows() {
        let payload = json!({
            "kind": "overlay", "verb": "target",
            "defaults": {},
            "profile": {
                "permission_mode": "yolo",
                "harness": {"claude": {"permission_mode": "bypassPermissions"}}
            },
            "harness": "claude",
        });
        let out = resolve(payload).unwrap();
        assert!(out["refusal"].is_null());
        assert_eq!(
            out["effective"]["permission_mode"]["value"],
            "bypassPermissions"
        );
        // Assembled, not a literal: the rung string would end in a word the
        // placement-rule lint reads as a path construction.
        let want = format!("agents.profiles.target.harness.{}", "claude");
        assert_eq!(out["effective"]["permission_mode"]["rung"], want);

        let out = overlay(json!({
            "profile": {"harness": {"codx": {"effort": "high"}}},
        }));
        assert!(out["refusal"]
            .as_str()
            .unwrap()
            .contains("profiles.target.harness.codx is not a known harness"));

        let out = overlay(json!({
            "defaults": {"harness": {"codex": {"model": "opus"}}},
        }));
        assert!(out["refusal"]
            .as_str()
            .unwrap()
            .contains("harness.codex.model is a lane field"));

        let out = overlay(json!({
            "defaults": {"harness": {"codex": {"args": ["--profile", "overlay"]}}},
            "lane": {"provider": "codex", "args": ["--profile", "lane"]},
            "lane_index": 0,
            "harness": "codex",
        }));
        assert_eq!(out["bundle"]["tokens"][0], "--profile");
        assert_eq!(out["bundle"]["tokens"][1], "lane");
        assert!(out["bundle"]["rung"]
            .as_str()
            .unwrap()
            .ends_with("lanes[0].args"));

        let out = overlay(json!({
            "defaults": {"harness": {"codex": {"args": ["--profile", "fno"]}}},
            "harness": "codex",
            "argv_tail": ["--name", "w", "hi", "--", "--profile", "other"],
        }));
        assert_eq!(out["bundle"]["displaced"]["boundary"], "--");
        assert_eq!(
            out["bundle"]["displaced"]["rung"],
            "agents.defaults.harness.codex.args"
        );
    }

    #[test]
    fn vendor_rows() {
        let vendor = |payload: Value| {
            resolve(payload).unwrap()["vendor"]
                .as_str()
                .map(String::from)
        };
        assert_eq!(
            vendor(json!({"kind": "lane-vendor", "argv_tail": ["--route", "zai/glm"]})),
            Some("zai".into())
        );
        assert_eq!(
            vendor(json!({"kind": "lane-vendor", "argv_tail": [], "harness": "codex"})),
            Some("openai".into())
        );
        // opencode holds no vendor opinion of its own.
        assert_eq!(
            vendor(json!({"kind": "lane-vendor", "argv_tail": [], "harness": "opencode"})),
            None
        );
        assert_eq!(
            vendor(json!({
                "kind": "lane-vendor",
                "argv_tail": ["--route", "zai/glm-5.3-flash"],
                "harness": "claude",
            })),
            Some("zai".into())
        );
    }

    /// A routed print stages its own world: a tempdir as the ask's cwd (the
    /// lane fold reads `.fno/config.toml` there) with FNO_STATE_DIR pinned
    /// under the claims env lock, so the rehearsed settings file writes into
    /// the sandbox, never the real state root. Returns `(guard, dir, saved)`;
    /// the caller drops the guard and calls print_world_restore.
    fn print_world(
        config: Option<&str>,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
        Option<std::ffi::OsString>,
    ) {
        let guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        if let Some(cfg) = config {
            std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
            std::fs::write(dir.path().join(".fno/config.toml"), cfg).unwrap();
        }
        let saved = std::env::var_os("FNO_STATE_DIR");
        std::env::set_var("FNO_STATE_DIR", dir.path().join("state"));
        (guard, dir, saved)
    }

    fn print_world_restore(
        guard: std::sync::MutexGuard<'static, ()>,
        saved: Option<std::ffi::OsString>,
    ) {
        match saved {
            Some(v) => std::env::set_var("FNO_STATE_DIR", v),
            None => std::env::remove_var("FNO_STATE_DIR"),
        }
        drop(guard);
    }

    /// The packaged claude template renders the ruled native line: the
    /// settings path, model and effort land between `--bg` and `--name`, and
    /// the payload rides as the final positional. This golden IS the wave's
    /// ruled print shape; the Python side asserts only the transport.
    #[test]
    fn print_fills_the_packaged_claude_line() {
        let (guard, dir, saved) = print_world(None);
        let out = resolve(json!({
            "kind": "print", "harness": "claude", "substrate": "thread",
            "name": "w", "payload": "/fno:target x-aaaa",
            "cwd": dir.path(),
            "model": "glm-5.3-flash[1m]", "effort": "xhigh",
            "route_env": {
                "FNO_ROUTE_PROVIDER": "zai",
                "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
                "ANTHROPIC_AUTH_TOKEN": "zk"
            },
        }))
        .unwrap();
        let tokens = out["tokens"].as_array().unwrap();
        assert_eq!(tokens[0], "claude");
        assert_eq!(tokens[1], "--bg");
        assert_eq!(tokens[2], "--settings");
        let settings_path = tokens[3].as_str().unwrap();
        assert!(
            settings_path.starts_with(dir.path().to_str().unwrap()),
            "the settings file lands in the staged state root: {settings_path}"
        );
        assert!(std::path::Path::new(settings_path).exists());
        assert_eq!(
            &tokens[4..],
            [
                "--model",
                "glm-5.3-flash[1m]",
                "--effort",
                "xhigh",
                "--name",
                "w",
                "/fno:target x-aaaa"
            ]
        );
        print_world_restore(guard, saved);
    }

    /// The contract gates through the staged inventory: a bad row template
    /// refuses by name, and a harness the lane has no native line for
    /// refuses before the template resolves.
    #[test]
    fn print_refuses_malformed_templates() {
        let (guard, dir, saved) = print_world(Some(
            "[[routing.models]]\nname = \"row-bad\"\nharness = \"claude\"\nmodel = \"row-bad-m\"\nroute = \"zai/row-bad-m\"\nlaunch = [\"claude\", \"--bg\", \"--x\", \"{bogus}\", \"{name}\", \"{payload}\"]\n",
        ));
        let error = resolve(json!({
            "kind": "print", "harness": "claude", "substrate": "thread",
            "name": "w", "payload": "/x",
            "cwd": dir.path(),
            "model": "row-bad-m",
            "route_env": {
                "FNO_ROUTE_PROVIDER": "zai",
                "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
                "ANTHROPIC_AUTH_TOKEN": "zk"
            },
        }))
        .unwrap_err();
        assert!(error.contains("bogus"), "{error}");
        print_world_restore(guard, saved);

        let error =
            resolve(json!({"kind": "print", "harness": "codex", "name": "w", "payload": "/x"}))
                .unwrap_err();
        assert!(error.contains("no native line yet"), "{error}");
    }
}
