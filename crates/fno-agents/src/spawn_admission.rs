//! Spawn admission: the duties the Python seam (`rust_runtime.make_context`)
//! runs before it execs this client, ported for the in-process arm (agents
//! port group 2, row 10): the seed refusals, the node seed rewrite,
//! normalization, the compose and the headroom account pick.
//!
//! Four seam duties are not here yet and land with the client wiring: the
//! codex git-grant refusal, `extract_existing_pane`, the worker-dir export and
//! the codex sandbox probe. `Admission::events` are the compose's
//! `model_vendor_mismatch` rows, for the caller to emit.
//!
//! [`admit`] is not on the client's spawn path yet. Python `cmd_spawn` still
//! serves three shapes no Rust arm owns: a `--role` spawn, `--route` with an
//! account flag, and a routed non-thread spawn (`spawn_route::route_seam`
//! bounces the last two). The seam bounce and both Python legs stay until
//! those shapes have an owner here.
//!
//! The goldens (`tests/golden/spawn_seam/`, captured from the live Python
//! legs by `spawn_seam_golden_driver.py`) pin the scan projection, the
//! normalization and the refusal texts; `spawn_seam_parity.rs` replays them
//! against the fns here.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

use crate::claude_ask::py_repr;

// --------------------------------------------------------------------------- //
// The fence + flag vocabulary (spawn_defaults.py). Every scan stops at the
// `--argv` payload boundary and a bare `--` fence: fenced tokens are the
// harness's own flags, never fno's. Value-flag VALUES are skipped so a literal
// occurrence of one flag can never be misread out of another flag's value.
// --------------------------------------------------------------------------- //

const VALUE_FLAGS: &[&str] = &[
    "--provider",
    "-P",
    "--harness",
    "-H",
    "--model",
    "-m",
    "--effort",
    "--from",
    "--cwd",
    "-c",
    "--message",
    "--session-id",
    "--cc-session-id",
    "--channel-id",
    "--status",
    "--from-name",
    "--timeout",
    "-t",
    "--mode",
    "--substrate",
    "--as",
    "--permission-mode",
    "--output-format",
    "--monitor",
    "--tab",
    "--node",
];

/// The spawn-only value options on top of [`VALUE_FLAGS`]. Kept in lockstep
/// with the deleted Python table: a missing entry misreads that flag's value
/// as a positional (a nameless `spawn --route zai,glm-5.2` once registered an
/// agent named after a route value).
const SPAWN_VALUE_FLAGS_EXTRA: &[&str] = &[
    "--role",
    "--resume",
    "-r",
    "--add-dir",
    "--agent",
    "--tools",
    "--deny-tools",
    "--workspace",
    "--squad",
    "-s",
    "--split",
    "-x",
    "--tab",
    "--pane",
    "--node",
    "--node-reason",
    "--slug",
    "--plan",
    "--name",
    "--recorded-provider",
    "--route",
    "--account",
    "--promote",
    "--dispatch-account",
    "--take-over",
    "--at",
    "--portal",
    "--session-phase",
    "--sandbox-write-policy",
    "--wait",
    "--prompt-file",
    "--mux-session",
];

const HARNESS_FLAGS: &[&str] = &["--harness", "-H"];
const MODEL_FLAGS: &[&str] = &["--model", "-m"];
const EFFORT_FLAGS: &[&str] = &["--effort"];

fn is_value_flag(tok: &str) -> bool {
    VALUE_FLAGS.contains(&tok) || SPAWN_VALUE_FLAGS_EXTRA.contains(&tok)
}

/// `(index, token)` pairs up to the `--argv` boundary and the bare `--` fence,
/// skipping each value flag's consumed value.
fn spawn_tokens(toks: &[String]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut skip = false;
    for (i, t) in toks.iter().enumerate() {
        if t == "--argv" || t == "--" {
            break;
        }
        if skip {
            skip = false;
            continue;
        }
        out.push((i, t.clone()));
        if is_value_flag(t) && !t.contains('=') {
            skip = true;
        }
    }
    out
}

/// Value of the first of `flags` found as `--flag value`, `--flag=value`, or
/// the glued two-char short form (`-Pvalue`).
pub(crate) fn flag_value(toks: &[String], flags: &[&str]) -> Option<String> {
    for (i, t) in spawn_tokens(toks) {
        for f in flags {
            if &t == f {
                return toks.get(i + 1).cloned();
            }
            if let Some(rest) = t.strip_prefix(&format!("{f}=")) {
                return Some(rest.to_string());
            }
            if f.len() == 2 && f.ends_with(|c: char| c != '-') && t != *f && t.starts_with(*f) {
                return Some(t[f.len()..].to_string());
            }
        }
    }
    None
}

fn flag_present(toks: &[String], flag: &str) -> bool {
    spawn_tokens(toks)
        .iter()
        .any(|(_, t)| t == flag || t.starts_with(&format!("{flag}=")))
}

fn positional_indices(toks: &[String]) -> Vec<usize> {
    let mut idxs = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if t == "--argv" || t == "--" {
            break;
        }
        if t.starts_with('-') {
            if !t.contains('=') && is_value_flag(t) {
                i += 2;
            } else {
                i += 1;
            }
        } else {
            idxs.push(i);
            i += 1;
        }
    }
    idxs
}

/// Where the MESSAGE seed lives: `(index, form)` into `toks`, or None.
fn seed_slot(toks: &[String]) -> Option<(usize, &'static str)> {
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if t == "--argv" {
            break;
        }
        if t == "--" {
            let head_pos = positional_indices(&toks[..i]);
            if let Some(first) = head_pos.first() {
                return Some((*first, "positional"));
            }
            return if i + 1 < toks.len() {
                Some((i + 1, "fenced"))
            } else {
                None
            };
        }
        if t == "--message" {
            return if i + 1 < toks.len() {
                Some((i + 1, "message"))
            } else {
                None
            };
        }
        if t.starts_with("--message=") {
            return Some((i, "message_eq"));
        }
        i += 1;
    }
    positional_indices(toks)
        .first()
        .map(|first| (*first, "positional"))
}

/// The MESSAGE seed: the `--message` value, else the sole positional.
pub(crate) fn seed_of(toks: &[String]) -> Option<String> {
    let (i, form) = seed_slot(toks)?;
    let text = &toks[i];
    if form == "message_eq" {
        text.split_once('=').map(|(_, v)| v.to_string())
    } else {
        Some(text.clone())
    }
}

fn role_of(toks: &[String]) -> Option<String> {
    for (i, t) in spawn_tokens(toks) {
        if t == "--role" {
            return toks.get(i + 1).cloned();
        }
        if let Some(rest) = t.strip_prefix("--role=") {
            return Some(rest.to_string());
        }
    }
    None
}

fn has_permission_mode(toks: &[String]) -> bool {
    for t in toks {
        if t == "--argv" || t == "--" {
            break;
        }
        if t == "--permission-mode" || t == "--yolo" || t == "-Y" {
            return true;
        }
        if t.starts_with("--permission-mode=") {
            return true;
        }
    }
    false
}

/// The substrate value if pinned by an explicit flag, else None. `-o/--once`
/// spell headless. The two-token form returns the FOLLOWING token (an empty
/// string when absent), exactly the deleted leg's shape.
pub(crate) fn has_explicit_substrate(toks: &[String]) -> Option<String> {
    let mut skip = false;
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if t == "--argv" || t == "--" {
            break;
        }
        if skip {
            skip = false;
            i += 1;
            continue;
        }
        if t == "-o" || t == "--once" {
            return Some("headless".to_string());
        }
        if t == "--substrate" || t == "--as" {
            return Some(toks.get(i + 1).cloned().unwrap_or_default());
        }
        if let Some(rest) = t
            .strip_prefix("--substrate=")
            .or_else(|| t.strip_prefix("--as="))
        {
            return Some(rest.to_string());
        }
        if is_value_flag(t) && !t.contains('=') {
            skip = true;
        }
        i += 1;
    }
    None
}

/// One pass over a spawn argv (command already stripped by the caller):
/// `(harness_present, harness_value, model_present, effort_present)`. The
/// value read is position-based (the token after THIS occurrence), not a
/// first-occurrence lookup.
fn scan_axes(toks: &[String]) -> (bool, Option<String>, bool, bool) {
    let mut harness_present = false;
    let mut harness_value: Option<String> = None;
    let mut model_present = false;
    let mut effort_present = false;
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if t == "--argv" || t == "--" {
            break;
        }
        let (key, eq) = match t.split_once('=') {
            Some((k, _)) => (k, true),
            None => (t.as_str(), false),
        };
        if HARNESS_FLAGS.contains(&key) {
            harness_present = true;
            if eq {
                harness_value = t.split_once('=').map(|(_, v)| v.to_string());
                i += 1;
            } else {
                harness_value = toks.get(i + 1).cloned();
                i += 2;
            }
        } else if MODEL_FLAGS.contains(&key) {
            model_present = true;
            i += if eq { 1 } else { 2 };
        } else if EFFORT_FLAGS.contains(&key) {
            effort_present = true;
            i += if eq { 1 } else { 2 };
        } else if VALUE_FLAGS.contains(&key) && !eq {
            i += 2;
        } else {
            i += 1;
        }
    }
    (
        harness_present,
        harness_value,
        model_present,
        effort_present,
    )
}

/// The model a fenced passthrough names (`mux_spawn.passthrough_model`), the
/// last one winning as the CLI reads it.
fn fenced_flag_value(tail: &[String], long: &str, short: &str) -> Option<String> {
    let at = tail.iter().position(|t| t == "--")?;
    let mut found: Option<String> = None;
    for i in at + 1..tail.len() {
        let tok = &tail[i];
        if (tok == long || tok == short) && i + 1 < tail.len() {
            found = Some(tail[i + 1].clone());
        } else if let Some(rest) = tok.strip_prefix(&format!("{long}=")) {
            found = Some(rest.to_string());
        }
    }
    found
}

/// The compose-transport facts the deleted Python transport sent as the
/// `compose` payload's `scan`. The scanners answer what the OPERATOR typed;
/// the compose owns every config decision over the result.
pub fn scan_projection(tail: &[String]) -> Value {
    let (has_harness, explicit_harness, has_model, has_effort) = scan_axes(tail);
    let has_fenced = tail.iter().any(|t| t == "--");
    // A fenced --model (the -m shorthand lands there) occupies the model axis.
    let has_model = if has_fenced && !has_model {
        fenced_flag_value(tail, "--model", "-m").is_some()
    } else {
        has_model
    };
    let has_effort = if has_fenced && !has_effort {
        fenced_flag_value(tail, "--effort", "--effort").is_some()
    } else {
        has_effort
    };
    let explicit_route = flag_present(tail, "--route");
    let has_permission = has_permission_mode(tail);
    let mut permission_value = flag_value(tail, &["--permission-mode"]).filter(|v| !v.is_empty());
    if permission_value.is_none() && has_permission {
        permission_value = Some("yolo".to_string());
    }
    let slot = seed_slot(tail);
    json!({
        "has_harness": has_harness,
        "explicit_harness": explicit_harness,
        "has_model": has_model,
        "has_effort": has_effort,
        "explicit_vendor": flag_value(tail, &["--provider", "-P"]),
        "explicit_route": explicit_route,
        "route_value": if explicit_route { flag_value(tail, &["--route"]) } else { None },
        "model_value": if has_model { flag_value(tail, MODEL_FLAGS) } else { None },
        "role": role_of(tail),
        "explicit_substrate": has_explicit_substrate(tail),
        "permission_value": permission_value,
        "has_permission": has_permission,
        "flag_node": flag_value(tail, &["--node"]),
        "seed": seed_of(tail),
        "seed_index": slot.map(|(i, _)| i + 1),
        "seed_form": slot.map(|(_, f)| f),
        "account_flag_present": flag_present(tail, "--account"),
        "tab_flag_present": flag_present(tail, "--tab"),
        "name": flag_value(tail, &["--name"]),
        "positional_present": !positional_indices(tail).is_empty(),
        "spawn_tokens": tail,
    })
}

/// Two-word slug lists for a nameless spawn (docker/heroku pattern). The
/// deleted Python lists, verbatim; the goldens pin membership, not a draw.
const SLUG_ADJ: &[&str] = &[
    "amber", "brave", "calm", "clever", "coral", "cosmic", "crisp", "dapper", "eager", "fabled",
    "gentle", "glossy", "golden", "hardy", "jolly", "keen", "lively", "lucid", "mellow", "merry",
    "nimble", "noble", "plucky", "quiet", "rapid", "ruddy", "sage", "sleek", "snug", "spry",
    "stout", "sunny", "swift", "tidy", "vivid", "warm", "witty", "zesty", "bold", "bright",
];

const SLUG_NOUN: &[&str] = &[
    "otter", "falcon", "willow", "cedar", "comet", "ember", "harbor", "meadow", "pebble", "quartz",
    "river", "summit", "thicket", "vale", "walrus", "yak", "badger", "bison", "cobra", "crane",
    "dingo", "eagle", "ferret", "gecko", "heron", "ibis", "jaguar", "koala", "lemur", "marten",
    "newt", "osprey", "puffin", "raven", "shrew", "tapir", "urchin", "viper", "wombat", "finch",
];

/// Best-effort collision-avoided `adjective-noun` slug. Regenerates up to 5
/// times on a registry hit; the flocked downstream check remains
/// authoritative. The draw is a wall-clock/pid LCG: no rand dependency, and
/// the goldens pin membership in the word lists, never a draw.
fn mint_slug(existing: &HashSet<String>) -> Result<String, ()> {
    let mut state =
        crate::accounts::state::wall_now().to_bits() ^ ((std::process::id() as u64) << 32);
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    for _ in 0..5 {
        if let Ok(slug) = adj_noun(&mut next, existing) {
            return Ok(slug);
        }
    }
    Err(())
}

/// The `agents.naming.worker` name of a node-driven or `/target` spawn, with
/// the `-2`..`-5` suffix loop the deleted leg ran. Any failure returns None
/// and the adjective-noun mint runs: a spawn must never die on a naming
/// lookup.
fn mint_node_name(
    cwd: &Path,
    node: Option<&str>,
    seed: Option<&str>,
    slug_flag: Option<&str>,
    existing: &HashSet<String>,
) -> Option<String> {
    if node.filter(|n| !n.is_empty()).is_none() && seed.filter(|s| !s.is_empty()).is_none() {
        return None;
    }
    let name = crate::naming::worker_mint(node, seed, slug_flag, cwd).1?;
    for n in 1..=5 {
        let candidate = if n == 1 {
            name.clone()
        } else {
            format!("{name}-{n}")
        };
        if !existing.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// The outcome of one normalization pass: the rewritten argv, the stderr
/// notes it printed, and the refusal exit when the pass refused.
pub struct Normalized {
    pub argv: Vec<String>,
    pub stderr: Vec<String>,
    pub exit: Option<i32>,
}

/// The harness flags a spawn accepts before the fence as shorthand for the
/// same flag behind it, each mapped to its long spelling. Each takes one value.
fn fence_shorthand_long(tok: &str) -> Option<&'static str> {
    match tok {
        "--model" | "-m" => Some("--model"),
        "--effort" => Some("--effort"),
        "--permission-mode" => Some("--permission-mode"),
        "--add-dir" => Some("--add-dir"),
        "--agent" => Some("--agent"),
        "--tools" => Some("--tools"),
        _ => None,
    }
}

/// Move the head shorthands behind the `--` fence. A prompt-less spawn keeps
/// an explicit empty MESSAGE, or the first fenced token would become the
/// prompt.
fn fence_model_shorthand(toks: &[String]) -> Result<Vec<String>, (String, i32)> {
    let cut = toks.iter().position(|t| t == "--").unwrap_or(toks.len());
    let mut kept: Vec<String> = Vec::new();
    let mut moved: Vec<String> = Vec::new();
    let mut skip = false;
    for tok in &toks[..cut] {
        if skip {
            moved.push(tok.clone());
            skip = false;
            continue;
        }
        if let Some(long) = fence_shorthand_long(tok) {
            moved.push(long.to_string());
            skip = true;
            continue;
        }
        // Only a long flag takes the `=` spelling: `-m=x` stays where it is.
        match tok.split_once('=') {
            Some((flag, value))
                if flag.starts_with("--") && fence_shorthand_long(flag).is_some() =>
            {
                moved.push(fence_shorthand_long(flag).unwrap_or(flag).to_string());
                moved.push(value.to_string());
            }
            _ => kept.push(tok.clone()),
        }
    }
    if moved.is_empty() {
        return Ok(toks.to_vec());
    }
    if moved.len() % 2 == 1 {
        moved.push(String::new());
    }
    if moved.chunks(2).any(|p| p[1].is_empty()) {
        let mut names: Vec<String> = moved.chunks(2).map(|p| p[0].clone()).collect();
        names.sort();
        names.dedup();
        return Err((
            format!(
                "fno agents spawn: {} needs a value; no worker launched",
                names.join(" / ")
            ),
            2,
        ));
    }
    let mut out = kept.clone();
    if positional_indices(&kept).is_empty() {
        out.push(String::new());
    }
    let tail: Vec<String> = if cut < toks.len() {
        toks[cut + 1..].to_vec()
    } else {
        Vec::new()
    };
    let mut fence = out;
    fence.push("--".to_string());
    fence.extend(moved);
    fence.extend(tail);
    Ok(fence)
}

/// Canonicalize a `spawn` argv (command at index 0); pure argv -> argv. The
/// single positional is the MESSAGE; the name rides `--name` and is minted
/// when omitted. A second positional refuses with exit 2. Read-only.
pub fn normalize(
    args: &[String],
    existing_names: Option<&HashSet<String>>,
    cwd: &Path,
) -> Normalized {
    let mut notes: Vec<String> = Vec::new();
    if args.first().map(String::as_str) != Some("spawn") {
        return Normalized {
            argv: args.to_vec(),
            stderr: notes,
            exit: None,
        };
    }
    for a in &args[1..] {
        if a == "--argv" {
            break;
        }
        if a == "-h" || a == "--help" {
            return Normalized {
                argv: args.to_vec(),
                stderr: notes,
                exit: None,
            };
        }
    }
    let body = &args[1..];
    let (toks, payload): (Vec<String>, Vec<String>) = match body.iter().position(|t| t == "--argv")
    {
        Some(cut) => (body[..cut].to_vec(), body[cut..].to_vec()),
        None => (body.to_vec(), Vec::new()),
    };
    let toks = if payload.is_empty() {
        match fence_model_shorthand(&toks) {
            Ok(t) => t,
            Err((line, code)) => {
                notes.push(line);
                return Normalized {
                    argv: args.to_vec(),
                    stderr: notes,
                    exit: Some(code),
                };
            }
        }
    } else {
        toks
    };
    let fence = toks.iter().position(|t| t == "--");
    let head = match fence {
        Some(f) => &toks[..f],
        None => &toks[..],
    };
    let hand_off = head
        .iter()
        .any(|t| t == "--hand-off" || t.starts_with("--hand-off="));
    let named = head
        .iter()
        .any(|t| t == "--name" || t.starts_with("--name="));
    let toks = if !hand_off && !named {
        let names: HashSet<String> = existing_names.cloned().unwrap_or_default();
        let seeds = positional_indices(head);
        let minted = mint_node_name(
            cwd,
            flag_value(head, &["--node"]).as_deref(),
            seeds.first().map(|i| head[*i].as_str()),
            flag_value(head, &["--slug"]).as_deref(),
            &names,
        );
        let name = match minted {
            Some(n) => n,
            None => match mint_slug(&names) {
                Ok(n) => n,
                Err(()) => {
                    notes.push("fno agents spawn: could not find a free auto-name after 5 tries; pass one explicitly".to_string());
                    return Normalized {
                        argv: args.to_vec(),
                        stderr: notes,
                        exit: Some(2),
                    };
                }
            },
        };
        let mut with_name = vec!["--name".to_string(), name];
        with_name.extend(toks);
        with_name
    } else {
        toks
    };
    let extra = positional_indices(&toks);
    if extra.len() > 1 {
        let listed: Vec<String> = extra.iter().map(|i| py_repr(&toks[*i])).collect();
        notes.push(format!(
            "fno agents spawn: takes one positional (the prompt); got {} ({}). The agent name moved to --name, so pass `--name <n> \"<prompt>\"`.",
            extra.len(),
            listed.join(", ")
        ));
        return Normalized {
            argv: args.to_vec(),
            stderr: notes,
            exit: Some(2),
        };
    }
    let mut out = vec!["spawn".to_string()];
    out.extend(toks);
    out.extend(payload);
    Normalized {
        argv: out,
        stderr: notes,
        exit: None,
    }
}

/// One draw attempt: a slug from the two lists that is not already taken.
fn adj_noun(next: &mut impl FnMut() -> usize, existing: &HashSet<String>) -> Result<String, ()> {
    let slug = format!(
        "{}-{}",
        SLUG_ADJ[next() % SLUG_ADJ.len()],
        SLUG_NOUN[next() % SLUG_NOUN.len()]
    );
    if existing.contains(&slug) {
        Err(())
    } else {
        Ok(slug)
    }
}

// --------------------------------------------------------------------------- //
// Pre-route seed refusals. The texts are the deleted Python legs', byte for
// byte; the goldens pin them.
// --------------------------------------------------------------------------- //

/// The refusal for a fresh claude thread spawn with no message. A resume
/// continues a transcript and a role spawn gets the term command typed later
/// in dispatch, so neither needs a message here.
pub fn seedless_thread_refusal(
    harness: Option<&str>,
    substrate: Option<&str>,
    message: Option<&str>,
    resume: Option<&str>,
    role: bool,
    name: Option<&str>,
    node: Option<&str>,
) -> Option<String> {
    if harness != Some("claude") || !matches!(substrate, Some("thread") | Some("bg")) {
        return None;
    }
    if message.map(|m| !m.trim().is_empty()).unwrap_or(false) || resume.is_some() || role {
        return None;
    }
    Some(format!(
        "refusing a claude thread spawn with no message. Claude starts that session with no prompt, it waits for one forever, and its row holds a worker slot. Put the work in the message: fno agents spawn '/fno:target {node}' --name {name} --node {node}. A revival needs no message. No worker launched.",
        name = name.unwrap_or("<name>"),
        node = node.unwrap_or("<node>"),
    ))
}

/// Whether `seed`'s FIRST token is a token the codex session cannot expand.
pub fn is_command_seed(seed: Option<&str>) -> bool {
    seed.and_then(|s| s.split_whitespace().next())
        .and_then(crate::provider::parse_command_token)
        .is_some()
}

/// The refusal for a payload whose `$fno:` prefix the calling shell ate, or
/// None when the payload is intact. Inside double quotes a shell expands
/// `$fno` to nothing before fno runs; bash leaves `:target`, zsh leaves
/// `arget`. The zsh shape matches only for commands longer than four letters.
pub fn lost_command_refusal(message: &str) -> Option<String> {
    const ZSH_EATEN_LETTERS: &[u8] = b"acelqrtu";
    // The bare command names (the skill names plus the stripped target
    // family), exactly the set the deleted Python leg matched against.
    let mut commands: HashSet<String> = crate::provider::footnote_commands();
    commands.insert("target".to_string());
    commands.insert("blueprint".to_string());
    let mut lost: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for c in &commands {
        lost.insert(format!(":{c}"), c.clone());
        if c.len() > 4 {
            if let Some(first) = c.as_bytes().first() {
                if ZSH_EATEN_LETTERS.contains(first) {
                    lost.insert(c[1..].to_string(), c.clone());
                }
            }
        }
    }
    for token in message.split_whitespace() {
        if let Some(command) = lost.get(token) {
            return Some(format!(
                "the payload has {} where '$fno:{command}' belongs. The shell expanded $fno to nothing inside double quotes, and zsh also drops the first letter of the command. Single-quote the payload: '$fno:{command} ...'",
                py_repr(token)
            ));
        }
    }
    None
}

/// The refusal for a command-shaped seed whose command cannot expand, or
/// None. `plugin_status` is the `fno plugin-install --status --json codex`
/// verdict's `status` field, resolved by the caller (in-process at the
/// client); None or any other value fails open, because the spawn fails on
/// its own there.
pub fn cannot_fire_refusal(
    message: &str,
    harness: &str,
    plugin_status: Option<&str>,
) -> Option<String> {
    if harness != "codex" || !is_command_seed(Some(message)) {
        return None;
    }
    if matches!(plugin_status, Some("missing") | Some("wrong-channel")) {
        return Some(format!(
            "the seed invokes {} but the footnote plugin is not enabled for codex on this machine, so the command would not fire and the worker would read the seed as prose. Install it with 'fno config plugin install codex' and spawn again.",
            py_repr(message.split_whitespace().next().unwrap_or("")),
        ));
    }
    None
}

/// footnote's headless arm reads `--once`, never `--as`: rewrite the
/// `--as headless` spellings for a footnote-harness spawn. One pass, up to
/// the `--argv` payload boundary; the two-token form consumes its value.
pub fn footnote_headless_as_once(args: &[String]) -> Vec<String> {
    let head_len = args
        .iter()
        .position(|t| t == "--argv" || t == "--")
        .unwrap_or(args.len());
    if args.first().map(String::as_str) != Some("spawn") {
        return args.to_vec();
    }
    if !flag_value(args, HARNESS_FLAGS)
        .map(|h| h.eq_ignore_ascii_case("footnote"))
        .unwrap_or(false)
    {
        return args.to_vec();
    }
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for (i, t) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if i < head_len
            && (t == "--as=headless"
                || (t == "--as" && args.get(i + 1).map(String::as_str) == Some("headless")))
        {
            out.push("--once".to_string());
            if t == "--as" {
                skip_next = true;
            }
            continue;
        }
        out.push(t.clone());
    }
    out
}

/// The admission result: the argv to parse and dispatch, the stderr lines to
/// print, the refusal exit when this refused, and the take-over reason the
/// compose consumed from the flags.
pub struct Admission {
    pub argv: Vec<String>,
    pub stderr: Vec<String>,
    pub exit: Option<i32>,
    pub stdout: Option<Value>,
    pub take_over: Option<String>,
    pub events: Vec<Value>,
}

impl Admission {
    fn refused(stderr: Vec<String>, text: String) -> Admission {
        let mut stderr = stderr;
        stderr.push(format!("fno agents spawn: {text}"));
        Admission {
            argv: Vec::new(),
            stderr,
            exit: Some(2),
            stdout: None,
            take_over: None,
            events: Vec::new(),
        }
    }
}

/// The dispatch harness for a pre-route refusal: the explicit flag, else the
/// invoking harness the ambient markers name, else the builtin `claude`.
/// None for an explicit empty value, which the deleted leg skipped on.
fn dispatch_harness(tail: &[String]) -> Option<String> {
    match flag_value(tail, HARNESS_FLAGS) {
        Some(explicit) => {
            let harness = explicit.trim().to_string();
            (!harness.is_empty()).then_some(harness)
        }
        None => Some(crate::claims::resolve_harness().unwrap_or_else(|| "claude".to_string())),
    }
}

/// Whether the fno-arg head (up to the `--argv` payload and the bare `--`
/// fence) carries a flag: a short as `-X`, `-X=V` or glued `-XV`, a long as
/// `--foo` or `--foo=V`.
fn has_flag(args: &[String], short: &str, longs: &[&str]) -> bool {
    args.iter()
        .take_while(|a| *a != "--argv" && *a != "--")
        .any(|a| {
            (!short.is_empty() && a.starts_with(short))
                || longs
                    .iter()
                    .any(|l| a == l || a.starts_with(&format!("{l}=")))
        })
}

/// `--resume` or `-r`, in any spelling.
fn resume_bearing(args: &[String]) -> bool {
    has_flag(args, "-r", &["--resume"])
}

/// The seed refusals on the RAW argv, in the deleted seam's order: a check
/// after normalization would read a minted name as the seed.
/// `plugin_status` is read only for a command seed bound for codex.
fn seed_refusals(tail: &[String], plugin_status: &dyn Fn() -> Option<String>) -> Option<String> {
    if let Some(substrate) = has_explicit_substrate(tail) {
        if let Some(harness) = dispatch_harness(tail) {
            let refusal = seedless_thread_refusal(
                Some(harness.as_str()),
                Some(substrate.as_str()),
                seed_of(tail).as_deref(),
                flag_value(tail, &["--resume"])
                    .filter(|r| !r.is_empty())
                    .as_deref(),
                flag_present(tail, "--promote"),
                flag_value(tail, &["--name"]).as_deref(),
                flag_value(tail, &["--node"]).as_deref(),
            );
            if refusal.is_some() {
                return refusal;
            }
        }
    }
    let seed = seed_of(tail)?;
    if let Some(text) = lost_command_refusal(&seed) {
        return Some(text);
    }
    if !is_command_seed(Some(&seed)) {
        return None;
    }
    let harness = dispatch_harness(tail)?;
    if harness != "codex" {
        return None;
    }
    cannot_fire_refusal(&seed, &harness, plugin_status().as_deref())
}

/// The codex plugin verdict's `status`, the `plugin-install --status` read.
fn codex_plugin_status() -> Option<String> {
    let verdict = crate::codex_converge::inspect(
        &mut crate::codex_converge::codex_runner,
        &crate::codex_converge::codex_home(),
    );
    verdict
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn argv_of(answer: &Value) -> Option<Vec<String>> {
    let arr = answer.get("argv").and_then(Value::as_array)?;
    let toks: Vec<String> = arr
        .iter()
        .map(|t| {
            t.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| t.to_string())
        })
        .collect();
    (!toks.is_empty()).then_some(toks)
}

/// The node seed rewrite: the same decide the spawn-axes door answers, in
/// process. Returns the (possibly rewritten) argv and the profile command a
/// `profile` answer names. A payload-named node's refusals name the
/// payload, not `--node`.
fn node_seed_rewrite(args: &[String]) -> Result<(Vec<String>, Option<String>), String> {
    let family = json!(["/target", "/blueprint"]);
    let mut args = args.to_vec();
    let mut node = flag_value(&args[1..], &["--node"])
        .map(|n| n.trim().to_string())
        .unwrap_or_default();
    let mut node_source = Value::Null;
    if node.is_empty() {
        let answer = crate::node_seed::decide(&json!({
            "family": family,
            "resume": resume_bearing(&args),
            "argv": args,
        }));
        // A `refuse` was the Python transport's stale-binary note; the decide
        // is in-process, so anything but `compose` reads as nodeless.
        if answer.get("action").and_then(Value::as_str) == Some("compose") {
            if let Some(rewritten) = argv_of(&answer) {
                args = rewritten;
            }
            node = flag_value(&args[1..], &["--node"])
                .map(|n| n.trim().to_string())
                .unwrap_or_default();
            if answer.get("source").and_then(Value::as_str) == Some("payload") {
                node_source = json!("payload");
            }
            if node.is_empty() {
                if let Some(reason) = flag_value(&args[1..], &["--node-reason"]) {
                    std::env::set_var("FNO_NODE_REASON", reason);
                    if let Some(at) = args.iter().position(|t| t == "--node-reason") {
                        args.drain(at..(at + 2).min(args.len()));
                    }
                }
            }
        }
    }
    if node.is_empty() {
        return Ok((args, None));
    }

    // The named-node pass: the row's lifecycle facts ride the same decide.
    let row = crate::spawn_compose::graph_row_for(&node);
    let (effective_command, derive_error) = match &row {
        Some(r) => match crate::backlog_ready::effective_command(r) {
            Ok((_, cmd)) => (Some(cmd), None),
            Err(e) => (None, Some(e)),
        },
        None => (None, None),
    };
    let stored = row
        .as_ref()
        .and_then(|r| r.get("dispatch_command"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(crate::backlog::fields::canonical_command_key)
        .filter(|s| !s.is_empty());
    let slot = seed_slot(&args[1..]);
    let answer = crate::node_seed::decide(&json!({
        "node": node,
        "row_found": row.is_some(),
        "effective_command": effective_command,
        "stored_command": stored,
        "derive_error": derive_error,
        "node_source": node_source,
        "family": family,
        "resume": resume_bearing(&args),
        "argv": args,
        "seed_index": slot.map(|(i, _)| i + 1),
        "seed_form": slot.map(|(_, f)| f),
    }));
    match answer.get("action").and_then(Value::as_str) {
        Some("refuse") => Err(answer
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("refused")
            .to_string()),
        Some("compose") => Ok((argv_of(&answer).unwrap_or(args), None)),
        Some("profile") => {
            let command = answer
                .get("verb") // verb-wording: allow (the decide's answer key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string);
            Ok((args, command))
        }
        _ => Ok((args, None)),
    }
}

/// A literal `--verbose` before the first `--` fence or `--argv` payload.
fn verbose_requested(argv: &[String]) -> bool {
    argv.iter()
        .skip(1)
        .take_while(|t| *t != "--" && *t != "--argv")
        .any(|t| t == "--verbose")
}

/// Run the full admission for one `spawn` argv: the seed refusals, the node
/// seed rewrite, normalization, the compose (config defaults) applied
/// in-process, the footnote once-rewrite and the headroom account pick. The
/// caller prints `stderr` and honors `exit` before parsing `argv`.
///
/// `existing_names` are the registry's live agent names (the caller loads the
/// registry once for its own collision check and passes the names here, so
/// admission never reads the registry twice).
pub fn admit(args: &[String], cwd: &Path, existing_names: &HashSet<String>) -> Admission {
    let mut stderr: Vec<String> = Vec::new();
    if args.first().map(String::as_str) != Some("spawn") {
        return pass_through(args);
    }
    let tail: Vec<String> = args[1..].to_vec();

    // 1. The seed refusals, on the raw argv (pre-normalization).
    if let Some(text) = seed_refusals(&tail, &codex_plugin_status) {
        return Admission::refused(stderr, text);
    }

    // 2. The node seed rewrite. A `profile` answer names the command the compose
    //    reads as the node's command.
    let (args, node_command) = match node_seed_rewrite(args) {
        Ok(rewritten) => rewritten,
        Err(text) => return Admission::refused(stderr, text),
    };

    // A help request skips the compose: its argv is the parser's to answer.
    if args[1..]
        .iter()
        .take_while(|a| *a != "--argv")
        .any(|a| a == "-h" || a == "--help")
    {
        let mut admitted = pass_through(&args);
        admitted.stderr = stderr;
        return admitted;
    }

    // 3. Normalization. A normalization refusal keeps its exit code and the
    //    notes it printed.
    let normalized = normalize(&args, Some(existing_names), cwd);
    stderr.extend(normalized.stderr);
    if let Some(code) = normalized.exit {
        return Admission {
            argv: normalized.argv,
            stderr,
            exit: Some(code),
            stdout: None,
            take_over: None,
            events: Vec::new(),
        };
    }
    let out = &normalized.argv;
    let out_tail = &out[1..];

    // 4. The compose: gather + compose in-process, apply the answer exactly
    //    where the deleted transport applied it.
    let scan = scan_projection(out_tail);
    let facts = json!({
        "role_resolves": Value::Null,
        "role_protected": scan
            .get("role")
            .and_then(Value::as_str)
            .map(|r| r.trim().to_lowercase())
            .filter(|r| PROTECTED_ROLES.contains(&r.as_str())),
    });
    let env_node = std::env::var("FNO_NODE").ok().filter(|v| !v.is_empty());
    let payload = json!({
        "argv": out,
        "node_command": node_command,
        "env_node": env_node,
        "permission_builtin": "bypassPermissions",
        "scan": scan,
        "facts": facts,
        "verbose": verbose_requested(out),
    });
    let inputs = crate::spawn_compose::gather(&payload, cwd);
    let answer = crate::spawn_compose::compose(&inputs);
    stderr.extend(answer.stderr.clone());
    if answer.exit != 0 {
        return Admission {
            argv: out.clone(),
            stderr,
            exit: Some(answer.exit),
            stdout: answer.stdout.clone(),
            take_over: None,
            events: answer.events.clone(),
        };
    }
    let mut argv: Vec<String> = if answer.argv.is_empty() {
        out.clone()
    } else {
        answer.argv.clone()
    };
    // The take-over reason the compose consumed rides the answer; re-inject it
    // as a flag so the parse below reads the legal shape (the deleted
    // `_with_seam_marker` did this at the exec).
    if !answer.take_over.is_empty() {
        argv.insert(1, format!("--take-over={}", answer.take_over));
    }

    // 5. The footnote once-rewrite, then the headroom account pick: the pick
    //    runs on the composed argv, exactly where the seam ran it.
    let argv = footnote_headless_as_once(&argv);
    let argv = pick_account_arg(&argv, cwd);

    Admission {
        argv,
        stderr,
        exit: None,
        stdout: None,
        take_over: (!answer.take_over.is_empty()).then(|| answer.take_over.clone()),
        events: answer.events.clone(),
    }
}

/// The protected roles whose spawns the compose facts name
/// (model_routing.PROTECTED_ROLES, the deleted transport's facts writer).
const PROTECTED_ROLES: &[&str] = &["implement", "review-verdict"];

/// A non-spawn argv passes through admission untouched.
fn pass_through(args: &[String]) -> Admission {
    Admission {
        argv: args.to_vec(),
        stderr: Vec::new(),
        exit: None,
        stdout: None,
        take_over: None,
        events: Vec::new(),
    }
}

/// Inject a headroom-picked `--account` for a spawn that named none. Six
/// spawns are left alone: one that already named an account, one carrying
/// `--role` or `--route`/`--provider`, one carrying `--dispatch-account`, one
/// carrying `--resume`, and one pinned to a non-claude harness. Advisory in
/// every direction: a failure never blocks the spawn.
fn pick_account_arg(args: &[String], cwd: &Path) -> Vec<String> {
    // The provenance carrier: drop any previous spawn's decision first, so a
    // stale "config" never outlives its pick.
    std::env::remove_var(crate::state::LAUNCH_ACCOUNT_SOURCE_ENV_KEY);
    let tail = &args[1..];
    if flag_value(tail, &["--account"]).is_some()
        || has_flag(args, "", &["--role"])
        || has_flag(args, "-P", &["--route", "--provider"])
        // A revive continues an EXISTING transcript under the config dir it
        // was created in, so an injected --account would point
        // CLAUDE_CONFIG_DIR at a directory where the uuid does not exist.
        || resume_bearing(args)
        // A cutover already SELECTED its account; a second pick merges two
        // overlays, the exact mis-bill the --route guard refuses.
        || has_flag(args, "", &["--dispatch-account"])
    {
        return args.to_vec();
    }
    let harness = flag_value(tail, HARNESS_FLAGS);
    if harness.is_some_and(|h| !h.is_empty() && h != "claude") {
        return args.to_vec();
    }
    let verdict = match crate::accounts::pick::pick_live(cwd, None, &[], true) {
        Ok(v) => v,
        Err(_) => return args.to_vec(),
    };
    let Some(account) = verdict.account else {
        return args.to_vec();
    };
    // The mint tells an injected pick from a flag the operator typed.
    std::env::set_var(crate::state::LAUNCH_ACCOUNT_SOURCE_ENV_KEY, "config");
    let boundary = args
        .iter()
        .position(|a| a == "--argv" || a == "--")
        .unwrap_or(args.len());
    let mut out: Vec<String> = Vec::with_capacity(args.len() + 2);
    out.extend_from_slice(&args[..boundary]);
    out.push("--account".to_string());
    out.push(account);
    out.extend_from_slice(&args[boundary..]);
    out
}
