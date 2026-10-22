//! The billing axes of the spawn seam: config-sourced route, account and
//! model, decided in one place. This module answers the spawn compose's
//! axis-region asks (route/account/model): the Python front door projects the
//! caller's facts and the config axes' values plus rungs, this module
//! returns the injections, receipts and skip reasons verbatim, and the seam
//! only applies them. The message strings are
//! the seam's own vocabulary - one spelling, read by tests and operators
//! alike. Pure over its input: no config, filesystem or network reads.

use serde_json::{json, Map, Value};

/// One config axis as the payload carries it: `{"value": ..., "rung": ...}`.
struct Axis {
    value: String,
    rung: String,
}

fn axis(payload: &Value, key: &str) -> Axis {
    let a = payload.get(key).cloned().unwrap_or(json!({}));
    Axis {
        value: a
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        rung: a
            .get("rung")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

fn flag(payload: &Value, key: &str) -> bool {
    payload.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn opt_str(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_default())
}

/// Python `repr()` for a str: single quotes by default, double quotes when
/// the value contains a single quote and no double quote, backslashes and
/// control characters escaped. The receipts quote operator-typed values, so
/// the spelling must match what the Python seam used to print.
pub(crate) fn repr(s: &str) -> String {
    let has_sq = s.contains('\'');
    let has_dq = s.contains('"');
    let (open, close) = if has_sq && !has_dq {
        ('"', '"')
    } else {
        ('\'', '\'')
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(open);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(close);
    out
}

/// The decision over the three billing axes. Inputs are facts the seam
/// already resolved (explicit flags, the slot walk's candidate, harness
/// answers); outputs are argv tokens and receipt rows the seam applies
/// mechanically, in the seam's own field order.
pub fn decide(payload: &Value) -> Value {
    let route = axis(payload, "route");
    let account = axis(payload, "account");
    let explicit_route = flag(payload, "explicit_route");
    let grid_candidate_present = flag(payload, "grid_candidate_present");
    let slot_chain: Vec<String> = payload
        .get("slot_chain")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    let prov = payload.get("prov").and_then(Value::as_str).unwrap_or("");

    let mut inject: Vec<Value> = Vec::new();
    let mut applied: Vec<Value> = Vec::new();
    let mut suppressed: Vec<Value> = Vec::new();
    let mut messages: Vec<String> = Vec::new();
    let mut route_injected = false;
    let mut out_tail_append_empty = false;
    let mut bundle_inject: Vec<Value> = Vec::new();

    // --- route ---------------------------------------------------------- //
    if !route.value.is_empty() && !explicit_route && !grid_candidate_present {
        inject.push(json!(["--route", route.value]));
        applied.push(json!([
            "route",
            route.value,
            format!("{}.route", route.rung),
        ]));
        route_injected = true;
    } else if !route.value.is_empty() {
        // AC9-UI: config-sourced routing is never invisible. The route axis
        // is the one that bills, so a dropped route names which condition
        // fired. The grid case is named before any --model read: a grid
        // candidate excludes an explicit -m anyway.
        let why = if explicit_route {
            "the caller passed --route".to_string()
        } else if grid_candidate_present {
            let chain = if slot_chain.is_empty() {
                "no reason recorded".to_string()
            } else {
                slot_chain.join("; ")
            };
            format!("the capacity grid chose a lane ({chain})")
        } else {
            "no suppression reason recorded".to_string()
        };
        // The caller's own route bills and the "caller default" tail would be
        // false.
        let caller_route_shaped = explicit_route;
        let tail = if caller_route_shaped {
            "your own route bills this worker"
        } else {
            "this worker bills at the caller default"
        };
        messages.push(format!(
            "fno agents spawn: route skipped ({why}); {}.route {} NOT applied - {tail}",
            route.rung,
            repr(&route.value),
        ));
        suppressed.push(json!(["route", route.value, route.rung, why,]));
    }
    // --- account -------------------------------------------------------- //
    let grid_account_injected = flag(payload, "grid_account_injected");
    let account_flag_present = flag(payload, "account_flag_present");
    if !account.value.is_empty() && !grid_account_injected && !account_flag_present {
        // Accounts are Claude-only (cmd_spawn rejects --account on any other
        // harness), so a configured account must not follow an explicit
        // non-Claude harness.
        if prov == "claude" {
            inject.push(json!(["--account", account.value]));
            applied.push(json!([
                "account",
                account.value,
                format!("{}.account", account.rung),
            ]));
        } else {
            // AC9-UI: never silently drop the pin on a Claude-to-Codex
            // cutover - name the condition the way substrate/permission do.
            messages.push(format!(
                "fno agents spawn: account skipped (accounts are claude-only, resolved \
                 provider {}); {}.account {} ignored",
                repr(prov),
                account.rung,
                repr(&account.value),
            ));
            suppressed.push(json!([
                "account",
                account.value,
                account.rung,
                format!("accounts are claude-only; resolved provider {}", repr(prov)),
            ]));
        }
    }

    // --- pane group ----------------------------------------------------- //
    // The placement judgment itself is the spawn-overlay command's; this only
    // decides what the seam does with its answer. An unavailable overlay
    // degrades open with a named line, never failing the spawn.
    let pane_group = axis(payload, "pane_group");
    let tab_flag_present = flag(payload, "tab_flag_present");
    if !pane_group.value.is_empty() && !tab_flag_present {
        let pg_rung = payload
            .get("pane_group_pg_rung")
            .and_then(Value::as_str)
            .unwrap_or("");
        if let Some(exc) = opt_str(payload, "pane_group_unavailable").filter(|s| !s.is_empty()) {
            messages.push(format!(
                "fno agents spawn: pane group skipped ({exc}); {pg_rung} ignored"
            ));
            suppressed.push(json!([
                "pane_group",
                pane_group.value,
                pane_group.rung,
                exc
            ]));
        } else {
            let answer = payload
                .get("pane_group_answer")
                .cloned()
                .unwrap_or(json!({}));
            // The overlay's "inject" is a boolean or its suggested token
            // list: any non-empty, non-false value means the group applies.
            let injects = match answer.get("inject") {
                Some(Value::Bool(b)) => *b,
                Some(Value::Array(a)) => !a.is_empty(),
                Some(Value::Null) | None => false,
                Some(_) => true,
            };
            if injects {
                inject.push(json!(["--tab", pane_group.value]));
                applied.push(json!(["tab", pane_group.value, pg_rung]));
            } else if let Some(skipped) = answer
                .get("skipped")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                messages.push(skipped.to_string());
                suppressed.push(json!([
                    "pane_group",
                    pane_group.value,
                    pane_group.rung,
                    skipped,
                ]));
            }
        }
    }

    // --- harness bundle ------------------------------------------------- //
    // The overlay's ONE bundle answer (lane args > profile harness overlay >
    // defaults harness overlay, never concatenated) lands behind the --
    // passthrough fence; a boundary the caller already typed displaces it.
    let bundle_json = payload.get("bundle");
    if bundle_json.map(|b| b.is_object()).unwrap_or(false)
        && bundle_json
            .as_ref()
            .and_then(|b| b.get("displaced"))
            .map(|d| !d.is_null())
            .unwrap_or(false)
    {
        let displaced = bundle_json
            .as_ref()
            .and_then(|b| b.get("displaced"))
            .unwrap();
        let boundary = displaced
            .get("boundary")
            .and_then(Value::as_str)
            .unwrap_or("");
        let rung = displaced.get("rung").and_then(Value::as_str).unwrap_or("");
        messages.push(format!(
            "fno agents spawn: harness args skipped (argv already carries a \
             {boundary} passthrough); {rung} ignored"
        ));
    } else if let Some(tokens) = bundle_json
        .as_ref()
        .and_then(|b| b.get("tokens"))
        .and_then(Value::as_array)
    {
        let token_strs: Vec<String> = tokens
            .iter()
            .map(|a| a.as_str().unwrap_or("").to_string())
            .collect();
        let rung = bundle_json
            .as_ref()
            .and_then(|b| b.get("rung"))
            .and_then(Value::as_str)
            .unwrap_or("");
        // click fills positionals in order, so a spawn with no message would
        // eat the bundle's first token as MESSAGE; an explicit empty keeps
        // the slot reserved for the prompt.
        if !flag(payload, "positional_present") {
            out_tail_append_empty = true;
        }
        let mut bundle_tokens: Vec<String> = vec!["--".to_string()];
        bundle_tokens.extend(token_strs.iter().cloned());
        bundle_inject.push(json!(bundle_tokens));
        applied.push(json!(["args", token_strs.join(" "), rung]));
        messages.push(format!(
            "fno agents spawn: bundle {rung} applied unverified (fno reads no \
             effective harness config; confirm on the worker receipt)"
        ));
    }

    let mut out = Map::new();
    out.insert("out_tail_append_empty".into(), json!(out_tail_append_empty));
    out.insert("bundle_inject".into(), json!(bundle_inject));
    out.insert("inject".into(), json!(inject));
    out.insert("applied".into(), json!(applied));
    out.insert("suppressed".into(), json!(suppressed));
    out.insert("messages".into(), json!(messages));
    out.insert("route_injected".into(), json!(route_injected));
    Value::Object(out)
}

/// The `removal_accounting` field's answer: run the write choke point's
/// removal accounting for the Python write primitive. The before-rows ride
/// the payload (Python's raw pre-write snapshot); the after-rows read from
/// the registry on disk, which the write has already persisted. The existing
/// accounting stages the receipts and emits the events; the answer names the
/// count so the caller's audit trail reads one line.
pub fn removal_accounting_decide(ask: &Value) -> Value {
    let path = std::path::PathBuf::from(ask.get("registry").and_then(Value::as_str).unwrap_or(""));
    if path.as_os_str().is_empty() {
        return serde_json::json!({ "refused": "registry path is required" });
    }
    let before: Vec<crate::state::RegistryEntry> = ask
        .get("before")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| serde_json::from_value(row.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let removed = crate::state::load_registry(&path)
        .map(|registry| crate::state::account_for_removed_rows(&path, &before, &registry.entries))
        .unwrap_or(0);
    serde_json::json!({ "removed": removed })
}

/// The `reentry_mechanism` field's answer: which relaunch a Resume resolves
/// to for the named row, or the refusal naming why the wake may not respawn.
pub fn reentry_mechanism_decide(ask: &Value) -> Value {
    let name = ask.get("name").and_then(Value::as_str).unwrap_or("");
    match crate::reentry::resolve_reentry(
        &crate::paths::AgentsHome::from_env().registry_json(),
        name,
        crate::reentry::ReentryTransition::Resume,
        None,
        None,
    ) {
        Ok(plan) => serde_json::json!({ "mechanism": plan.mechanism }),
        Err(reason) => serde_json::json!({ "refused": reason }),
    }
}

/// The `wake_name` field's answer: the identity a wake fork carries, read
/// from the registry beside the reentry plan (same field-on-a-command shape).
/// An unreadable registry degrades to an empty one, so the full rung chain
/// (row, tombstone, transcript, alias) still answers: a wake must never
/// block mail on registry state.
pub fn wake_name_decide(ask: &Value) -> Value {
    let session_id = ask.get("session_id").and_then(Value::as_str).unwrap_or("");
    if session_id.trim().is_empty() {
        return serde_json::json!({ "refused": "session_id is required" });
    }
    let home = crate::paths::AgentsHome::from_env();
    let registry_path = home.registry_json();
    let registry = crate::state::load_registry(&registry_path).unwrap_or_default();
    let carried = crate::reentry::revival_identity(&registry, &registry_path, session_id);
    let mut answer = serde_json::json!({ "name": carried.name });
    for (key, value) in [
        ("fno_id", &carried.fno_id),
        ("spawned_by_session", &carried.spawned_by_session),
        ("spawned_by_harness", &carried.spawned_by_harness),
        ("spawned_by_cwd", &carried.spawned_by_cwd),
        ("node", &carried.node),
    ] {
        if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
            answer[key] = serde_json::json!(v);
        }
    }
    answer
}

/// The permission mode a fleet pane spec carries. An explicit ask wins. A
/// non-empty `fno_id` is the fleet-spawn proof: the spawner assigns it and
/// bare `footnote` in the user's own terminal never sends one, so only a
/// fleet pane gets the operator's spawn default, else fno's builtin
/// unattended answer (the fleet cannot answer a footnote ask). Without an
/// `fno_id` the spec carries no mode and the user's own settings decide.
fn fleet_pane_mode(ask: &Value, cwd: &std::path::Path) -> Option<String> {
    fleet_pane_mode_decided(
        ask.get("permission_mode").and_then(Value::as_str),
        ask.get("fno_id").and_then(Value::as_str),
        || {
            crate::agents_config::config_lookup(cwd, &["agents", "defaults", "permission_mode"])
                .and_then(|v| v.as_str().map(str::to_string))
        },
    )
}

/// The decision core, with the config read injected so the test needs no disk.
fn fleet_pane_mode_decided(
    explicit: Option<&str>,
    fno_id: Option<&str>,
    config: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if let Some(mode) = explicit.map(str::trim).filter(|m| !m.is_empty()) {
        return Some(mode.to_string());
    }
    fno_id.filter(|id| !id.is_empty())?;
    Some(
        config()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                crate::backlog::advance_dispatch::SPAWN_PERMISSION_BUILTIN.to_string()
            }),
    )
}

/// The command entry: JSON payload on stdin, decision JSON on stdout (the
/// spawn-overlay shape). Exit 0 even for a "no axes" answer; exit 2 only for
/// transport-level faults (unreadable payload), which the caller reports as
/// the owner being unavailable.
pub fn run_spawn_axes(args: &[String]) -> i32 {
    use std::io::Read;

    let mut payload = String::new();
    let read = if let Some(path) = args.iter().find_map(|a| a.strip_prefix("--payload-file=")) {
        std::fs::read_to_string(path)
    } else {
        std::io::stdin()
            .read_to_string(&mut payload)
            .map(|_| payload.clone())
    };
    let payload = match read {
        Ok(text) => text,
        Err(e) => {
            eprint!("spawn-axes: cannot read payload: {e}\n");
            return 2;
        }
    };
    let parsed: Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => {
            eprint!("spawn-axes: bad payload: {e}\n");
            return 2;
        }
    };
    // A `dispatch_credit` field answers who answers for a node dispatch:
    // the covering role owner plus, when the role row carries a live
    // session, the lead to mail at launch (same field-on-a-command shape as
    // node_seed).
    if let Some(ask) = parsed.get("dispatch_credit") {
        println!("{}", crate::dispatch_credit::covering_role(ask));
        return 0;
    }
    // A `launch_credit_mail` ask delivers the one launch notice to the
    // covering lead. Best-effort: the answer names a delivery fault, and
    // the caller never fails a launch over it.
    if let Some(ask) = parsed.get("launch_credit_mail") {
        println!("{}", crate::dispatch_credit::launch_credit_mail(ask));
        return 0;
    }
    if let Some(seed) = parsed.get("node_seed") {
        println!("{}", crate::node_seed::decide(seed));
        return 0;
    }
    // A `spawn_node` field answers which node the spawn works, with the
    // source that named it (same field-on-a-command shape as node_seed).
    if let Some(ask) = parsed.get("spawn_node") {
        println!("{}", crate::node_seed::resolve_node(ask));
        return 0;
    }
    // A `keeper_posture` field asks the agy_launch owner which permission
    // tokens a launch carries (same field-on-a-command shape as node_seed).
    if let Some(ask) = parsed.get("keeper_posture") {
        let answer = match crate::agy_launch::keeper_posture(
            ask.get("harness").and_then(Value::as_str).unwrap_or(""),
            ask.get("lane").and_then(Value::as_str).unwrap_or("pane"),
        ) {
            Ok(p) => serde_json::json!({
                "tokens": p.tokens,
                "effective": p.effective,
                "source": p.source,
                "note": p.note,
            }),
            Err(reason) => serde_json::json!({"refused": reason}),
        };
        println!("{answer}");
        return 0;
    }
    // A `resume_pin` field routes the same way: which model a resume comes
    // back on, answered by crates/fno-agents/src/resume_pin.rs.
    if let Some(pin) = parsed.get("resume_pin") {
        println!("{}", crate::resume_pin::decide(pin));
        return 0;
    }
    // A `reentry_mechanism` field asks the reentry resolver which relaunch a
    // wake may take (the same field-on-a-command shape). It calls the
    // registry-writing wrapper, so a route the resolver recovered from a
    // bare row's transcript is also persisted here, the same as any door.
    if let Some(ask) = parsed.get("reentry_mechanism") {
        println!("{}", reentry_mechanism_decide(ask));
        return 0;
    }
    // A `wake_name` field asks which identity a wake fork carries: the
    // session's last recorded registry name, else the wake- alias (same
    // field-on-a-command shape).
    if let Some(ask) = parsed.get("wake_name") {
        println!("{}", wake_name_decide(ask));
        return 0;
    }
    // A `reap_receipt` field asks the receipt builder for the Python
    // registry choke point's removal receipt (same field-on-a-command shape).
    if let Some(ask) = parsed.get("reap_receipt") {
        println!("{}", crate::receipt::decide_reap_receipt(ask));
        return 0;
    }
    // A `removal_accounting` field runs the registry write choke point's
    // removal accounting for the Python write primitive (same
    // field-on-a-command shape): the before-rows ride the payload, the
    // after-rows read from the registry on disk, and the existing
    // accounting stages the receipts and emits the events.
    if let Some(ask) = parsed.get("removal_accounting") {
        println!("{}", removal_accounting_decide(ask));
        return 0;
    }
    // A `pi_session_lookup` field asks the pi store owner what a
    // (cwd, session id) pair resolves to right now (same field-on-a-command
    // shape as node_seed).
    if let Some(ask) = parsed.get("pi_session_lookup") {
        use std::path::PathBuf;
        let cwd = PathBuf::from(ask.get("cwd").and_then(Value::as_str).unwrap_or(""));
        let session_id = ask.get("session_id").and_then(Value::as_str).unwrap_or("");
        // An unnamed cwd never touches the filesystem: resolving the store
        // against "" would read the process cwd's own project settings and
        // answer about a directory nobody asked about.
        if cwd.as_os_str().is_empty() {
            let answer = serde_json::json!({
                "state": "unknown",
                "files": [],
                "directory": "",
                "reason": "cwd is required",
            });
            println!("{answer}");
            return 0;
        }
        let (lookup, directory) = match crate::pi::pi_store(&cwd) {
            Ok(store) => {
                let dir = match store.layout {
                    crate::pi::StoreLayout::CwdScoped => {
                        store.root.join(crate::pi::encode_cwd(&cwd))
                    }
                    crate::pi::StoreLayout::Flat => store.root.clone(),
                };
                (crate::pi::lookup_sessions_in(&store, &cwd, session_id), dir)
            }
            Err(reason) => (
                crate::pi::SessionLookup::Unknown {
                    dir: PathBuf::new(),
                    reason,
                },
                PathBuf::new(),
            ),
        };
        let (state, files, reason) = match &lookup {
            crate::pi::SessionLookup::Unknown { reason, .. } => {
                ("unknown", Vec::new(), reason.clone())
            }
            crate::pi::SessionLookup::None => ("none", Vec::new(), String::new()),
            crate::pi::SessionLookup::One { file } => ("one", vec![file.clone()], String::new()),
            crate::pi::SessionLookup::Duplicate { files } => {
                ("duplicate", files.clone(), String::new())
            }
        };
        let answer = serde_json::json!({
            "state": state,
            "files": files,
            "directory": directory,
            "reason": reason,
        });
        println!("{answer}");
        return 0;
    }
    // A `pi_route` field asks the pi route owner which provider, model and
    // effort tokens a launch carries (same field-on-a-command shape).
    if let Some(ask) = parsed.get("pi_route") {
        let s = |k: &str| ask.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let route = crate::pi::pi_route(&s("model"), &s("effort"), &s("tools"), &s("deny_tools"));
        let answer = serde_json::json!({
            "tokens": route.tokens,
            "route_source": route.route_source,
            "note": route.note,
        });
        println!("{answer}");
        return 0;
    }
    // A `footnote_pane_spec` field asks the footnote harness owner to write
    // the launch spec a pane's TUI reads, and answers the file's path and the
    // session id it names.
    if let Some(ask) = parsed.get("footnote_pane_spec") {
        let s = |k: &str| ask.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
        let cwd = std::path::Path::new(s("cwd").unwrap_or("."));
        let answer = match crate::footnote_harness::write_pane_spec(
            cwd,
            s("model").unwrap_or(""),
            s("node"),
            fleet_pane_mode(ask, cwd).as_deref(),
            s("fno_id"),
        ) {
            Ok((path, fno_id)) => serde_json::json!({"path": path, "fno_id": fno_id}),
            Err(reason) => serde_json::json!({"refused": reason}),
        };
        println!("{answer}");
        return 0;
    }
    // The fallthrough WAS the axis composition (config --model/--effort/
    // --substrate/--permission-mode injection); it is retired with the flags.
    // A stale caller reads the refusal and stops; the field asks above are
    // the command's live surface.
    println!(
        "{}",
        serde_json::json!({
            "refused": "the spawn-axis composition is retired; the lane template owns the launch line (docs/HARNESSES.md)"
        })
    );
    0
}

/// One role flag the client parses into spawn params: typed here
/// so a promoted codex thread row is promoted AT MINT - the seed turn
/// enqueues inside the lane and must never submit to an unpromoted row.
/// The level bound is the type the registry row stores (a u32 0..=2);
/// territory and succession policy live at the Python seam.
pub fn insert_role_flag(
    flag: &str,
    args: &mut impl Iterator<Item = String>,
    params: &mut serde_json::Map<String, Value>,
) -> Result<(), String> {
    match flag {
        "--role-level" => {
            let raw = args.next().ok_or("--role-level needs a value")?;
            let level = raw
                .parse::<u32>()
                .ok()
                .filter(|l| *l <= 2)
                .ok_or("--role-level takes an integer level 0-2")?;
            params.insert("role_level".into(), Value::from(level));
        }
        "--role-scope" => {
            let scope = args.next().ok_or("--role-scope needs a value")?;
            params.insert("role_scope".into(), Value::from(scope));
        }
        other => return Err(format!("unknown role flag: {other}")),
    }
    Ok(())
}

/// How a non-pane `--account` spawn proceeds on the Rust arm.
pub enum AccountSeam {
    /// No `--account` on the spawn; nothing to decide.
    None,
    /// The seam resolved this very account and stamped it, so the overlay it
    /// applied is already in this process's env; launch natively.
    Applied(String),
    /// The account axis refuses this spawn outright; the line carries the
    /// receipt (the cli.py claude-only text, Python repr quoted).
    Refuse(String),
    /// The seam did not stamp the requested account (no stamp, or another
    /// id), so the overlay is unapplied or stale: re-exec the Python front
    /// door, whose `cmd_spawn` resolves the overlay and owns the refusals.
    NeedsPython,
}

/// Decide how an `--account` spawn proceeds, from three facts and nothing
/// global, so every answer is unit-testable. The overlay decision itself
/// stays in Python (`account_env.resolve_account_overlay`); this only reads
/// the stamp the seam publishes after a successful resolution
/// (`FNO_LAUNCH_ACCOUNT`) and refuses what Python would refuse before any
/// resolution: an account rides the claude binary, so a non-claude harness
/// is a refusal even when a stamp names the request.
pub fn account_seam(account: Option<&str>, provider: &str, stamp: Option<&str>) -> AccountSeam {
    let Some(account) = account else {
        return AccountSeam::None;
    };
    if account.is_empty() {
        // Python reads an empty flag value as an account spawn it must
        // resolve (and refuse), so the old re-exec always handed it there;
        // an empty value goes back too, and an empty stamp can never match.
        return AccountSeam::NeedsPython;
    }
    if provider != "claude" {
        return AccountSeam::Refuse(format!(
            "--account is claude-only; got harness {}",
            crate::claude_ask::py_repr(provider)
        ));
    }
    if stamp == Some(account) {
        AccountSeam::Applied(account.to_string())
    } else {
        AccountSeam::NeedsPython
    }
}

/// Apply the account verdict for a non-pane spawn: `Refuse` prints the
/// receipt and exits 2; `NeedsPython` re-execs the Python front door on the
/// unmarked argv (the Python CLI has no --defaults-applied flag) and exits
/// 127 when that exec fails. `None` and `Applied` launch natively. The
/// stamp is read HERE, not inside `account_seam`, so the verdict stays
/// pure and every answer stays unit-testable.
pub fn account_seam_exit(
    account: Option<&str>,
    provider: &str,
    args: &[String],
    exec_python: impl Fn(&[String]) -> std::io::Error,
) -> Option<i32> {
    let stamp = std::env::var(crate::state::LAUNCH_ACCOUNT_ENV_KEY).ok();
    match account_seam(account, provider, stamp.as_deref()) {
        AccountSeam::None | AccountSeam::Applied(_) => None,
        AccountSeam::Refuse(line) => {
            eprintln!("{line}");
            Some(2)
        }
        AccountSeam::NeedsPython => {
            let stripped: Vec<String> = args
                .iter()
                .filter(|a| !a.starts_with("--defaults-applied"))
                .cloned()
                .collect();
            let err = exec_python(&stripped);
            eprintln!(
                "fno-agents: --account needs the Python CLI to resolve, but \
                 exec of 'fno agents spawn' failed: {err}. Run `fno agents \
                 spawn ...` directly."
            );
            Some(127)
        }
    }
}

/// The two remaining spawn-head flags the binary owns end to end (row 9):
/// the typed parse beside `insert_role_flag`, then the two verdicts. Both
/// verdicts print the Python door's line bare, so both doors teach one
/// owner text until row 10 retires the Python stubs (d-19004329).
pub fn insert_spawn_flag(
    flag: &str,
    args: &mut impl Iterator<Item = String>,
    params: &mut serde_json::Map<String, Value>,
) -> Result<(), String> {
    match flag {
        "--output-format" => {
            let v = args.next().ok_or("--output-format needs a value")?;
            params.insert("output_format".into(), Value::from(v));
        }
        "--monitor" => {
            let v = args.next().ok_or("--monitor needs a value")?;
            params.insert("monitor".into(), Value::from(v));
        }
        other => return Err(format!("unknown spawn flag: {other}")),
    }
    Ok(())
}

/// The Python door's refusal lines, pinned as constants (plan change 1):
/// `agents/cli.py` output-format gate and `spawn_defaults.resolve_spawn_gates`.
/// The zai line is unreachable at this door (every valid pane monitor spawn
/// stays Python on the pane predicate), pinned so the parity stays one grep
/// away.
pub const OUTPUT_FORMAT_GOLDEN: &str =
    "--output-format supports only 'json' on claude headless spawns";
pub const MONITOR_VALUE_GOLDEN: &str = "--monitor must be 'happy'";
pub const MONITOR_PANE_GOLDEN: &str = "--monitor happy needs --as pane; thread and headless workers do not pass the happy launcher seam";
pub const MONITOR_CLAUDE_GOLDEN: &str = "--monitor happy requires the claude harness; got harness ";
pub const MONITOR_ZAI_GOLDEN: &str = "--monitor happy currently supports only the zai provider";

/// The `--monitor` verdict: value gate, pane-only gate, claude-only gate, in
/// `resolve_spawn_gates`' order. `Launch` answers the one combination the
/// gate admits; the seam's pane predicate keeps that spawn Python, so this
/// door only ever fires the `Refuse` half in practice.
pub enum MonitorSeam {
    None,
    Launch,
    Refuse(String),
}

pub fn monitor_seam(monitor: Option<&str>, provider: &str, substrate: &str) -> MonitorSeam {
    let Some(v) = monitor else {
        return MonitorSeam::None;
    };
    if v != "happy" {
        return MonitorSeam::Refuse(format!("{} (got {})", MONITOR_VALUE_GOLDEN, repr(v)));
    }
    if substrate != "pane" {
        return MonitorSeam::Refuse(MONITOR_PANE_GOLDEN.to_string());
    }
    if provider != "claude" {
        return MonitorSeam::Refuse(format!("{MONITOR_CLAUDE_GOLDEN}{}", repr(provider)));
    }
    MonitorSeam::Launch
}

/// The `--output-format` verdict: admitted only for the one shape PR-watch
/// fires (claude + headless + json). `Some(v)` carries the validated value
/// to the claude headless argv builder; `Err` is the golden refusal.
pub fn output_format_seam(
    format: Option<&str>,
    provider: &str,
    substrate: &str,
) -> Result<Option<String>, String> {
    match format {
        None => Ok(None),
        Some(v) => {
            if provider != "claude" || substrate != "headless" || v != "json" {
                Err(OUTPUT_FORMAT_GOLDEN.to_string())
            } else {
                Ok(Some(v.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decide_map(payload: Value) -> Map<String, Value> {
        decide(&payload)
            .as_object()
            .cloned()
            .expect("decision is an object")
    }

    #[test]
    fn fleet_pane_mode_answers_explicit_config_builtin_or_none() {
        let cfg = || Some("default".to_string());
        // An explicit ask wins; the config read never fires.
        assert_eq!(
            fleet_pane_mode_decided(Some(" plan "), Some("u"), cfg),
            Some("plan".to_string())
        );
        // A fleet spawn (fno_id present) rides the operator default...
        assert_eq!(
            fleet_pane_mode_decided(None, Some("u"), cfg),
            Some("default".to_string())
        );
        // ...else fno's builtin unattended answer.
        assert_eq!(
            fleet_pane_mode_decided(None, Some("u"), || None),
            Some(crate::backlog::advance_dispatch::SPAWN_PERMISSION_BUILTIN.to_string())
        );
        // Bare footnote sends no fno_id: no mode, the user's settings decide.
        assert_eq!(fleet_pane_mode_decided(None, None, cfg), None);
        assert_eq!(fleet_pane_mode_decided(None, Some(""), cfg), None);
    }

    #[test]
    fn account_seam_passes_an_unaccounted_spawn_through() {
        // No --account, nothing to decide - whatever the stamp says. A stale
        // inherited stamp must never turn a plain spawn into an account one.
        assert!(matches!(
            account_seam(None, "claude", Some("leftover")),
            AccountSeam::None
        ));
    }

    #[test]
    fn account_seam_trusts_a_stamp_that_names_the_request() {
        // The seam resolved THIS account and stamped it; the applied overlay
        // is in env, so the client launches natively with no second pass.
        assert!(matches!(
            account_seam(Some("makers"), "claude", Some("makers")),
            AccountSeam::Applied(id) if id == "makers"
        ));
    }

    #[test]
    fn account_seam_sends_an_unstamped_request_back_to_python() {
        // No stamp (seam resolution failed) or a stamp naming another id
        // (parent's inherited one): trusting either would launch on an
        // unapplied or foreign overlay, so Python resolves and owns the
        // refusal receipt.
        assert!(matches!(
            account_seam(Some("makers"), "claude", None),
            AccountSeam::NeedsPython
        ));
        assert!(matches!(
            account_seam(Some("makers"), "claude", Some("other-acct")),
            AccountSeam::NeedsPython
        ));
    }

    #[test]
    fn account_seam_sends_an_empty_account_value_back_to_python() {
        // Python treats an empty flag value as an account it must resolve;
        // an empty stamp (exported empty) must never satisfy it either.
        assert!(matches!(
            account_seam(Some(""), "claude", Some("")),
            AccountSeam::NeedsPython
        ));
        assert!(matches!(
            account_seam(Some(""), "claude", None),
            AccountSeam::NeedsPython
        ));
    }

    #[test]
    fn account_seam_refuses_a_non_claude_harness() {
        // Byte-for-byte the cli.py claude-only receipt (Python repr quoted),
        // even when a stamp names the request: the refusal precedes any
        // resolution, exactly as cmd_spawn orders it.
        match account_seam(Some("makers"), "codex", Some("makers")) {
            AccountSeam::Refuse(line) => {
                assert_eq!(line, "--account is claude-only; got harness 'codex'")
            }
            _ => panic!("expected Refuse"),
        }
    }

    #[test]
    fn reentry_mechanism_answers_the_plan_mechanism() {
        // AC6-HP (Rust half): the field answers the resolver's mechanism, or
        // a refusal shaped so Python's respawn_ok can only say false.
        let _root = crate::paths::DeclaredRoot::declare("reentry_mechanism_decide");
        let refused = reentry_mechanism_decide(&json!({"name": "no-such-row"}));
        assert!(refused.get("refused").is_some(), "{refused}");
        assert!(refused.get("mechanism").is_none());
        // An unnamed row refuses the same shape - never a mechanism.
        let unnamed = reentry_mechanism_decide(&json!({}));
        assert!(unnamed.get("refused").is_some(), "{unnamed}");
    }

    #[test]
    fn route_skips_with_the_grid_reason_before_the_model_read() {
        let out = decide_map(json!({
            "route": {"value": "zai/glm", "rung": "agents.defaults"},
            "grid_candidate_present": true,
            "slot_chain": ["slot skip agents.profiles.target.lanes[0] capacity=low"],
        }));
        assert_eq!(out["inject"], json!([]));
        let msg = out["messages"][0].as_str().unwrap();
        assert!(
            msg.starts_with("fno agents spawn: route skipped (the capacity grid chose a lane (")
        );
        assert_eq!(out["suppressed"][0][3], "the capacity grid chose a lane (slot skip agents.profiles.target.lanes[0] capacity=low)");
    }

    #[test]
    fn claude_account_injects_and_non_claude_skips_by_name() {
        let out = decide_map(json!({
            "account": {"value": "readyrule", "rung": "agents.defaults"},
            "prov": "claude",
        }));
        assert_eq!(out["inject"], json!([["--account", "readyrule"]]));
        let out = decide_map(json!({
            "account": {"value": "readyrule", "rung": "agents.defaults"},
            "prov": "codex",
        }));
        assert_eq!(out["inject"], json!([]));
        assert!(out["messages"][0]
            .as_str()
            .unwrap()
            .starts_with("fno agents spawn: account skipped (accounts are claude-only, resolved provider 'codex')"));
    }

    #[test]
    fn monitor_seam_refuses_with_the_python_golden_lines() {
        // Byte-for-byte the resolve_spawn_gates receipts (spawn_defaults.py),
        // in its order: value, pane-only, claude-only.
        match monitor_seam(Some("grumpy"), "claude", "pane") {
            MonitorSeam::Refuse(line) => {
                assert_eq!(line, "--monitor must be 'happy' (got 'grumpy')")
            }
            _ => panic!("expected Refuse"),
        }
        match monitor_seam(Some("happy"), "claude", "thread") {
            MonitorSeam::Refuse(line) => assert_eq!(line, MONITOR_PANE_GOLDEN),
            _ => panic!("expected Refuse"),
        }
        match monitor_seam(Some("happy"), "codex", "pane") {
            MonitorSeam::Refuse(line) => assert_eq!(
                line,
                "--monitor happy requires the claude harness; got harness 'codex'"
            ),
            _ => panic!("expected Refuse"),
        }
        assert!(matches!(
            monitor_seam(Some("happy"), "claude", "pane"),
            MonitorSeam::Launch
        ));
        assert!(matches!(
            monitor_seam(None, "codex", "thread"),
            MonitorSeam::None
        ));
    }

    #[test]
    fn output_format_seam_admits_only_claude_headless_json() {
        // The PR-watch shape (pr_watch/_dispatch.py) is the one admitted
        // argv; every other combination refuses with the agents/cli.py line.
        assert_eq!(
            output_format_seam(Some("json"), "claude", "headless").expect("admitted"),
            Some("json".to_string())
        );
        for (provider, substrate, value) in [
            ("claude", "headless", "text"),
            ("codex", "headless", "json"),
            ("claude", "thread", "json"),
            ("claude", "pane", "json"),
        ] {
            let err = output_format_seam(Some(value), provider, substrate)
                .expect_err("the non-PR-watch shape refuses");
            assert_eq!(err, OUTPUT_FORMAT_GOLDEN);
        }
        assert_eq!(output_format_seam(None, "codex", "thread"), Ok(None));
    }

    #[test]
    fn insert_spawn_flag_parses_and_stores_both_flags() {
        // The typed parse beside insert_role_flag: value flags refuse a
        // missing value and store under the params keys the seam verdicts
        // read.
        let mut it = ["json".to_string()].into_iter();
        let mut params = Map::new();
        insert_spawn_flag("--output-format", &mut it, &mut params).expect("parses");
        assert_eq!(params["output_format"], json!("json"));
        let err = insert_spawn_flag(
            "--output-format",
            &mut Vec::<String>::new().into_iter(),
            &mut params,
        )
        .expect_err("a valueless flag refuses");
        assert!(err.contains("needs a value"), "{err}");
        let mut it = ["happy".to_string()].into_iter();
        insert_spawn_flag("--monitor", &mut it, &mut params).expect("parses");
        assert_eq!(params["monitor"], json!("happy"));
    }

    #[test]
    fn monitor_and_zai_goldens_match_the_python_door() {
        // The five pinned constants (plan change 1): agents/cli.py:1559 and
        // resolve_spawn_gates' three lines, plus the zai line cli.py pins for
        // the pane lane this door never sees.
        assert_eq!(
            OUTPUT_FORMAT_GOLDEN,
            "--output-format supports only 'json' on claude headless spawns"
        );
        assert_eq!(
            MONITOR_PANE_GOLDEN,
            "--monitor happy needs --as pane; thread and headless workers do not pass \
             the happy launcher seam"
        );
        assert_eq!(
            MONITOR_CLAUDE_GOLDEN,
            "--monitor happy requires the claude harness; got harness "
        );
        assert_eq!(
            MONITOR_ZAI_GOLDEN,
            "--monitor happy currently supports only the zai provider"
        );
    }
}
