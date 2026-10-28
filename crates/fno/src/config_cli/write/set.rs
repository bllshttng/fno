//! `fno config set`: one key (`<key> <value>`) or an atomic batch
//! (`a=1 b=2`), coerced, validated and written in one locked pass. After the
//! write it warns when a higher-precedence file overrides the value, and it
//! couples `pr_watch.enabled` and an armed dispatch grant to the pr-watch
//! agent (loud on failure, never reverting the config).

use super::{
    deep_set, final_value, is_merge_gating, locked_update, optout_release_command, parse_argv,
    prepare_leases, resolve_parent_block, storage_parts, target_path, validate_block, Lease,
    LeaseBook, Parsed, Refusal, Res,
};
use crate::config_cli::model::{self, py_repr, py_str};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The `--help` page typer printed, captured with the goldens.
const HELP: &str = include_str!("../../../tests/golden/config_set/set_help.out");

/// One written key, as the command reports it.
pub struct SetResult {
    pub key: String,
    pub value: Value,
    pub path: PathBuf,
    pub scope: String,
    pub lease: Option<Lease>,
}

/// `set_config_values`: all-or-nothing. A key repeated in one batch uses its
/// last value; every key resolves before the lock, every value coerces and
/// every touched block validates under it, and nothing is written unless
/// all of them pass. `budget` bounds the lock wait (`None` blocks).
pub fn set_values(
    items: &[(String, String)],
    scope: &str,
    budget: Option<Duration>,
) -> Res<Vec<SetResult>> {
    if items.is_empty() {
        return Err(Refusal::new("no key=value pairs given", 2));
    }
    let mut order: Vec<String> = Vec::new();
    let mut values: HashMap<String, String> = HashMap::new();
    for (key, value) in items {
        if !values.contains_key(key) {
            order.push(key.clone());
        }
        values.insert(key.clone(), value.clone());
    }
    let mut resolved = Vec::new();
    for key in &order {
        let Some((owner, field)) = resolve_parent_block(key) else {
            return Err(Refusal::new(
                format!(
                    "unknown config key {}",
                    py_repr(&Value::String(key.clone()))
                ),
                1,
            ));
        };
        resolved.push((key.clone(), owner, field));
    }
    let target = target_path(scope)?;
    let touched = order.iter().any(|key| is_merge_gating(key));
    let mut finals: Vec<(String, Value)> = Vec::new();
    let mut book = LeaseBook::default();
    let mutate = |existing: Value| -> Res<Value> {
        let mut data = existing.clone();
        let mut blocks: Vec<(Vec<String>, String)> = Vec::new();
        for (key, owner, field) in &resolved {
            let (value, block) = final_value(key, &values[key], owner, field)?;
            deep_set(&mut data, &storage_parts(key), value.clone());
            finals.push((key.clone(), value));
            if let Some((parts, owner)) = block {
                match blocks.iter_mut().find(|(seen, _)| *seen == parts) {
                    Some(slot) => slot.1 = owner,
                    None => blocks.push((parts, owner)),
                }
            }
        }
        for (parts, owner) in &blocks {
            validate_block(&data, parts, owner)?;
        }
        if touched {
            prepare_leases(&order, &existing, &data, &target, scope, &mut book)?;
        }
        Ok(data)
    };
    let written = match locked_update(&target, scope, budget, mutate) {
        Ok(written) => written,
        Err(refusal) => {
            book.rollback();
            return Err(refusal);
        }
    };
    book.finalize()?;
    Ok(order
        .iter()
        .map(|key| SetResult {
            key: key.clone(),
            value: finals
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Null),
            path: written.path.clone(),
            scope: scope.to_string(),
            lease: book.lease(key).cloned(),
        })
        .collect())
}

pub fn run(args: &[OsString]) -> i32 {
    let (tokens, local) = match parse_argv("set", "TOKENS...", HELP, args) {
        Parsed::Exit(code) => return code,
        Parsed::Args { positionals, local } => (positionals, local),
    };
    let scope = if local { "project" } else { "global" };
    // Exactly two tokens whose first carries no '=' is the single
    // `<key> <value>` form, so a value may itself contain '='.
    let items: Vec<(String, String)> = if tokens.len() == 2 && !tokens[0].contains('=') {
        vec![(tokens[0].clone(), tokens[1].clone())]
    } else {
        let mut items = Vec::new();
        for token in &tokens {
            let Some((key, value)) = token.split_once('=') else {
                eprintln!(
                    "error: expected key=value, got {}. Use `fno config set <key> <value>` for a single key.",
                    py_repr(&Value::String(token.clone()))
                );
                return 2;
            };
            items.push((key.to_string(), value.to_string()));
        }
        items
    };
    // A dangling accounts.active bricks every loader call, so the pointer is
    // checked against declared record ids before the write. A global pointer
    // must name a globally declared record; a project one may name any.
    for (key, value) in &items {
        let bare = key.strip_prefix("config.").unwrap_or(key);
        if matches!(bare, "accounts.active" | "providers.active") {
            let ids = account_ids(scope);
            if !ids.contains(value) {
                let known = if ids.is_empty() {
                    "(no records configured)".to_string()
                } else {
                    ids.iter().cloned().collect::<Vec<_>>().join(", ")
                };
                eprintln!(
                    "error: account {} is not a configured record id (known: {known}); add the record first",
                    py_repr(&Value::String(value.clone()))
                );
                return 1;
            }
        }
    }
    let results = match set_values(&items, scope, None) {
        Ok(results) => results,
        Err(refusal) => {
            eprintln!("error: {}", refusal.message);
            return refusal.code;
        }
    };
    if let [only] = results.as_slice() {
        println!(
            "set {} = {} ({}: {})",
            only.key,
            py_str(&only.value),
            only.scope,
            only.path.display()
        );
    } else {
        for result in &results {
            println!("set {} = {}", result.key, py_str(&result.value));
        }
        println!("({}: {})", results[0].scope, results[0].path.display());
    }
    for result in &results {
        if let Some(lease) = &result.lease {
            let flag = if result.scope == "project" {
                " --local"
            } else {
                ""
            };
            println!(
                "lease: {} held by {}, expires at {}; release: {}",
                result.key,
                lease.holder,
                py_str(&lease.expires_at),
                optout_release_command(&result.key, flag)
            );
        }
    }
    let settings = check_overridden(&results);
    if let Some(result) = results.iter().find(|r| r.key.ends_with("pr_watch.enabled")) {
        couple_pr_watch(truthy(&result.value), settings.as_ref());
    }
    if results
        .iter()
        .any(|r| r.key.ends_with("auto_merge.enabled") || r.key.ends_with("auto_merge.grant"))
    {
        couple_grant_observer();
    }
    0
}

/// The record ids an `accounts.active` pointer may name: the global file's
/// own records for a global write, every layer's for a project one.
fn account_ids(scope: &str) -> BTreeSet<String> {
    let layers: Vec<Value> = if scope == "global" {
        let dir = super::global_dir();
        let toml = std::env::var_os("FNO_CONFIG")
            .filter(|pin| !pin.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| dir.join("config.toml"));
        let path = if toml.is_file() {
            toml
        } else {
            dir.join("settings.yaml")
        };
        crate::config_cli::parse_config_file(&path)
            .map(|mut raw| {
                crate::config_cli::alias_legacy_keys(&mut raw);
                crate::config_cli::unwrap_config_dict(&raw)
            })
            .into_iter()
            .collect()
    } else {
        crate::config_cli::aliased_layers(&crate::config_cli::repo_root())
            .into_iter()
            .map(|(_, layer)| layer)
            .collect()
    };
    layers
        .iter()
        .filter_map(|layer| layer.get("accounts")?.get("records")?.as_array())
        .flatten()
        .filter_map(|record| record.get("id")?.as_str().map(str::to_string))
        .collect()
}

/// Python truthiness of a written value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// The validated value at a dotted key, spelled the way the writer reads it.
fn effective_at(settings: &Value, key: &str) -> Value {
    let mut node = settings;
    for part in storage_parts(key) {
        match node.get(&part) {
            Some(next) => node = next,
            None => return Value::Null,
        }
    }
    node.clone()
}

/// Warn when a write landed on disk but a higher-precedence file overrides
/// it: the write is inert in this project, and a silent success hid that.
/// Returns the settings it loaded, for the couplings below.
fn check_overridden(results: &[SetResult]) -> Option<Value> {
    let root = crate::config_cli::repo_root();
    let settings = match model::load_validated(&root) {
        Ok(settings) => settings,
        Err(refusal) => {
            eprintln!("Error: {refusal}");
            return None;
        }
    };
    for result in results {
        let Some((decider, _)) = crate::config_cli::resolve_source(&result.key, &root) else {
            continue;
        };
        let same_file = crate::config_cli::resolve_path(&decider)
            == crate::config_cli::resolve_path(&result.path);
        if same_file {
            continue;
        }
        let effective = effective_at(&settings, &result.key);
        if effective != result.value {
            let flag = if result.scope == "global" {
                "--local"
            } else {
                "--global"
            };
            eprintln!(
                "warn: {} set in {}, but {} overrides it (value in effect: {}). Target the winning file with {flag}.",
                result.key,
                result.path.display(),
                decider.display(),
                py_str(&effective)
            );
        }
    }
    Some(settings)
}

/// The last stdout word of an `fno-agents` door, bounded at 60 s; a word
/// outside `known`, a timeout or no binary reads as `fallback`.
fn door_word(args: &[String], known: &[&str], fallback: &str) -> String {
    let Some(binary) = crate::digest_overlay::fno_agents_bin_auto() else {
        return fallback.to_string();
    };
    let Ok(mut child) = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return fallback.to_string();
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return fallback.to_string();
            }
        }
    }
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stdout);
    }
    let word = stdout.trim();
    if known.contains(&word) {
        word.to_string()
    } else {
        fallback.to_string()
    }
}

/// Install and load the pr-watch agent if absent; never prompts.
fn ensure_watcher(settings: Option<&Value>) -> String {
    let loaded;
    let settings = match settings {
        Some(settings) => settings,
        None => match model::load_validated(&crate::config_cli::repo_root()) {
            Ok(value) => {
                loaded = value;
                &loaded
            }
            Err(_) => return "load-failed".to_string(),
        },
    };
    let interval = settings["pr_watch"]["interval_seconds"]
        .as_i64()
        .unwrap_or(600);
    let fno = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "fno".to_string());
    let interval = interval.to_string();
    let args = [
        "pr-watch",
        "install",
        "--ensure",
        "--fno-binary",
        fno.as_str(),
        "--interval",
        interval.as_str(),
    ]
    .map(str::to_string);
    door_word(
        &args,
        &[
            "already-running",
            "activated",
            "write-failed",
            "load-failed",
        ],
        "load-failed",
    )
}

fn couple_pr_watch(enabled: bool, settings: Option<&Value>) {
    if enabled {
        match ensure_watcher(settings).as_str() {
            "activated" => println!("pr-watch: agent installed and loaded."),
            "already-running" => println!("pr-watch: agent already running."),
            outcome => eprintln!(
                "pr-watch: WARNING enabled but activation failed ({outcome}); config stays enabled. Run `fno do pr watch install` or check `fno doctor`."
            ),
        }
        return;
    }
    let args = ["pr-watch", "uninstall", "--unload-only"].map(str::to_string);
    let outcome = door_word(
        &args,
        &[
            "not-installed",
            "already-unloaded",
            "unloaded",
            "unload-failed",
        ],
        "unload-failed",
    );
    if outcome == "unload-failed" {
        // Config says disabled but the agent may keep ticking; doctor's
        // liveness line stays silent once enabled=false, so this is the only
        // signal that the watcher did not stop.
        eprintln!(
            "pr-watch: WARNING disable set but launchctl unload failed; the agent may still be running. Check whether it is (`launchctl list | grep sh.fno.pr-watcher`), then retry the unload: `launchctl unload ~/Library/LaunchAgents/sh.fno.pr-watcher.plist` (or re-run this config set, which re-fires the unload). `fno do pr watch uninstall` removes the watcher entirely - nothing measured is lost (the watermark store survives; `fno do pr watch install` restores it) - but run it only when removal is the intent, not to silence this warning."
        );
    } else {
        println!("pr-watch: agent {outcome} (disabled).");
    }
}

/// A write that leaves the standing dispatch grant armed implies a live
/// watcher. One-way on purpose: revoking the grant never unloads, since
/// pr_watch.enabled owns that switch. The autonomy panic switch outranks it.
fn couple_grant_observer() {
    let settings = match model::load_validated(&crate::config_cli::repo_root()) {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("pr-watch grant coupling unavailable: {e}");
            return;
        }
    };
    let grant = settings["auto_merge"]["grant"].as_str().unwrap_or("none");
    let armed = settings["auto_merge"]["enabled"] == Value::Bool(true) && grant == "dispatch";
    if !armed || settings["autonomy"]["enabled"] != Value::Bool(true) {
        return;
    }
    match ensure_watcher(Some(&settings)).as_str() {
        "activated" => println!("pr-watch: activated to serve the standing dispatch grant."),
        "already-running" => {}
        outcome => eprintln!(
            "pr-watch: WARNING dispatch grant armed but activation failed ({outcome}); config stays. Run `fno do pr watch install` or check `fno doctor`."
        ),
    }
}
