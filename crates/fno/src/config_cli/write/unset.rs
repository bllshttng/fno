//! `fno config unset <key>` (alias `rm`): remove a key so it reverts to the
//! model default. Non-destructive, so no confirmation: an unknown key exits
//! 1 and changes nothing, an absent key is a clean no-op, and any block the
//! removal leaves empty is pruned.

use super::{
    default_at, is_merge_gating, locked_update, parse_argv, plan_unset_release, py_model_str,
    release_optout, resolve_parent_block, storage_parts, target_path, usage_error, Parsed, Refusal,
    Res,
};
use crate::config_cli::model::{self, py_repr, py_str};
use serde_json::Value;
use std::ffi::OsString;
use std::path::PathBuf;

const HELP_UNSET: &str = include_str!("../../../tests/golden/config_set/unset_help.out");
const HELP_RM: &str = include_str!("../../../tests/golden/config_set/rm_help.out");

/// What one unset found and left.
pub struct UnsetResult {
    pub was: Value,
    pub present: bool,
    /// The default the key reverts to, already rendered.
    pub default: String,
    pub path: PathBuf,
}

/// `unset_config_value`: remove `key` from the scope's file. A key stored
/// under the pre-rename `[providers]` block is removed in both spellings,
/// so the legacy copy cannot keep the value alive.
pub fn unset_value(key: &str, scope: &str) -> Res<UnsetResult> {
    let bare = key.strip_prefix("config.").unwrap_or(key);
    let retired = model::RETIRED_DISPATCH_KEYS.contains(&bare);
    if !retired && resolve_parent_block(key).is_none() {
        return Err(Refusal::new(
            format!(
                "unknown config key {}",
                py_repr(&Value::String(key.to_string()))
            ),
            1,
        ));
    }
    let default = if retired {
        "None".to_string()
    } else {
        match default_at(key) {
            (value, true) => py_model_str(&value),
            (value, false) => py_str(&value),
        }
    };
    let store: Vec<String> = if retired {
        key.split('.').map(str::to_string).collect()
    } else {
        storage_parts(key)
    };
    let legacy: Option<Vec<String>> = match store.split_first() {
        Some((head, rest)) if head == "accounts" => {
            let mut parts = vec!["providers".to_string()];
            parts.extend(rest.iter().cloned());
            Some(parts)
        }
        _ => None,
    };
    let target = target_path(scope)?;
    let real = if crate::config_cli::is_symlink(&target) {
        crate::config_cli::resolve_path(&target)
    } else {
        target.clone()
    };
    let holder = if is_merge_gating(key) {
        plan_unset_release(key)?
    } else {
        None
    };
    // No file, nothing to remove: never create an empty one.
    if !real.exists() {
        if let Some(holder) = &holder {
            release_optout(key, holder)?;
        }
        return Ok(UnsetResult {
            was: Value::Null,
            present: false,
            default,
            path: real,
        });
    }
    let mut was = Value::Null;
    let mut present = false;
    let written = locked_update(&target, scope, None, |mut existing| {
        if let Some(value) = super::deep_unset(&mut existing, &store) {
            was = value;
            present = true;
        }
        if let Some(legacy) = &legacy {
            if let Some(value) = super::deep_unset(&mut existing, legacy) {
                present = true;
                if was.is_null() {
                    was = value;
                }
            }
        }
        Ok(existing)
    })?;
    if let Some(holder) = &holder {
        release_optout(key, holder)?;
    }
    Ok(UnsetResult {
        was,
        present,
        default,
        path: written.path,
    })
}

/// `unset --restore-optout <claim-json>`: the claim reaper's restore door.
/// fno-agents cannot link this crate, so it shells here with the reaped
/// record (`key`, `metadata`). A failure is one stderr line the reaper folds.
fn run_restore(args: &[OsString]) -> i32 {
    let record = match args {
        [one] => serde_json::from_str::<Value>(&one.to_string_lossy()).ok(),
        _ => None,
    };
    let Some(record) = record else {
        eprintln!("opt-out restore failed: expected one claim record JSON");
        return 2;
    };
    let key = record.get("key").and_then(Value::as_str).unwrap_or("");
    let empty = serde_json::Map::new();
    let metadata = record
        .get("metadata")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    match super::restore_reaped_optout(key, metadata) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("opt-out restore failed: {e}");
            1
        }
    }
}

pub fn run(command: &str, args: &[OsString]) -> i32 {
    if command == "unset" && args.first().is_some_and(|a| a == "--restore-optout") {
        return run_restore(&args[1..]);
    }
    let help = if command == "rm" { HELP_RM } else { HELP_UNSET };
    let (positionals, local) = match parse_argv(command, "KEY", help, args) {
        Parsed::Exit(code) => return code,
        Parsed::Args { positionals, local } => (positionals, local),
    };
    if positionals.len() > 1 {
        let extra = &positionals[1..];
        let noun = if extra.len() == 1 {
            "argument"
        } else {
            "arguments"
        };
        return usage_error(
            command,
            "KEY",
            &format!("Got unexpected extra {noun} ({})", extra.join(" ")),
        );
    }
    let key = &positionals[0];
    let scope = if local { "project" } else { "global" };
    let result = match unset_value(key, scope) {
        Ok(result) => result,
        Err(refusal) => {
            eprintln!("error: {}", refusal.message);
            return refusal.code;
        }
    };
    if !result.present {
        println!("not set: {key}");
        return 0;
    }
    println!(
        "unset {key} (was {}); now defaults to {} ({scope}: {})",
        py_str(&result.was),
        result.default,
        result.path.display()
    );
    0
}
