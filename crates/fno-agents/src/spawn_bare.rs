//! A bare spawn's duties in the client: the default substrate and its thread
//! refusals, the fno-owned flags `--wait`, `--print`, `--prompt-file`,
//! `--sandbox-write-policy` and `--mux-session`, the `--wait` gate retry,
//! the `--print` line and the native lane. The Python twin is `cmd_spawn`
//! (`fno.agents.cli`), which answers only `FNO_AGENTS_RUNTIME=python` and is
//! the golden source until the seam delete removes it.

use crate::harness_capabilities::HarnessContract;
use crate::spawn_gate::Refusal;
use crate::spawn_route::Route;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// The fno-owned spawn flags this module parses, each with a value except
/// `--print`.
pub const FLAGS: &[&str] = &[
    "--wait",
    "--print",
    "--prompt-file",
    "--sandbox-write-policy",
    "--mux-session",
];

/// Whether `token` is one of [`FLAGS`], in its space or `=` form.
pub fn owns(token: &str) -> bool {
    FLAGS.contains(&token.split_once('=').map_or(token, |(flag, _)| flag))
}

/// Parse one of [`FLAGS`] into its spawn param: `wait`, `print`,
/// `prompt_file`, `sandbox_write_policy`, and `session` (the key the pane
/// spine reads for its mux session).
pub fn take_flag(
    token: &str,
    args: &mut impl Iterator<Item = String>,
    params: &mut Map<String, Value>,
) -> Result<(), String> {
    let (flag, inline) = match token.split_once('=') {
        Some((flag, value)) => (flag, Some(value.to_string())),
        None => (token, None),
    };
    if flag == "--print" {
        params.insert("print".into(), Value::Bool(true));
        return Ok(());
    }
    let value = match inline {
        Some(v) => v,
        None => args.next().ok_or_else(|| format!("{flag} needs a value"))?,
    };
    let key = match flag {
        "--mux-session" => "session".to_string(),
        other => other[2..].replace('-', "_"),
    };
    params.insert(key, Value::String(value));
    Ok(())
}

/// The substrate an unflagged spawn takes: `--once` gives headless, else the
/// thread where this client seats one (claude, codex, opencode), else the
/// pane. The harness defaults to codex, as the daemon's `handle_spawn` does.
/// `--once` beside `--as pane` runs headless, as `cmd_spawn` does.
pub fn settle_substrate(params: &mut Map<String, Value>) {
    let once = params.get("once").and_then(Value::as_bool) == Some(true);
    let harness = params.get("provider").and_then(Value::as_str);
    let substrate = params.get("substrate").and_then(Value::as_str);
    let settled = match substrate {
        None if once => "headless",
        None if matches!(harness.unwrap_or("codex"), "claude" | "codex" | "opencode") => "thread",
        None => "pane",
        Some("pane") if once => "headless",
        Some(_) => return,
    };
    params.insert("substrate".into(), Value::String(settled.into()));
}

/// The first fenced token the harness's thread lane cannot carry, or `None`.
/// Reads the thread carrier row, else the keeper row; `*` carries every
/// token, and a carried spelling consumes its value token too.
pub fn fence_uncarried(
    contract: &HarnessContract,
    harness: &str,
    fenced: &[String],
) -> Option<String> {
    let caps = contract.capabilities(harness).ok()?;
    let covered = caps
        .thread
        .as_ref()
        .map(|t| &t.passthrough)
        .or_else(|| caps.keeper.as_ref().map(|k| &k.passthrough))?;
    if covered.iter().any(|c| c == "*") {
        return None;
    }
    let mut skip_value = false;
    for token in fenced {
        if skip_value {
            skip_value = false;
        } else if covered.contains(token) {
            skip_value = true;
        } else if !covered
            .iter()
            .any(|c| c == token.split('=').next().unwrap_or(""))
        {
            return Some(token.clone());
        }
    }
    None
}

/// The refusal for a thread the harness cannot seat, or whose fence holds a
/// token the lane cannot carry.
pub fn thread_refusal(
    contract: &HarnessContract,
    harness: &str,
    fenced: &[String],
) -> Option<String> {
    let why = match fence_uncarried(contract, harness, fenced) {
        Some(token) => format!("has no carrier for {token}"),
        None if crate::backlog::dispatch_resolve::thread_seatable(contract, harness) => {
            return None
        }
        None => "is not seatable".to_string(),
    };
    Some(format!(
        "fno agents spawn: the {harness} thread lane {why}; add --as pane"
    ))
}

/// `cmd_spawn`'s bare-spawn admission, in its order: the prompt file, the
/// thread seat, the seedless claude thread, the mux session, the sandbox
/// write policy and the `--wait` duration. A refusal is its line and exit.
/// A loaded sandbox block rides `params["sandbox"]`; `--wait` forces
/// `no_wait` and rides `params["wait_secs"]`.
pub fn admit(params: &mut Map<String, Value>) -> Result<(), (String, i32)> {
    let text = |params: &Map<String, Value>, key: &str| {
        params.get(key).and_then(Value::as_str).map(str::to_string)
    };
    if let Some(path) = text(params, "prompt_file") {
        let inline = text(params, "message").filter(|m| !m.is_empty());
        let read = crate::text_or_file::read_text_arg(inline.as_deref(), Some(&path), "the prompt")
            .map_err(|e| (e, 1))?;
        params.insert("message".into(), Value::String(read.unwrap_or_default()));
    }
    let harness = text(params, "provider").unwrap_or_else(|| {
        crate::provider::infer_dispatch_provider(|k| std::env::var(k).ok()).to_string()
    });
    let substrate = text(params, "substrate").unwrap_or_default();
    if substrate == "thread" {
        let fenced: Vec<String> = params
            .get("harness_args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let contract =
            HarnessContract::packaged().map_err(|e| (format!("fno agents spawn: {e}"), 2))?;
        if let Some(line) = thread_refusal(&contract, &harness, &fenced) {
            return Err((line, 2));
        }
    }
    let blank = text(params, "message").is_none_or(|m| m.trim().is_empty());
    if let Some(node) = text(params, "node").filter(|n| blank && !n.is_empty()) {
        render_node_seed(params, &node, &harness)?;
    }
    let promoted = params
        .get("promote")
        .and_then(Value::as_array)
        .is_some_and(|scopes| !scopes.is_empty());
    let non_empty = |key: &str| text(params, key).filter(|s| !s.is_empty());
    if let Some(refusal) = crate::spawn_admission::seedless_thread_refusal(
        Some(harness.as_str()),
        Some(substrate.as_str()),
        text(params, "message").as_deref(),
        None,
        promoted,
        non_empty("name").as_deref(),
        non_empty("node").as_deref(),
    ) {
        return Err((format!("fno agents spawn: {refusal}"), 2));
    }
    // The pane spine never reaches the print kind, so its refusal is said
    // here, in the overlay's own words, before any lane can launch.
    let lane = if substrate == "thread" {
        "bg"
    } else {
        substrate.as_str()
    };
    if params.get("print").and_then(Value::as_bool) == Some(true)
        && (harness != "claude" || lane != "bg")
    {
        return Err((
            format!(
                "fno-agents spawn-overlay exited 2: spawn-overlay: --print renders claude --bg lines; harness {harness:?} substrate {lane:?} has no native line yet."
            ),
            2,
        ));
    }
    if params.contains_key("session") && substrate != "pane" {
        return Err((
            "--mux-session needs --as pane; only a pane has a mux session to spawn into".into(),
            2,
        ));
    }
    if let Some(path) = text(params, "sandbox_write_policy").filter(|p| !p.is_empty()) {
        let block = sandbox_block(&path, &substrate, &harness)?;
        params.insert("sandbox".into(), block);
    }
    if let Some(raw) = text(params, "wait") {
        if params.get("no_wait").and_then(Value::as_bool) == Some(true) {
            return Err((
                "error: --wait and --no-wait are mutually exclusive".into(),
                2,
            ));
        }
        let secs = crate::duration::parse_wait_secs(&raw).ok_or_else(|| {
            (
                format!(
                    "error: --wait wants a positive duration like 5m, 90s or 1h (got {})",
                    crate::claude_ask::py_repr(&raw)
                ),
                2,
            )
        })?;
        params.insert("no_wait".into(), Value::Bool(true));
        params.insert("wait_secs".into(), json!(secs));
    }
    Ok(())
}

/// `--node` with no message: the node's own seed fills the message, its brief
/// env joins this process's env, and with no `--cwd` or `--here` the node's
/// worktree becomes the launch dir. `cmd_spawn`'s `render_node_seed` twin.
fn render_node_seed(
    params: &mut Map<String, Value>,
    node: &str,
    harness: &str,
) -> Result<(), (String, i32)> {
    let row = crate::spawn_compose::graph_row_for(node)
        .filter(|row| row.get("archived_at").is_none_or(Value::is_null));
    let field = |key: &str| {
        row.as_ref()
            .and_then(|r| r.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let id = field("id").unwrap_or_else(|| node.to_string());
    let command = field("dispatch_command")
        .or_else(|| field(crate::backlog::model::LEGACY_DISPATCH_COMMAND_KEY));
    let (Some(row), Some(command)) = (row.as_ref(), command) else {
        return Err((
            format!(
                "refusing node-seeded spawn: node {id} carries no dispatch_command and no message was typed; an idle worker holds a fleet slot and reads as alive. Encode one with `fno backlog update {id} dispatch_command=<command>`."
            ),
            2,
        ));
    };
    let cfg_root = field("cwd")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let floor = crate::agents_config::config_lookup(&cfg_root, &["dispatch", "blueprint_floor"])
        .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR.to_string());
    let lifecycle =
        crate::backlog_ready::effective_command_with_floor(row, &floor).map_err(|e| (e, 2))?;
    let brief = crate::backlog::autobrief::resolve_dispatch_brief(row);
    let stage = lifecycle.0.clone().unwrap_or_else(|| command.clone());
    let cfg = crate::backlog::dispatch_resolve::dispatch_cfg_for(None, &stage);
    let input = crate::backlog::dispatch_resolve::DispatchInput {
        harness: Some(harness),
        node_id: Some(id.as_str()),
        command_name: Some(command.as_str()),
        lifecycle: Some(lifecycle),
        brief: brief.text.as_deref(),
        trigger: "autonomous".to_string(),
        ..Default::default()
    };
    let resolved =
        crate::backlog::dispatch_resolve::resolve_dispatch(&input, &cfg).map_err(|e| (e, 2))?;
    for (key, value) in &resolved.env {
        std::env::set_var(key, value);
    }
    params.insert("message".into(), Value::String(resolved.command));
    for (key, source) in [("slug", "slug"), ("plan", "plan_path")] {
        if let (false, Some(value)) = (params.contains_key(key), field(source)) {
            params.insert(key.into(), Value::String(value));
        }
    }
    let here = params.get("here").and_then(Value::as_bool) == Some(true);
    let cwd_given = params
        .get("cwd")
        .and_then(Value::as_str)
        .is_some_and(|c| !c.is_empty());
    if !here && !cwd_given {
        let recorded = field("_resolved_cwd")
            .or_else(|| field("cwd"))
            .map(std::path::PathBuf::from);
        let recorded = recorded.unwrap_or_else(|| cfg_root.clone());
        let workdir = crate::launch_workdir::ensure_node_workdir(&recorded, &id, harness).map_err(|_| {
            (
                format!(
                    "fno agents spawn: worktree ensure refused or misconfigured for {id}; holding the node rather than launching on canonical main"
                ),
                2,
            )
        })?;
        params.insert(
            "cwd".into(),
            Value::String(workdir.to_string_lossy().into_owned()),
        );
    }
    Ok(())
}

/// A `--sandbox-write-policy` file's enabled `sandbox` block, behind the
/// thread-only and claude-only refusals (a pane's `--settings` slot belongs
/// to the mux hook server; another harness would enforce nothing).
pub fn sandbox_block(path: &str, substrate: &str, harness: &str) -> Result<Value, (String, i32)> {
    if substrate == "pane" {
        return Err((
            "--sandbox-write-policy needs the thread substrate: a pane spawn's --settings slot is reserved by the mux hook server, so a pane cannot carry the OS write allowlist."
                .into(),
            2,
        ));
    }
    if harness != "claude" {
        return Err((
            format!(
                "--sandbox-write-policy composes into the claude --settings payload; harness {} would carry the flag and enforce nothing. Drop the flag or spawn claude.",
                crate::claude_ask::py_repr(harness)
            ),
            2,
        ));
    }
    let block = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|payload| payload.get("sandbox").cloned())
        .filter(|block| block.is_object() && truthy(block.get("enabled")));
    block.ok_or_else(|| {
        (
            format!("sandbox write policy {path} carries no enabled sandbox block"),
            2,
        )
    })
}

/// Python truthiness of one JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The step between the route and the gate: `--print` renders and answers
/// its exit, and a sandbox block writes the settings file the claude thread
/// owes it. A routed spawn rewrites its route's file with the block folded in
/// and answers `None`; an unrouted one answers its own file's path.
pub fn pre_gate(
    params: &Value,
    harness: &str,
    name: &str,
    route: Option<&mut Route>,
    cwd: &Path,
) -> Result<Option<String>, i32> {
    if params.get("print").and_then(Value::as_bool) == Some(true) {
        return Err(print_line(params, harness, name, route.as_deref(), cwd));
    }
    let Some(sandbox) = params.get("sandbox").filter(|v| v.is_object()).cloned() else {
        return Ok(None);
    };
    let env: Map<String, Value> = route
        .as_ref()
        .map(|r| {
            r.env
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect()
        })
        .unwrap_or_default();
    let written =
        crate::route_settings::write_composed(&env, &Map::new(), Some(sandbox), None, cwd)
            .map_err(|e| {
                eprintln!("fno agents spawn: {e}");
                2
            })?;
    match (route, written) {
        (Some(route), Some(path)) => {
            route.settings = path;
            Ok(None)
        }
        (_, written) => Ok(written),
    }
}

/// The refusal reasons a `--wait` spawn outlasts by retrying, the gate's own
/// capacity verdicts. Dual with `spawn_gate.py`'s set until its one Python
/// reader (`cmd_spawn`) is deleted; `bare_spawn_parity.rs` pins the two.
pub const WAITABLE_REFUSAL_REASONS: &[&str] = &[
    "ram_floor",
    "swap_pressure",
    "cpu_instrument_unreadable",
    "cpu_share_undecidable",
    "fleet_cpu_share",
    "provider_cap",
    "max_live",
    "no_wait",
    "no_wait_mutex_held",
];

/// Run the gate, retrying a waitable refusal until `wait_secs` runs out. A
/// note prints on the first retry and then at most once a minute; each sleep
/// is `min(10s, left)`. With no wait, the first verdict stands.
pub fn gate_with_wait<G>(
    wait_secs: Option<f64>,
    mut run: impl FnMut() -> Result<G, Refusal>,
) -> Result<G, Refusal> {
    let deadline = wait_secs.map(|s| Instant::now() + Duration::from_secs_f64(s.max(0.0)));
    let mut last_note: Option<Instant> = None;
    loop {
        let refusal = match run() {
            Ok(guard) => return Ok(guard),
            Err(refusal) => refusal,
        };
        let reason = refusal
            .receipt
            .as_ref()
            .and_then(|r| r.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let now = Instant::now();
        let Some(deadline) = deadline else {
            return Err(refusal);
        };
        if !WAITABLE_REFUSAL_REASONS.contains(&reason.as_str()) || now >= deadline {
            return Err(refusal);
        }
        let left = deadline - now;
        if last_note.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(60)) {
            eprintln!(
                "spawn-gate note: {reason}; --wait retries for {}s more",
                left.as_secs()
            );
            last_note = Some(now);
        }
        std::thread::sleep(left.min(Duration::from_secs(10)));
    }
}

/// `--print`: the native line a routed claude thread would exec, shell-quoted
/// on one stdout line. Launches nothing. A refusal prints the line the Python
/// door relayed from the overlay and exits 2.
fn print_line(params: &Value, harness: &str, name: &str, route: Option<&Route>, cwd: &Path) -> i32 {
    let substrate = match params.get("substrate").and_then(Value::as_str) {
        Some("thread") => "bg",
        other => other.unwrap_or(""),
    };
    let route_env: Map<String, Value> = route
        .map(|r| {
            r.env
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect()
        })
        .unwrap_or_default();
    let ask = json!({
        "kind": "print",
        "harness": harness,
        "substrate": substrate,
        "name": name,
        "payload": params.get("message").and_then(Value::as_str).unwrap_or(""),
        "model": route.map(|r| r.model.as_str()).unwrap_or(""),
        "effort": route.and_then(|r| r.effort.as_deref()).unwrap_or(""),
        "permission_mode": "",
        "route_env": route_env,
        "account_env": account_env(params, harness),
        "cwd": cwd.to_string_lossy(),
    });
    match crate::spawn_overlay::resolve(ask) {
        Ok(answer) => {
            let tokens: Vec<String> = answer
                .get("tokens")
                .and_then(Value::as_array)
                .map(|t| {
                    t.iter()
                        .filter_map(Value::as_str)
                        .map(crate::client_commands::shlex_quote)
                        .collect()
                })
                .unwrap_or_default();
            println!("{}", tokens.join(" "));
            0
        }
        Err(msg) => {
            eprintln!("fno-agents spawn-overlay exited 2: spawn-overlay: {msg}");
            2
        }
    }
}

/// The spawn's account overlay for the print ask, as the account seam
/// resolved it before the gate (a refusal already exited there). Only the
/// print, which launches nothing, may resolve it a second time.
fn account_env(params: &Value, harness: &str) -> Map<String, Value> {
    match crate::accounts::launch::pane_overlay(params, harness) {
        Ok(Some(applied)) => applied.env,
        _ => Map::new(),
    }
}

/// The native lane: a routed claude thread with no promotion, no fenced
/// passthrough and no monitor execs the lane's own line through the
/// spawn-overlay launch kind. `None` means the spawn keeps the dispatcher
/// path: the shape does not qualify, or no launcher session names the parent
/// (the note says so).
pub fn native_lane(params: &Value, name: &str, route: Option<&Route>, cwd: &Path) -> Option<i32> {
    let route = route?;
    let message = params.get("message").and_then(Value::as_str).unwrap_or("");
    let has = |key: &str| {
        params.get(key).is_some_and(|v| match v {
            Value::Array(items) => !items.is_empty(),
            Value::Null => false,
            _ => true,
        })
    };
    if has("promote") || has("harness_args") || has("monitor") {
        return None;
    }
    let (session, harness, launcher_cwd) = crate::claims::ambient_parent_edge();
    let Some(session) = session else {
        eprintln!("spawn: the native lane needs this session's own identity (the launch record names the parent); a spawn from outside any session keeps the dispatcher path.");
        return None;
    };
    let permission = if crate::spawn_admission::is_command_seed(Some(message)) {
        crate::backlog::advance_dispatch::SPAWN_PERMISSION_BUILTIN
    } else {
        ""
    };
    let mut ask = json!({
        "kind": "launch",
        "harness": "claude",
        "name": name,
        "payload": message,
        "route_env": route.env,
        // The account seam already folded the overlay into this env, which
        // the launch and its settings file inherit; a second resolve would
        // drop a carrier-only spawn's launch-account stamp.
        "account_env": {},
        "sandbox": params.get("sandbox").cloned().unwrap_or(Value::Null),
        "model": route.model,
        "effort": route.effort.as_deref().unwrap_or(""),
        "permission_mode": permission,
        "cwd": cwd.to_string_lossy(),
        "provider": route.provider,
        "node": params.get("node").and_then(Value::as_str).unwrap_or(""),
        "launcher_session": session,
    });
    if let Some(h) = harness {
        ask["launcher_harness"] = Value::String(h);
    }
    if let Some(c) = launcher_cwd.filter(|c| !c.trim().is_empty()) {
        ask["launcher_cwd"] = Value::String(c);
    }
    let answer = match crate::launch_exec::launch_kind(&ask) {
        Ok(answer) => answer,
        Err(msg) => {
            eprintln!("spawn-overlay: {msg}");
            return Some(2);
        }
    };
    println!("{answer}");
    let code = answer.get("exit").and_then(Value::as_i64).unwrap_or(0) as i32;
    if code == 0 {
        eprintln!(
            "spawn: {name} launched on the native lane; the row lands at its SessionStart adopt"
        );
    }
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn flags_parse_and_the_substrate_settles() {
        let mut p = Map::new();
        let mut rest = vec!["5m".to_string()].into_iter();
        take_flag("--wait", &mut rest, &mut p).unwrap();
        take_flag("--mux-session=s", &mut std::iter::empty(), &mut p).unwrap();
        take_flag("--print", &mut std::iter::empty(), &mut p).unwrap();
        assert_eq!(p["wait"], "5m");
        assert_eq!(p["session"], "s");
        assert_eq!(p["print"], true);
        assert!(owns("--prompt-file=-") && !owns("--bogus"));
        let err = take_flag("--prompt-file", &mut std::iter::empty(), &mut p).unwrap_err();
        assert_eq!(err, "--prompt-file needs a value");
        let mut p = Map::new();
        settle_substrate(&mut p);
        assert_eq!(p["substrate"], "thread");
        let mut p = params(&[("provider", json!("gemini"))]);
        settle_substrate(&mut p);
        assert_eq!(p["substrate"], "pane");
        let mut p = params(&[("once", json!(true)), ("substrate", json!("pane"))]);
        settle_substrate(&mut p);
        assert_eq!(p["substrate"], "headless");
        let mut p = params(&[("substrate", json!("pane"))]);
        settle_substrate(&mut p);
        assert_eq!(p["substrate"], "pane");
    }

    #[test]
    fn admission_refuses_in_the_python_words_and_wait_retries() {
        let mut p = params(&[
            ("provider", json!("claude")),
            ("substrate", json!("headless")),
            ("wait", json!("5m")),
            ("no_wait", json!(true)),
        ]);
        assert_eq!(
            admit(&mut p),
            Err((
                "error: --wait and --no-wait are mutually exclusive".into(),
                2
            ))
        );
        let mut p = params(&[
            ("provider", json!("claude")),
            ("substrate", json!("headless")),
            ("wait", json!("1.5m")),
        ]);
        admit(&mut p).unwrap();
        assert_eq!(p["no_wait"], true);
        assert_eq!(p["wait_secs"], 90.0);
        let mut p = params(&[
            ("provider", json!("claude")),
            ("substrate", json!("headless")),
            ("wait", json!("9".repeat(400))),
        ]);
        assert!(admit(&mut p)
            .unwrap_err()
            .0
            .starts_with("error: --wait wants"));
        let mut p = params(&[
            ("provider", json!("claude")),
            ("substrate", json!("headless")),
            ("session", json!("s")),
        ]);
        assert_eq!(
            admit(&mut p).unwrap_err().0,
            "--mux-session needs --as pane; only a pane has a mux session to spawn into"
        );
        let refusal = |reason: &str| Refusal {
            exit_code: 3,
            receipt: Some(json!({ "reason": reason })),
            event: Map::new(),
        };
        let mut calls = 0;
        let out: Result<(), Refusal> = gate_with_wait(Some(0.0), || {
            calls += 1;
            Err(refusal("max_live"))
        });
        assert_eq!(out.unwrap_err().exit_code, 3);
        assert_eq!(calls, 1, "a spent deadline stops at once");
        let mut calls = 0;
        let out: Result<(), Refusal> = gate_with_wait(Some(30.0), || {
            calls += 1;
            Err(refusal("policy"))
        });
        assert!(out.is_err());
        assert_eq!(calls, 1, "a non-waitable reason never retries");
    }
}
