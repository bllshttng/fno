//! The dispatch resolver: (config + context) -> the launch tuple.
//!
//! Ports `fno/agents/harness_map.py` - `resolve_dispatch` and the seams it
//! owns (`normalize_command`, `dispatch_command`, `substrate_default`,
//! `thread_seatable`, `spawn_state`, `footnote_commands`, and the shared
//! skill-presence probe, native in `skill_presence` here since the
//! review-capability port folded the Python twin into it)
//! over the native capability table (`HarnessContract::packaged`) and the
//! shared merge-posture vocabulary. Refusal texts are byte-matched to the
//! Python owner; the decision log rides the result so a receipt answers
//! "why this command" the way the Python lane's did.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::backlog::fields::canonical_command_key;
use crate::claude_ask::py_repr;
use crate::harness_capabilities::{HarnessCapabilities, HarnessContract};
use crate::merge_posture as posture;
use crate::provider::parse_command_token;

/// The shipped dispatch-command allowlist (config.dispatch.allowed_commands
/// overrides).
const DEFAULT_ALLOWED_COMMANDS: [&str; 3] = ["/target", "/think", "/blueprint"];
/// The env budget a brief must fit; 8 KB, measured in UTF-8 bytes.
/// Oversized -> explicit error, never truncation.
const BRIEF_MAX_BYTES: usize = 8192;

/// The resolved launch tuple: everything the spawn argv builder and the
/// receipt need (the Python dict, narrowed to the fields the node-dispatch
/// spawn path reads).
#[derive(Debug, Clone)]
pub struct DispatchResolution {
    pub harness: String,
    pub substrate: String,
    pub route: String,
    pub command: String,
    /// The lifecycle-derived canonical command, or None when the table
    /// abstained (bare resolve, explicit command, out-of-family command).
    pub command_name: Option<String>,
    /// TARGET_NO_MERGE / TARGET_BRIEF: the only env keys the resolver owns.
    pub env: BTreeMap<String, String>,
    pub decision: Vec<String>,
}

/// A config.dispatch.command_registry descriptor (the fields
/// fno.config._dispatch_commands.DispatchCommandDescriptor carries).
#[derive(Debug, Clone, Default)]
pub struct CommandDescriptor {
    pub invocation: String,
    pub invocations: BTreeMap<String, String>,
    pub requires: String,
    pub takes_node_id: bool,
    pub asserts: String,
    pub session_phase: String,
}

/// The dispatch config rung (the `_load_dispatch_cfg` dict): the caller
/// resolves the stage table and the config.dispatch keys, so this core
/// stays pure - the same seam Python's `dispatch_cfg` override kept pure.
#[derive(Debug, Clone, Default)]
pub struct DispatchCfg {
    pub harness: String,
    pub harness_note: String,
    pub route: String,
    pub substrate: String,
    pub command: String,
    /// None reads the shipped trio; Some carries the list even when empty.
    pub allowed_commands: Option<Vec<String>>,
    pub command_registry: BTreeMap<String, CommandDescriptor>,
    pub auto_merge: bool,
}

/// One resolve call's context, mirroring resolve_dispatch's keyword args.
#[derive(Debug, Clone, Default)]
pub struct DispatchInput<'a> {
    pub harness: Option<&'a str>,
    pub substrate: Option<&'a str>,
    pub node_id: Option<&'a str>,
    pub command: Option<&'a str>,
    pub command_name: Option<&'a str>,
    /// (derived command, note) the lifecycle table answered; the note opens
    /// the decision log when the command rung consumes the derivation.
    pub lifecycle: Option<(Option<String>, String)>,
    pub brief: Option<&'a str>,
    pub merge_posture: Option<&'a str>,
    pub trigger: String,
}

/// The loud-refusal message for a deprecated harness with no dispatch
/// lane - names the successor (agy) so the failure is actionable.
fn refused_reason(harness: &str) -> String {
    format!(
        "harness {} has no maintained footnote dispatch lane and is deprecated; \
         route this work to its successor 'agy' (or a claude/codex/opencode harness) \
         - no prose build brief is generated",
        py_repr(harness)
    )
}

/// The capability row, or the unknown-harness loud error naming the roster
/// - never a silent default to claude.
fn contract_caps<'a>(
    contract: &'a HarnessContract,
    harness: &str,
) -> Result<&'a HarnessCapabilities, String> {
    contract.capabilities(harness).map_err(|_| {
        let roster: Vec<&str> = contract.harness.keys().map(String::as_str).collect();
        format!(
            "unknown harness {}; the harness-capability map \
             (fno.agents.harness_map) knows: {}",
            py_repr(harness),
            roster.join(", ")
        )
    })
}

/// The features dimension's spawn claim for the harness: `native`,
/// `capable`, `absent`, or `unmeasured` (the fail-closed answer).
pub fn spawn_state(caps: &HarnessCapabilities) -> String {
    let state = caps
        .features
        .get("spawn")
        .map(|claim| claim.state.as_str())
        .unwrap_or("");
    if state.is_empty() {
        "unmeasured".to_string()
    } else {
        state.to_string()
    }
}

/// Whether fno's thread lane exists for the harness: the spawn claim reads
/// `native`. Derived from the claim, never stored.
pub fn thread_seatable(contract: &HarnessContract, harness: &str) -> bool {
    contract
        .capabilities(harness)
        .map(|caps| spawn_state(caps) == "native")
        .unwrap_or(false)
}

/// Per-harness default substrate: `thread` where the spawn claim reads
/// `native` (a journey-proven launch seam), else `headless`.
pub fn substrate_default(contract: &HarnessContract, harness: &str) -> String {
    if thread_seatable(contract, harness) {
        "thread".to_string()
    } else {
        "headless".to_string()
    }
}

/// The shipped footnote command roster: every `skills/<name>/SKILL.md` and
/// every `commands/<name>.md` under the plugin surface. Env hints first,
/// then the repo walk-up; empty on any resolution or read failure -
/// pass-through is the safe direction (the Python owner's contract).
pub fn footnote_commands() -> BTreeSet<String> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    for key in ["CLAUDE_PLUGIN_ROOT", "CODEX_PLUGIN_ROOT", "FNO_REPO_ROOT"] {
        if let Some(v) = std::env::var_os(key).filter(|v| !v.is_empty()) {
            roots.push(std::path::PathBuf::from(v));
        }
    }
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        if d.join("skills").is_dir() && d.join("commands").is_dir() {
            roots.push(d);
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    let mut commands = BTreeSet::new();
    for root in &roots {
        if let Ok(entries) = std::fs::read_dir(root.join("skills")) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && p.join("SKILL.md").is_file() {
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        commands.insert(name.to_string());
                    }
                }
            }
        }
        if let Ok(entries) = std::fs::read_dir(root.join("commands")) {
            for entry in entries.flatten() {
                let p = entry.path();
                let is_md = p.extension().and_then(|e| e.to_str()) == Some("md");
                if p.is_file() && is_md {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        commands.insert(stem.to_string());
                    }
                }
            }
        }
    }
    commands
}

/// Where Claude resolves a BARE skill name: the user root and the project
/// root (the Python probe's root set, deliberately short).
fn claude_skill_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(Path::new(&home).join(".claude").join("skills"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join(".claude").join("skills"));
    }
    roots
}

/// The shared skill-presence probe, byte-matched to the Python resolver's
/// three outcomes: found -> satisfiable, absent with one root readable ->
/// unavailable, unanswerable -> unverifiable (degrades rather than bricking
/// a run). The harness gate lives HERE, so a codex dispatch probing Claude's
/// roots answers `unverifiable` instead of `unavailable` - the dual this
/// port collapses (the Python resolver gated on harness; the Rust one
/// probed every harness).
pub(crate) fn skill_presence(name: &str, harness: &str, context: &str) -> (&'static str, String) {
    skill_presence_in(name, harness, context, claude_skill_roots())
}

/// The roots are injected so the absent/unverifiable boundary is testable
/// without mutating HOME (cargo runs unit tests in parallel).
pub(crate) fn skill_presence_in(
    name: &str,
    harness: &str,
    context: &str,
    roots: Vec<std::path::PathBuf>,
) -> (&'static str, String) {
    let proceed = "Proceeding - if the gate does go unmet, run the skill by hand";
    if harness != "claude" {
        return (
            "unverifiable",
            format!(
                "needs a resolvable skill; footnote only knows Claude's skill \
                 roots and this is harness={harness}. {proceed}"
            ),
        );
    }
    if name.contains(':') {
        return (
            "unverifiable",
            format!(
                "{} is a plugin-qualified skill; footnote does not read the \
                 plugin cache layout, so availability cannot be verified. {proceed}",
                py_repr(name)
            ),
        );
    }
    let mut searched: Vec<String> = Vec::new();
    for root in &roots {
        if !root.is_dir() {
            continue;
        }
        searched.push(root.to_string_lossy().to_string());
        if root.join(name).join("SKILL.md").is_file() {
            return (
                "satisfiable",
                format!("skill resolves at {}", root.join(name).display()),
            );
        }
    }
    if searched.is_empty() {
        let looked: Vec<String> = roots
            .iter()
            .map(|r| r.to_string_lossy().to_string())
            .collect();
        return (
            "unverifiable",
            format!(
                "no skill root exists on this host (looked for {}), so \
                 availability cannot be verified. {proceed}",
                looked.join(", ")
            ),
        );
    }
    (
        "unavailable",
        format!(
            "skill {} resolves in none of the roots searched ({}); install \
             it there or change {}",
            py_repr(name),
            searched.join(", "),
            context
        ),
    )
}

/// The dispatch-command registry's `requires == "skill"` gate: the shared
/// probe's three outcomes, narrowed to the one the resolver refuses on.
/// `unverifiable` degrades to admitted (the stop gate is the backstop);
/// only a searched-and-absent answer refuses.
fn skill_presence_refusal(name: &str, harness: &str, context: &str) -> Option<String> {
    match skill_presence(name, harness, context) {
        ("unavailable", reason) => Some(reason),
        _ => None,
    }
}

/// Translate a claude-syntax footnote slash command to the harness's
/// native invocation - the single normalizer both dispatch surfaces route
/// through. A non-slash command passes through unchanged for the slash and
/// codex surfaces, an absolute-path first token included (the guard lives
/// here so every caller inherits it). Pure string transform plus the
/// plugin roster read; no config.
pub fn normalize_command(command: &str, harness: &str) -> Result<String, String> {
    let contract = HarnessContract::packaged().map_err(|e| e.to_string())?;
    normalize_command_with(&contract, command, harness)
}

/// `normalize_command` over a caller-held contract (the resolver reuses
/// its packaged read instead of re-parsing per call).
pub fn normalize_command_with(
    contract: &HarnessContract,
    command: &str,
    harness: &str,
) -> Result<String, String> {
    let caps = contract_caps(contract, harness)?;
    let surface = caps.command_surface.as_str();
    if surface == "refused" {
        // The deprecation tripwire stays ahead of the parse gate.
        return Err(refused_reason(harness));
    }
    let cmd = command.trim();
    let first_word = cmd.split_whitespace().next().unwrap_or("");
    let Some((command_name, namespaced)) = (if first_word.is_empty() {
        None
    } else {
        parse_command_token(first_word)
    }) else {
        return Ok(cmd.to_string());
    };
    let tail = &cmd[first_word.len()..];
    let slash_sigil = first_word.starts_with('/');
    if surface == "codex-skill" {
        if !slash_sigil {
            return Ok(cmd.to_string());
        }
        // The namespaced spelling is unambiguous by namespace: it always
        // rewrites, native or not.
        if namespaced {
            return Ok(format!("$fno:{command_name}{tail}"));
        }
        let native: BTreeSet<&str> = caps.native_verbs.iter().map(String::as_str).collect();
        if native.contains(format!("/{command_name}").as_str()) {
            return Ok(cmd.to_string());
        }
        // The dispatch command is footnote's own: a STATIC fact. The bypass
        // makes an unresolvable roster unable to render `/target` as prose.
        if posture::is_target_family(first_word) {
            return Ok(format!("$fno:{command_name}{tail}"));
        }
        if !footnote_commands().contains(command_name) {
            return Ok(cmd.to_string());
        }
        return Ok(format!("$fno:{command_name}{tail}"));
    }
    if surface == "slash" {
        // Plugin-namespace prefix swap only (never re-tokenize).
        let prefix = caps.slash_prefix.as_str();
        let native: BTreeSet<&str> = caps.native_verbs.iter().map(String::as_str).collect();
        if !namespaced && !slash_sigil {
            return Ok(cmd.to_string());
        }
        if !namespaced && native.contains(format!("/{command_name}").as_str()) {
            return Ok(cmd.to_string());
        }
        // Idempotent over the builtin rung.
        if namespaced && !prefix.is_empty() && first_word.starts_with(&format!("/{prefix}")) {
            return Ok(cmd.to_string());
        }
        if namespaced && !prefix.is_empty() {
            return Ok(format!("/{prefix}{command_name}{tail}"));
        }
        if namespaced && harness == "agy" {
            return Ok(format!("/{command_name}{tail}"));
        }
        if namespaced {
            return Ok(format!("/fno:{command_name}{tail}"));
        }
        return Ok(format!("/{prefix}{command_name}{tail}"));
    }
    Ok(cmd.to_string())
}

/// Builtin autonomous dispatch command for the harness: the per-harness
/// normalization of `/target --no-merge {id}`, or of `/target {id}` when
/// `allow_merge`. An undeclared harness refuses by its own condition.
pub fn dispatch_command(
    contract: &HarnessContract,
    harness: &str,
    allow_merge: bool,
) -> Result<String, String> {
    if !contract.harness.contains_key(harness) {
        return Err(format!(
            "harness {} has no declared command surface: a native footnote \
             skill invocation for it must be measured (a row in \
             harness_capabilities.toml) before one can be generated",
            py_repr(harness)
        ));
    }
    let template = if allow_merge {
        "/target {id}"
    } else {
        "/target --no-merge {id}"
    };
    normalize_command_with(contract, template, harness)
}

/// Map (config + context) -> the dispatch tuple. Pure; never spawns or
/// claims. Field precedence: harness explicit > stage table > `claude`;
/// substrate explicit > config > per-harness default; command explicit >
/// lifecycle derivation > node command (allowlist-checked) >
/// `config.dispatch.command` > per-harness builtin. `brief` rides
/// TARGET_BRIEF only, capped at 8 KB, never truncated. `merge_posture`:
/// no-merge injects, allow overrides the config read, from-config reads
/// the grant. `node_id` substitutes the command's `{id}`.
pub fn resolve_dispatch(
    input: &DispatchInput,
    cfg: &DispatchCfg,
) -> Result<DispatchResolution, String> {
    let contract = HarnessContract::packaged().map_err(|e| e.to_string())?;
    resolve_dispatch_with(&contract, input, cfg)
}

/// `resolve_dispatch` over a caller-held contract.
pub fn resolve_dispatch_with(
    contract: &HarnessContract,
    input: &DispatchInput,
    cfg: &DispatchCfg,
) -> Result<DispatchResolution, String> {
    let mut decision: Vec<String> = Vec::new();
    let mut lifecycle_command: Option<String> = None;
    let command_raw = input.command.unwrap_or("");
    let command: Option<&str> = {
        let c = command_raw.trim();
        if c.is_empty() {
            None
        } else {
            Some(c)
        }
    };
    if command.is_none() {
        if let Some((command_name, note)) = &input.lifecycle {
            lifecycle_command = command_name.clone();
            decision.push(note.clone());
        }
    }
    let route_value = cfg.route.trim().to_string();
    if !route_value.is_empty() {
        decision.push(format!("route=config({route_value})"));
    }
    let chosen_trigger = if input.trigger.trim().is_empty() {
        "autonomous".to_string()
    } else {
        input.trigger.trim().to_ascii_lowercase()
    };
    if chosen_trigger != "autonomous" && chosen_trigger != "attended" {
        return Err(format!(
            "unknown dispatch trigger {}; valid: autonomous, attended",
            py_repr(&input.trigger)
        ));
    }
    let mut posture_value: Option<String> = input.merge_posture.map(str::to_string);
    if let Some(p) = &posture_value {
        if p == "from-config" {
            let resolved = if cfg.auto_merge { "allow" } else { "no-merge" };
            decision.push(format!("merge-posture=from-config({resolved})"));
            posture_value = Some(resolved.to_string());
        } else if p != "no-merge" && p != "allow" {
            return Err(format!(
                "unknown merge posture {}; valid: no-merge, allow",
                py_repr(p)
            ));
        }
    }

    // 1. harness. An explicit flag is distinguished by presence, NOT
    // truthiness: an empty explicit value fails loud, never silently
    // falling through to config or claude.
    let chosen_harness: String;
    if let Some(h) = input.harness {
        let trimmed = h.trim();
        if trimmed.is_empty() {
            return Err("explicit --harness must not be empty".to_string());
        }
        chosen_harness = trimmed.to_string();
        decision.push(format!("harness=explicit({chosen_harness})"));
    } else if !cfg.harness.trim().is_empty() {
        chosen_harness = cfg.harness.trim().to_string();
        decision.push(format!("harness=config({chosen_harness})"));
        if !cfg.harness_note.is_empty() {
            decision.push(cfg.harness_note.clone());
        }
    } else {
        chosen_harness = "claude".to_string();
        decision.push("harness=builtin(claude)".to_string());
    }
    let caps = contract_caps(contract, &chosen_harness)?;
    if caps.command_surface == "refused" {
        return Err(refused_reason(&chosen_harness));
    }

    // 2. substrate. The RESOLVED value validates once, whatever rung
    // supplied it; the config rung is a trust boundary too.
    let chosen_substrate: String;
    if let Some(s) = input.substrate {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err("explicit --substrate must not be empty".to_string());
        }
        chosen_substrate = trimmed.to_string();
        decision.push(format!("substrate=explicit({chosen_substrate})"));
    } else if !cfg.substrate.trim().is_empty() {
        chosen_substrate = cfg.substrate.trim().to_string();
        decision.push(format!("substrate=config({chosen_substrate})"));
    } else {
        let d = substrate_default(contract, &chosen_harness);
        decision.push(format!("substrate=default({d})"));
        chosen_substrate = d;
    }
    if chosen_substrate == "bg" {
        return Err("substrate 'bg' was retired; set substrate = \"thread\"".to_string());
    }
    if chosen_substrate != "thread" && chosen_substrate != "headless" && chosen_substrate != "pane"
    {
        return Err(format!(
            "unknown substrate {}; valid: thread, headless, pane",
            py_repr(&chosen_substrate)
        ));
    }
    if chosen_substrate == "thread" && !thread_seatable(contract, &chosen_harness) {
        let state = spawn_state(caps);
        let lane = contract.thread_lane(&chosen_harness).unwrap_or("none");
        return Err(format!(
            "substrate 'thread' is unsupported on harness {}: its features.spawn \
             state reads {}, so fno has not built the {} lane yet; use 'headless'",
            py_repr(&chosen_harness),
            py_repr(&state),
            lane
        ));
    }
    // Only an explicit attended trigger bypasses the autonomy check.
    if chosen_substrate == "pane" && chosen_trigger != "attended" && !caps.autonomous_pane {
        let seatable: Vec<String> = contract
            .harness
            .keys()
            .filter(|h| thread_seatable(contract, h))
            .cloned()
            .collect();
        return Err(format!(
            "harness {} does not have the evidence-backed autonomous_pane \
             capability; use 'headless' (or 'thread' on {})",
            py_repr(&chosen_harness),
            seatable.join(", ")
        ));
    }

    // 3. command template.
    let mut skip_normalize = false;
    let mut command_declares_no_id = false;
    let mut command_opt: Option<&str> = input.command_name;
    let derived_blueprint = lifecycle_command.as_deref() == Some("/blueprint");
    if lifecycle_command.as_deref() == Some("/target") {
        command_opt = None;
    }
    let template: String;
    if let Some(c) = command {
        template = c.to_string();
        decision.push("command=explicit".to_string());
    } else if derived_blueprint {
        template = format!("{} {{id}}", lifecycle_command.clone().unwrap_or_default());
        decision.push(format!(
            "command=derived({})",
            lifecycle_command.clone().unwrap_or_default()
        ));
    } else if let Some(v) = command_opt {
        let mut chosen_command = v.trim().to_string();
        if chosen_command.is_empty() {
            return Err("explicit dispatch command must not be empty".to_string());
        }
        if parse_command_token(&chosen_command).is_some() {
            chosen_command = canonical_command_key(&chosen_command);
        }
        let allowed: Vec<String> = cfg.allowed_commands.clone().unwrap_or_else(|| {
            DEFAULT_ALLOWED_COMMANDS
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
        let descriptor = cfg.command_registry.get(&chosen_command);
        if !allowed.iter().any(|a| a == &chosen_command) && descriptor.is_none() {
            let reg_list: Vec<String> = cfg.command_registry.keys().cloned().collect();
            return Err(format!(
                "dispatch command {} is in neither the allowlist ({}) nor \
                 config.dispatch.command_registry ({}); extend one of them",
                py_repr(&chosen_command),
                allowed.join(", "),
                if reg_list.is_empty() {
                    "empty".to_string()
                } else {
                    reg_list.join(", ")
                }
            ));
        }
        if let Some(desc) = descriptor {
            if desc.requires == "skill" {
                // First token only (the command may carry args); malformed
                // falls back to the key.
                let head = desc
                    .invocation
                    .split_whitespace()
                    .next()
                    .unwrap_or(&chosen_command);
                let skill_name = head
                    .trim_start_matches('/')
                    .rsplit(':')
                    .next()
                    .unwrap_or(head);
                if let Some(reason) = skill_presence_refusal(
                    skill_name,
                    &chosen_harness,
                    "config.dispatch.command_registry",
                ) {
                    return Err(reason);
                }
            }
            if !desc.invocations.is_empty() && !desc.invocations.contains_key(&chosen_harness) {
                let declared: Vec<String> = desc.invocations.keys().cloned().collect();
                return Err(format!(
                    "dispatch command {} is not declared on harness {}; \
                     config.dispatch.command_registry declares it on: {}",
                    py_repr(&chosen_command),
                    py_repr(&chosen_harness),
                    declared.join(", ")
                ));
            }
            let mut built = desc
                .invocations
                .get(&chosen_harness)
                .cloned()
                .unwrap_or_else(|| desc.invocation.clone());
            if desc.takes_node_id {
                built = format!("{built} {{id}}");
            } else {
                command_declares_no_id = true;
            }
            // The descriptor already spells the command natively; normalizing
            // would mint a phantom `$fno:` skill from it.
            skip_normalize = true;
            decision.push(format!(
                "command=registry-command({}, asserts={})",
                chosen_command, desc.asserts
            ));
            template = built;
        } else {
            // Slash-leading; the post-ladder seam normalizes it per-harness.
            template = format!("{chosen_command} {{id}}");
            decision.push(format!("command=command({chosen_command})"));
        }
    } else {
        // Per-harness builtin; config.dispatch.command overrides. The merge
        // posture applies to the builtin only: an explicit command or a
        // node dispatch_command already spells what to run.
        let allow_merge = cfg.auto_merge || posture_value.as_deref() == Some("allow");
        let raw = if !cfg.command.is_empty() {
            cfg.command.clone()
        } else {
            dispatch_command(contract, &chosen_harness, allow_merge)?
        };
        template = raw.trim().to_string();
        if !cfg.command.is_empty() {
            decision.push("command=config".to_string());
        } else {
            decision.push(format!(
                "command=builtin({})",
                if allow_merge { "merge" } else { "no-merge" }
            ));
        }
    }
    if template.is_empty() {
        return Err("resolved command is empty".to_string());
    }
    // Single normalization seam: a footnote slash command canonicalizes to
    // the chosen harness's spelling once, before `{id}` substitution.
    let mut resolved_command = if skip_normalize {
        template.clone()
    } else {
        normalize_command_with(contract, &template, &chosen_harness)?
    };
    if resolved_command != template {
        decision.push(format!("command=normalized({chosen_harness})"));
    }
    let node_id = input.node_id.map(str::trim).filter(|s| !s.is_empty());
    if let Some(id) = node_id {
        if resolved_command.contains("{id}") {
            resolved_command = resolved_command.replace("{id}", id);
            decision.push(format!("command=substituted({resolved_command})"));
        } else if !command_declares_no_id {
            return Err(format!(
                "command template {} must contain '{{id}}' at least once for \
                 substitution",
                py_repr(&resolved_command)
            ));
        } else {
            decision.push(format!("command=template({resolved_command})"));
        }
    } else {
        decision.push(format!("command=template({resolved_command})"));
    }

    // 4. posture + env. The legacy token rewrite, the no-merge injection,
    // and the env carrier all read the ONE merge-posture table, so every
    // spawn lane judges the same vocabulary.
    let normalized = posture::normalize_legacy_no_merge(&resolved_command);
    if normalized != resolved_command {
        resolved_command = normalized;
        decision.push("command=legacy-no-merge->--no-merge".to_string());
    }
    if posture_value.as_deref() == Some("no-merge") {
        let injected = posture::inject_no_merge(&resolved_command);
        if injected != resolved_command {
            resolved_command = injected;
            decision.push("merge-posture=no-merge(injected)".to_string());
        }
    }
    let mut env = BTreeMap::new();
    if posture::message_carries_no_merge(&resolved_command) {
        env.insert("TARGET_NO_MERGE".to_string(), "1".to_string());
        decision.push("no-merge->TARGET_NO_MERGE".to_string());
    }
    if let Some(brief) = input.brief.filter(|b| !b.is_empty()) {
        let n_bytes = brief.as_bytes().len();
        if n_bytes > BRIEF_MAX_BYTES {
            return Err(format!(
                "dispatch brief is {n_bytes} bytes, over the {}-byte \
                 (8 KB) env budget; shorten it (no silent truncation)",
                BRIEF_MAX_BYTES
            ));
        }
        env.insert("TARGET_BRIEF".to_string(), brief.to_string());
        decision.push(format!("brief={n_bytes}B->TARGET_BRIEF"));
    }

    Ok(DispatchResolution {
        harness: chosen_harness,
        substrate: chosen_substrate,
        route: route_value,
        command: resolved_command,
        command_name: lifecycle_command,
        env,
        decision,
    })
}

/// The dispatch config rung (the `_load_dispatch_cfg` dict): the stage-table
/// harness with the deprecated `dispatch.harness` folded beneath it, the
/// command's route lane, the config.dispatch substrate/command/command extension,
/// and the `config.auto_merge.grant` actor key. A missing or unreadable key
/// degrades to the default so a resolve never bricks on config; only the
/// literal "dispatch" grant reads true.
pub fn dispatch_cfg_for(node_cwd: Option<&str>, command_name: &str) -> DispatchCfg {
    let cwd = node_cwd
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let lookup =
        |keys: &[&str]| -> Option<toml::Value> { crate::agents_config::config_lookup(&cwd, keys) };
    let text_key = |keys: &[&str]| -> Option<String> {
        lookup(keys)
            .and_then(|v| v.as_str().map(str::to_string))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let profile_command = canonical_command_key(command_name.trim());
    let profile_command = profile_command.trim_start_matches('/').to_string();
    let stage = text_key(&["agents", "profiles", &profile_command, "provider"]);
    let route = text_key(&["agents", "profiles", &profile_command, "route"]).unwrap_or_default();
    let legacy = text_key(&["dispatch", "harness"]);
    let (harness, harness_note) = match (&stage, &legacy) {
        (Some(s), Some(l)) if l != s => (
            s.clone(),
            Some(format!(
                "harness=deprecated dispatch.harness={} ignored; \
                 agents.profiles.{}.provider={} is the home",
                py_repr(l),
                profile_command,
                py_repr(s)
            )),
        ),
        (Some(s), _) => (s.clone(), None),
        (None, Some(l)) => (l.clone(), None),
        (None, None) => (String::new(), None),
    };
    let allowed_commands = lookup(&["dispatch", "allowed_commands"])
        .or_else(|| lookup(&["dispatch", super::settings::LEGACY_ALLOWED_COMMANDS_KEY]))
        .and_then(|v| v.as_array().cloned())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.as_str().map(str::to_string))
                .collect::<Vec<String>>()
        });
    let command_registry = lookup(&["dispatch", "command_registry"])
        .or_else(|| lookup(&["dispatch", super::settings::LEGACY_COMMAND_REGISTRY_KEY]))
        .and_then(|v| v.as_table().cloned())
        .map(|table| {
            table
                .into_iter()
                .map(|(key, row)| (key, command_descriptor(&row)))
                .collect()
        })
        .unwrap_or_default();
    let auto_merge = text_key(&["auto_merge", "grant"]).as_deref() == Some("dispatch");
    DispatchCfg {
        harness,
        harness_note: harness_note.unwrap_or_default(),
        route,
        substrate: text_key(&["dispatch", "substrate"]).unwrap_or_default(),
        command: text_key(&["dispatch", "command"]).unwrap_or_default(),
        allowed_commands,
        command_registry,
        auto_merge,
    }
}

/// One registry descriptor row, tolerating a missing field (the Python
/// model defaults each).
fn command_descriptor(row: &toml::Value) -> CommandDescriptor {
    let text = |key: &str| -> String {
        row.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let invocations = row
        .get("invocations")
        .and_then(|v| v.as_table().cloned())
        .map(|table| {
            table
                .into_iter()
                .filter_map(|(h, v)| v.as_str().map(|s| (h, s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    CommandDescriptor {
        invocation: text("invocation"),
        invocations,
        requires: text("requires"),
        takes_node_id: row
            .get("takes_node_id")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        asserts: text("asserts"),
        session_phase: text("session_phase"),
    }
}

/// One capacity-grid lane pick: the resolved axes plus the lane's NAME, the
/// dispatcher's spelling - it names the lane, never a flag vocabulary. One
/// seam; a decline surfaces the walk's terminal chain line verbatim and
/// never refuses (the caller owns the no-model refusal). The transport fault
/// surfaces as `grid=unreadable (...)`.
pub struct LanePick {
    pub harness: Option<String>,
    pub model: Option<String>,
    pub route: Option<String>,
    pub account: Option<String>,
    pub lane: Option<String>,
    pub decline: Option<String>,
}

pub fn grid_lane_for(
    node: Option<&serde_json::Value>,
    model: Option<&str>,
    provider: Option<&str>,
    command: Option<&str>,
) -> LanePick {
    if provider.map(str::trim).unwrap_or("").len() > 0 || node.is_none() {
        return LanePick {
            harness: None,
            model: None,
            route: None,
            account: None,
            lane: None,
            decline: None,
        };
    }
    let node = node.unwrap_or(&serde_json::Value::Null);
    let profile_command = {
        let v = command.unwrap_or("target").trim().trim_start_matches('/');
        if v.is_empty() {
            "target".to_string()
        } else {
            v.to_string()
        }
    };
    // The node projection the walk judges (the Python transport's fields).
    let node_payload = serde_json::json!({
        "difficulty": node.get("difficulty").cloned().unwrap_or(serde_json::Value::Null),
        "priority": node.get("priority").cloned().unwrap_or(serde_json::Value::Null),
        "plan_path": node.get("plan_path").and_then(|v| v.as_str()).unwrap_or(""),
        "model": node.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        "provider": node.get("provider").and_then(|v| v.as_str()).unwrap_or(""),
        "effort": node.get("effort").and_then(|v| v.as_str()).unwrap_or(""),
    });
    let payload = serde_json::json!({
        "rung_base": format!("agents.profiles.{profile_command}"),
        "node": node_payload,
        "substrate": serde_json::Value::Null,
        "permission_mode": serde_json::Value::Null,
        "constrain_harness": serde_json::Value::Null,
        "explicit_lane": false,
        "gate_bypassed": std::env::var("FNO_SPAWN_GATE").as_deref() == Ok("0"),
        "role": serde_json::Value::Null,
        "protected_role": serde_json::Value::Null,
        "model_occupied": false,
        "work_command": profile_command,
        "explicit_model_value": model,
        "explicit_route_value": serde_json::Value::Null,
        "explicit_vendor_value": serde_json::Value::Null,
        "capacity_refresh": true,
    });
    let Ok(exe) = std::env::current_exe() else {
        return LanePick {
            harness: None,
            model: None,
            route: None,
            account: None,
            lane: None,
            decline: Some("grid=unreadable (no binary)".to_string()),
        };
    };
    let argv = vec![
        "route-slot".to_string(),
        serde_json::to_string(&payload).unwrap_or_default(),
    ];
    let out = match crate::backlog::advance::bounded_command_env(&exe, &argv, 90, &[]) {
        Ok(out) if out.code == 0 => out,
        Ok(out) => {
            let head: String = out.stderr.trim().chars().take(80).collect();
            return LanePick {
                harness: None,
                model: None,
                route: None,
                account: None,
                lane: None,
                decline: Some(format!("grid=unreadable (exit {} {head})", out.code)),
            };
        }
        Err(e) => {
            return LanePick {
                harness: None,
                model: None,
                route: None,
                account: None,
                lane: None,
                decline: Some(format!("grid=unreadable ({e})")),
            };
        }
    };
    let answer: serde_json::Value =
        serde_json::from_str(out.stdout.trim()).unwrap_or(serde_json::Value::Null);
    let chain: Vec<String> = answer
        .get("chain")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|r| match r {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let terminal = chain
        .last()
        .cloned()
        .unwrap_or_else(|| "grid=no-reason-recorded".to_string());
    let Some(candidate) = answer.get("candidate").filter(|c| !c.is_null()) else {
        return LanePick {
            harness: None,
            model: None,
            route: None,
            account: None,
            lane: None,
            decline: Some(terminal),
        };
    };
    let pinned_undeclared = model.is_some()
        && !candidate
            .get("pin_row")
            .is_some_and(|p| !p.is_null() && p.as_bool() != Some(false));
    if pinned_undeclared {
        // A pinned model the rows do not declare dispatches on today's
        // default; the spawn's --node vendor refusal catches a mismatched
        // pairing there.
        return LanePick {
            harness: None,
            model: None,
            route: None,
            account: None,
            lane: None,
            decline: Some(terminal),
        };
    }
    let text_field = |key: &str| -> Option<String> {
        candidate
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    LanePick {
        harness: text_field("harness"),
        model: text_field("model"),
        route: text_field("route"),
        account: text_field("account"),
        lane: text_field("lane"),
        decline: None,
    }
}
