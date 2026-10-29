//! The advisory reporters bare `fno config doctor` appends, and the
//! `--review` capability report. A reporter never changes the exit code; one
//! that cannot read its input prints what it has and stops, the way the
//! Python leg's wrapper swallowed a failing report.

use super::{agents_call, obj, py_repr_str, py_str, Settings};
use serde_json::{json, Map, Value};
use std::fmt::Write as _;
use std::path::PathBuf;

/// The reviewers footnote authors. Project-owned reviewers live in
/// `config.review.reviewer_registry`; a built-in wins a name collision, so a
/// project cannot redefine one as a self-cert. Mirrors `_RESOLVABLE_REVIEWERS`
/// in `fno.config` and the Rust invocation table.
const BUILTIN_REVIEWERS: [(&str, &str, &str, &str, &str); 3] = [
    (
        "sigma",
        "local-attestation",
        "retired",
        "/fno:review",
        "review-evidence",
    ),
    (
        "code-review",
        "local-attestation",
        "none",
        "/fno:review",
        "review-evidence",
    ),
    (
        "declare",
        "local-attestation",
        "none",
        "/fno:review declare",
        "self-cert",
    ),
];

fn descriptor(
    kind: &Value,
    requires: &Value,
    invocation: &Value,
    asserts: &Value,
    invocations: &Value,
) -> Value {
    json!({
        "kind": kind,
        "requires": requires,
        "invocation": invocation,
        "asserts": asserts,
        "invocations": invocations,
    })
}

/// `resolvable_reviewers`: the registry, then the built-ins over it.
fn descriptors(settings: &Settings) -> Value {
    let mut out = Map::new();
    for (name, entry) in obj(&settings.get("review.reviewer_registry")) {
        let field = |key: &str| entry.get(key).cloned().unwrap_or(Value::Null);
        out.insert(
            name,
            descriptor(
                &field("kind"),
                &field("requires"),
                &field("invocation"),
                &field("asserts"),
                &field("invocations"),
            ),
        );
    }
    for (name, kind, requires, invocation, asserts) in BUILTIN_REVIEWERS {
        out.insert(
            name.to_string(),
            descriptor(
                &json!(kind),
                &json!(requires),
                &json!(invocation),
                &json!(asserts),
                &Value::Null,
            ),
        );
    }
    Value::Object(out)
}

fn capability(
    settings: &Settings,
    reviewers: &[String],
    peers: Value,
    peer_identity: Value,
) -> Result<Value, String> {
    let payload = json!({
        "reviewers": reviewers,
        "descriptors": descriptors(settings),
        "peers": peers,
        "peer_identity": peer_identity,
    });
    agents_call(&["review-level", "capability", "reviewers"], Some(&payload))
}

fn rows<'a>(answer: &'a Value, key: &str) -> Vec<&'a Value> {
    answer
        .get(key)
        .and_then(|r| r.as_array())
        .map(|r| r.iter().collect())
        .unwrap_or_default()
}

/// `fno config doctor --review`: each configured reviewer as satisfiable,
/// needs-operator or unavailable. Exit 0 when every reviewer can run here.
/// An unreachable binary is the error arm: the report has no answer to give.
pub fn review_capability(json_out: bool, out: &mut String) -> Result<i32, String> {
    let settings = Settings::load();
    let reviewers = settings.str_list("review.reviewers");
    let peers = match settings.get("review.peers") {
        Value::Array(items) => Value::Array(items),
        _ => json!([]),
    };
    let answer = capability(
        &settings,
        &reviewers,
        peers,
        settings.get("review.peer_identity"),
    )?;
    let session = &answer["session"];
    let verdicts = rows(&answer, "reviewers");
    let peer_verdicts = rows(&answer, "local_peers");
    let blocked = verdicts
        .iter()
        .any(|r| super::truthy(&r["blocks_autonomy"]));
    let peer_blocked = answer
        .get("local_peers_refusal")
        .map(|r| !r.is_null())
        .unwrap_or(false);

    let tiers = agents_call(
        &["spawn-overlay", "accounts", "benchmarks", "tiers"],
        Some(&json!({})),
    )?;
    let dead_ids: Vec<String> = tiers["unreachable_tier_ids"]
        .as_array()
        .map(|a| a.iter().map(py_str).collect())
        .unwrap_or_default();
    let empty_bands = obj(&tiers["empty_bands"]);
    let code = i32::from(blocked || peer_blocked);

    if json_out {
        let reviewer_rows: Vec<Value> = verdicts
            .iter()
            .map(|r| {
                json!({
                    "name": r["name"],
                    "status": r["status"],
                    "kind": r["kind"],
                    "requires": r["requires"],
                    "asserts": r["asserts"],
                    "invocation": r["invocation"],
                    "reason": r["reason"],
                    "resolved_route": r["name"],
                })
            })
            .collect();
        let peer_rows: Vec<Value> = peer_verdicts
            .iter()
            .map(|r| json!({"name": r["name"], "status": r["status"], "reason": r["reason"]}))
            .collect();
        let gate = if peer_blocked {
            "unavailable"
        } else if peer_verdicts.is_empty() {
            "not-configured"
        } else {
            "satisfiable"
        };
        let doc = json!({
            "harness": session["harness"],
            "substrate": session["substrate"],
            "attended": session["attended"],
            "provider": session["provider"],
            "reviewers": reviewer_rows,
            "local_peers": peer_rows,
            "local_peer_gate": gate,
            "tier_staleness": {
                "unreachable_tier_ids": tiers["unreachable_tier_ids"],
                "empty_bands": tiers["empty_bands"],
            },
        });
        out.push_str(&crate::config_schema::py_dumps(&doc, None, false));
        out.push('\n');
        return Ok(code);
    }

    let _ = writeln!(
        out,
        "review gate: {} attended={}",
        py_str(&session["describe"]),
        py_str(&session["attended"])
    );
    if reviewers.is_empty() {
        out.push_str("  config.review.reviewers is empty - no local reviewers gate.\n");
    } else {
        for row in &verdicts {
            let _ = writeln!(out, "  {}", py_str(&row["line"]));
        }
    }
    if !dead_ids.is_empty() {
        let _ = writeln!(
            out,
            "  WARN tier staleness: ids no reachability row serves: {}",
            dead_ids.join(", ")
        );
    }
    for (harness, bands) in &empty_bands {
        let bands: Vec<String> = bands
            .as_array()
            .map(|a| a.iter().map(py_str).collect())
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  WARN tier staleness: {harness} cannot serve band(s): {}",
            bands.join(", ")
        );
    }
    if !peer_verdicts.is_empty() {
        let state = if peer_blocked {
            "unavailable"
        } else {
            "satisfiable"
        };
        let _ = writeln!(out, "  local peer gate: {state}");
        for row in &peer_verdicts {
            let _ = writeln!(out, "    {}", py_str(&row["line"]));
        }
    }
    if blocked || peer_blocked {
        out.push('\n');
        out.push_str("This gate is fail-closed. Change config.review.reviewers or config.review.peers; `declare` is never substituted for you.\n");
    }
    Ok(code)
}

fn rung_gloss(rung: &str) -> &'static str {
    match rung {
        "review-evidence" => " (a reviewer ran and returned a verdict)",
        "invocation" => " (the reviewer ran at the reviewed commit; no claim about its verdict)",
        "self-cert" => " (nothing; satisfies the gate on its own say-so)",
        _ => "",
    }
}

/// The resolved ship gates: each probe with its source, each reviewer with
/// the rung it asserts. Reports the probes, never runs them.
pub fn gates(out: &mut String) {
    let settings = Settings::load();
    let probes = settings.str_list("done_probes");
    let reviewers = settings.str_list("review.reviewers");
    if probes.is_empty() && reviewers.is_empty() {
        return;
    }
    out.push_str("\nship gates:\n");
    for cmd in &probes {
        let _ = writeln!(out, "  probe (project): {cmd}");
    }
    if probes.is_empty() {
        out.push_str("  probe: none declared in config (a plan may still declare its own)\n");
    }
    if reviewers.is_empty() {
        return;
    }
    let Ok(answer) = capability(&settings, &reviewers, json!([]), Value::Null) else {
        return;
    };
    for row in rows(&answer, "reviewers") {
        let asserts = py_str(&row["asserts"]);
        let rung = if super::truthy(&row["asserts"]) {
            asserts.as_str()
        } else {
            "unknown"
        };
        let _ = writeln!(
            out,
            "  reviewer: {} - asserts {rung}{}",
            py_str(&row["name"]),
            rung_gloss(rung)
        );
    }
}

/// One state class: file, root class, selector and its resolved root.
fn state_rows(settings: &Settings) -> [(&'static str, &'static str, &'static str, PathBuf); 7] {
    [
        (
            "ledger.json",
            "OPERATOR",
            "config.paths.ledger_json, else config.state_dir",
            super::ledger_json(settings),
        ),
        (
            "registry.json",
            "OPERATOR",
            "FNO_AGENTS_HOME, else config.state_dir (Rust runtime home)",
            super::agents_registry_path(settings),
        ),
        (
            "claims",
            "OPERATOR",
            "FNO_CLAIMS_ROOT, else $HOME",
            super::global_claims_dir(),
        ),
        (
            "messages.jsonl",
            "OPERATOR",
            "FNO_BUS_DIR, else config.paths.bus_dir, else config.state_dir",
            super::bus_dir(settings),
        ),
        (
            "briefs",
            "OPERATOR",
            "config.paths.briefs_dir, else config.state_dir",
            super::state_child(settings, "briefs_dir", "briefs"),
        ),
        (
            "events.jsonl",
            "PROJECT",
            "FNO_EVENTS_PATH, else config.paths.spaces_dir, else state_dir",
            super::project_events_json(settings),
        ),
        (
            "target-state.md",
            "PROJECT",
            "worktree slice of the space",
            super::worktree_space_dir(settings).join("target-state.md"),
        ),
    ]
}

/// The key that actually decided a row's root: the first `config.*`
/// candidate a file sets, named with that file.
fn state_root_selector(selector: &str) -> String {
    for token in selector.split(',') {
        let token = token.trim();
        let key = token.strip_prefix("else ").unwrap_or(token).trim();
        let key = key.split('(').next().unwrap_or("").trim();
        let Some(bare) = key.strip_prefix("config.") else {
            continue;
        };
        if let Some(note) = super::source_note(bare) {
            return format!("{key} ({note})");
        }
    }
    selector.to_string()
}

/// The root each state class resolved, and the key that chose it, plus the
/// warning whenever the cwd's project root differs from the canonical one.
pub fn state_roots(out: &mut String) {
    let settings = Settings::load();
    out.push_str("\nstate roots:\n");
    let _ = writeln!(out, "  {:<9}{:<17}{:<48}selector", "class", "file", "root");
    for (file, class, selector, resolved) in state_rows(&settings) {
        let resolved = resolved.to_string_lossy().into_owned();
        let _ = writeln!(
            out,
            "  {class:<9}{file:<17}{resolved:<48} {}",
            state_root_selector(selector)
        );
    }
    let worktree_root = super::resolve(&crate::config_cli::repo_root());
    let canonical_root = super::resolve(&super::canonical_root());
    if worktree_root != canonical_root {
        let _ = writeln!(
            out,
            "  WARNING: the project root resolved from cwd ({}) differs\n           from the canonical root ({}). Project config is\n           read from the canonical root, so a key set only in this\n           worktree's .fno/settings.yaml is NOT what the loader served,\n           and a gate key set only in canonical reads its shipped default\n           for any resolver that stays on the worktree root.",
            worktree_root.to_string_lossy(),
            canonical_root.to_string_lossy()
        );
    }
}

/// Every parseable candidate carrying a `[dispatch]` block, flat or
/// config-wrapped: `(path, scope flag, scope, dispatch)`. Raw files, so each
/// file is named on its own.
fn deprecated_dispatch_rows() -> Vec<(String, &'static str, Value, Map<String, Value>)> {
    let global_dir = crate::config_cli::global_settings_path()
        .parent()
        .map(|p| p.to_path_buf());
    let mut rows = Vec::new();
    for candidate in crate::config_cli::candidate_paths(&crate::config_cli::repo_root()) {
        if !candidate.is_file() {
            continue;
        }
        let Some(parsed) =
            crate::config_cli::parse_config_file(&candidate).filter(|v| v.is_object())
        else {
            continue;
        };
        let (scope, dispatch) = match parsed.get("dispatch").and_then(|d| d.as_object()) {
            Some(dispatch) => (parsed.clone(), dispatch.clone()),
            None => match parsed.get("config").filter(|c| c.is_object()) {
                Some(wrapped) => match wrapped.get("dispatch").and_then(|d| d.as_object()) {
                    Some(dispatch) => (wrapped.clone(), dispatch.clone()),
                    None => continue,
                },
                None => continue,
            },
        };
        let flag = if candidate.parent().map(|p| p.to_path_buf()) == global_dir {
            ""
        } else {
            " --local"
        };
        rows.push((
            candidate.to_string_lossy().into_owned(),
            flag,
            scope,
            dispatch,
        ));
    }
    rows
}

/// Name every file still setting the deprecated `dispatch.auto_merge`.
pub fn deprecated_auto_merge(out: &mut String) {
    for (candidate, flag, scope, legacy) in deprecated_dispatch_rows() {
        let Some(value) = legacy.get("auto_merge") else {
            continue;
        };
        if scope
            .get("auto_merge")
            .and_then(|a| a.as_object())
            .map(|a| a.contains_key("grant"))
            .unwrap_or(false)
        {
            let _ = writeln!(
                out,
                "warn: {candidate} still sets the deprecated `dispatch.auto_merge`, but a canonical `auto_merge.grant` in the same file masks it (the file reads as the canonical value). Remove the legacy line.\n      Migrate: fno config unset dispatch.auto_merge{flag}"
            );
            continue;
        }
        let reads_as = if value == &Value::Bool(true) {
            "dispatch"
        } else {
            "none"
        };
        let _ = writeln!(
            out,
            "warn: {candidate} sets the deprecated `dispatch.auto_merge`.\n      It reads as `auto_merge.grant = \"{reads_as}\"` for one release.\n      Migrate: fno config set auto_merge.grant {reads_as}{flag} && fno config unset dispatch.auto_merge{flag}"
        );
    }
}

/// Name every file still setting the deprecated `dispatch.harness`.
pub fn deprecated_dispatch_harness(out: &mut String) {
    for (candidate, flag, scope, legacy) in deprecated_dispatch_rows() {
        let Some(value) = legacy.get("harness") else {
            continue;
        };
        let masked = scope
            .get("agents")
            .and_then(|a| a.get("profiles"))
            .and_then(|p| p.get("target"))
            .and_then(|t| t.as_object())
            .map(|t| t.contains_key("provider"))
            .unwrap_or(false);
        if masked {
            let _ = writeln!(
                out,
                "warn: {candidate} still sets the deprecated `dispatch.harness`, but a stage-table `agents.profiles.target.provider` in the same file masks it (the file reads as the stage-table value). Remove the legacy line.\n      Migrate: fno config unset dispatch.harness{flag}"
            );
            continue;
        }
        let shown = if super::truthy(value) {
            py_str(value)
        } else {
            String::new()
        };
        let reads_as = match shown.trim() {
            "" => "claude".to_string(),
            other => other.to_string(),
        };
        let _ = writeln!(
            out,
            "warn: {candidate} sets the deprecated `dispatch.harness`.\n      It reads as the fallback harness rung for one release, below the stage table.\n      Migrate: fno config set agents.profiles.target.provider {reads_as}{flag} && fno config unset dispatch.harness{flag}"
        );
    }
}

/// Name every file still setting the ignored `active_backlog.mission`.
pub fn deprecated_active_backlog_mission(out: &mut String) {
    let global_dir = crate::config_cli::global_settings_path()
        .parent()
        .map(|p| p.to_path_buf());
    for candidate in crate::config_cli::candidate_paths(&crate::config_cli::repo_root()) {
        if !candidate.is_file() {
            continue;
        }
        let Some(parsed) =
            crate::config_cli::parse_config_file(&candidate).filter(|v| v.is_object())
        else {
            continue;
        };
        let block = parsed
            .get("active_backlog")
            .filter(|b| b.is_object())
            .or_else(|| {
                parsed
                    .get("config")
                    .and_then(|c| c.get("active_backlog"))
                    .filter(|b| b.is_object())
            });
        let Some(mission) = block
            .and_then(|b| b.get("mission"))
            .and_then(|m| m.as_str())
        else {
            continue;
        };
        if mission.trim().is_empty() {
            continue;
        }
        let flag = if candidate.parent().map(|p| p.to_path_buf()) == global_dir {
            ""
        } else {
            " --local"
        };
        let _ = writeln!(
            out,
            "warn: {} sets `active_backlog.mission`, which the mission-scoped drain IGNORES (missions are per-epic graph state, not config).\n      Live axis: `fno backlog advance --epic <id>` activates a mission; `--stop` retires it.\n      Remove the ignored key: fno config unset active_backlog.mission{flag}",
            candidate.to_string_lossy()
        );
    }
}

/// The commands fno dispatches, and therefore the slots an operator fills.
const SLOT_COMMANDS: [&str; 5] = ["think", "blueprint", "target", "review", "role"];

/// Say when band routing routes nothing, and why. Silent once any command's
/// slot would take a lane.
pub fn band_routing(out: &mut String) {
    let inventory = agents_call(&["route-slot"], Some(&json!({"mode": "inventory"}))).ok();
    let mut row_names: Vec<String> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    if let Some(answer) = &inventory {
        for row in answer
            .get("rows")
            .and_then(|r| r.as_array())
            .into_iter()
            .flatten()
        {
            if !row.is_object() {
                continue;
            }
            let name = py_str(row.get("name").unwrap_or(&Value::Null));
            let name = if super::truthy(row.get("name").unwrap_or(&Value::Null)) {
                name.trim().to_string()
            } else {
                String::new()
            };
            if !name.is_empty() && !row_names.contains(&name) {
                row_names.push(name);
            }
        }
        for command in answer
            .get("verbs")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let command = py_str(command);
            if !command.is_empty() {
                commands.push(command);
            }
        }
    }
    if commands.is_empty() {
        commands = SLOT_COMMANDS.iter().map(|c| c.to_string()).collect();
    }
    let mut empty: Vec<String> = Vec::new();
    for command in &commands {
        let payload = json!({
            "rung_base": format!("agents.profiles.{command}"),
            "node": null,
            "substrate": null,
            "permission_mode": null,
            "constrain_harness": null,
            "explicit_lane": false,
            "gate_bypassed": false,
            "model_occupied": false,
            "work_command": command,
            "mode": "states",
        });
        let would_take = agents_call(&["route-slot"], Some(&payload))
            .ok()
            .and_then(|states| states.get("would_take").map(py_str))
            .unwrap_or_default();
        if !would_take.starts_with(&format!("agents.profiles.{command}.lanes")) {
            empty.push(command.clone());
        }
    }
    if empty.len() < commands.len() {
        return;
    }
    let settings = Settings::load();
    let refused = !super::schema_walk::refusals(settings.json_schema(), &settings.raw).is_empty();
    let (declared, roles) = if refused {
        (0, false)
    } else {
        let declared = settings
            .get("routing.models")
            .as_array()
            .map(|m| m.len())
            .unwrap_or(0);
        (
            declared,
            super::truthy(&settings.get("model_routing.roles")),
        )
    };
    row_names.sort();
    let built_in = if row_names.is_empty() {
        "none".to_string()
    } else {
        row_names.join(", ")
    };
    let _ = write!(
        out,
        "band routing inactive: config.routing.models declares {declared} row(s), and no command slot has a lane that resolves: {}\n      The built-in rows keep a tier request answerable: {built_in}\n      Declare [[routing.models]] rows plus [agents.profiles.<command>] lanes to activate (docs/architecture/role-based-model-routing.md)",
        empty.join(", ")
    );
    if roles {
        out.push_str(
            "\n      Note: model_routing.roles is set, but it routes by ROLE, not by band.",
        );
    }
    out.push('\n');
}

/// One block as pydantic's `model_dump()` would print it: every schema field
/// in declared order, the file's value where set, else the field default,
/// then any extra key the file carries.
fn model_dump(schema: &Value, def: &Value, raw: &Value) -> Value {
    let mut out = Map::new();
    let raw_map = obj(raw);
    if let Some(fields) = def.get("properties").and_then(|p| p.as_object()) {
        for (name, field) in fields {
            let value = match raw_map.get(name) {
                Some(value) => match field.get("$ref") {
                    Some(_) if value.is_object() => {
                        let name_ref = field["$ref"]
                            .as_str()
                            .unwrap_or("")
                            .rsplit('/')
                            .next()
                            .unwrap_or("");
                        model_dump(schema, &schema["$defs"][name_ref], value)
                    }
                    _ => value.clone(),
                },
                None => field.get("default").cloned().unwrap_or(Value::Null),
            };
            out.insert(name.clone(), value);
        }
    }
    for (name, value) in raw_map {
        if !out.contains_key(&name) {
            out.insert(name, value);
        }
    }
    Value::Object(out)
}

fn def_named<'a>(schema: &'a Value, name: &str) -> &'a Value {
    &schema["$defs"][name]
}

/// Name the (config rung, field) pairs that are inert on spawn. Silent when
/// every pair maps, or when the roster or the config cannot be read.
pub fn harness_overlays(out: &mut String) {
    let Ok(roster) = agents_call(&["harness-roster"], None) else {
        return;
    };
    let providers = roster
        .get("providers")
        .filter(|p| super::truthy(p))
        .or_else(|| roster.get("known"))
        .cloned()
        .unwrap_or(Value::Null);
    if !super::truthy(&providers) {
        return;
    }
    let settings = Settings::load();
    if !super::schema_walk::refusals(settings.json_schema(), &settings.raw).is_empty() {
        return;
    }
    let schema = settings.json_schema();
    let defaults = model_dump(
        schema,
        def_named(schema, "SpawnDefaultsBlock"),
        &settings.get("agents.defaults"),
    );
    let mut rows: Vec<(String, Value)> = obj(&settings.get("agents.profiles"))
        .into_iter()
        .map(|(command, profile)| {
            (
                command,
                model_dump(schema, def_named(schema, "SpawnProfileBlock"), &profile),
            )
        })
        .collect();
    rows.push((String::new(), Value::Null));
    let mut seen: Vec<(String, String, String)> = Vec::new();
    for (command, profile) in rows {
        let payload = json!({
            "kind": "overlay",
            "verb": command,
            "defaults": defaults,
            "profile": profile,
            "harnesses": providers,
        });
        let answer = match agents_call(&["spawn-overlay"], Some(&payload)) {
            Ok(answer) => answer,
            Err(err) => {
                let _ = writeln!(
                    out,
                    "config doctor: harness-overlay readout unavailable: {err}"
                );
                return;
            }
        };
        if let Some(refusal) = answer.get("refusal").filter(|r| super::truthy(r)) {
            let _ = writeln!(out, "{}", py_str(refusal));
            return;
        }
        for (_harness, effective) in obj(&answer["effective_by_harness"]) {
            for name in ["permission_mode", "effort"] {
                let entry = &effective[name];
                if !super::truthy(entry) {
                    continue;
                }
                let value = py_str(&entry["value"]);
                let rung = py_str(&entry["rung"]);
                let key = (rung.clone(), name.to_string(), value.clone());
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                let _ = writeln!(
                    out,
                    "config.{rung}.{name} = {} is inert on spawn: the spawn axis is retired and the lane template owns the launch line",
                    if entry["value"].is_string() { py_repr_str(&value) } else { super::py_repr(&entry["value"]) }
                );
            }
        }
    }
}
