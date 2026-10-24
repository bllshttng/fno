//! `fno config accounts pick`: the account to launch on, the first with
//! headroom that footnote can actually pin to a worker. Ported from
//! `pick_account`, the `pick` command and `pick_env_lines` in
//! `fno.adapters.providers.cli`; the parity cases live in
//! `tests/accounts_pick_parity.rs`.
//!
//! The launchability filter keeps the answer honest: a managed account that
//! rides the shared slot has no overlay of its own, so offering it would
//! promise a spawn that cannot happen. That is why exit 4 exists, a setup
//! instruction kept apart from the quota answer of exit 3.

use crate::accounts::env::{self, Sources};
use crate::accounts::records::{self, Record};
use crate::accounts::state;
use crate::fallback_chain::Verdict;
use serde_json::{json, Value};
use std::path::Path;

/// Every launchable candidate is exhausted.
pub const EXIT_NO_HEADROOM: i32 = 3;
/// No candidate can be pinned to a worker at all.
pub const EXIT_NO_CANDIDATE: i32 = 4;
/// `--if-armed` and `accounts.quota.pick_on_launch` is off.
pub const EXIT_NOT_ARMED: i32 = 5;

/// A refusal recorded by the login gate stays in force this long.
const LOGIN_REFUSAL_TTL_S: f64 = 900.0;

const PICK_HELP: &str = "Usage: fno config accounts pick [OPTIONS]\n\n  Print the account to launch on: the first with headroom footnote can use.\n\n  stdout is the bare account id; stderr always carries the reason and every\n  candidate's headroom. Exit 0 chose one, 3 means every launchable candidate\n  is exhausted, 4 means there is no launchable candidate at all (a setup\n  problem, not a quota one), and 5 (only with --if-armed) means launch\n  picking is switched off.\n\n  --combo NAME    Combo to pick from (default: the active combo).\n  --exclude ID    Account id to skip (repeatable).\n  --json, -J      Emit the full verdict as one JSON object.\n  --print-env     Emit the picked account's env overlay: the auth vars to\n                  clear as KEY= (empty), then its own KEY=VALUE lines.\n  --if-armed      Decline with exit 5 unless accounts.quota.pick_on_launch\n                  is true.";

/// The pick, the reason, and every considered id with its headroom or the
/// reason it was not launchable, so the receipt reads either way.
#[derive(Debug, Clone, PartialEq)]
pub struct PickVerdict {
    pub account: Option<String>,
    pub reason: String,
    pub candidates: Vec<(String, String)>,
    pub exit_code: i32,
}

/// What one pick asks of the world outside the config.
pub struct World<'a> {
    pub cwd: &'a Path,
    pub now: f64,
    /// Fill the usage cache for one record before its headroom is read.
    pub refresh: &'a dyn Fn(&Record),
    /// Open the chosen account's window (best effort).
    pub stamp: &'a dyn Fn(&str),
}

fn word(v: Verdict) -> &'static str {
    match v {
        Verdict::Exhausted => "exhausted",
        Verdict::Low => "low",
        Verdict::Ok => "ok",
        Verdict::Unknown => "unknown",
    }
}

/// `_pick_candidate_ids`: the active combo's members, else config order. An
/// unknown or empty combo degrades to config order: a stale name must not
/// wedge a launch.
fn candidate_ids(cwd: &Path, records: &[Record], combo: Option<&str>) -> Vec<String> {
    let name = combo
        .map(str::to_string)
        .or_else(|| records::load_active_combo(Some(cwd), cwd));
    if let Some(name) = name {
        let members = records::load_combos(Some(cwd), cwd)
            .ok()
            .and_then(|combos| combos.into_iter().find(|c| c.name == name))
            .map(|c| c.providers)
            .filter(|p| !p.is_empty());
        if let Some(members) = members {
            return members;
        }
    }
    records.iter().map(|r| r.id.clone()).collect()
}

/// `login_probe_refusal`: the detail of a fresh login-gate refusal for this
/// account and dir, or None.
fn login_refusal(id: &str, config_dir: Option<&str>, now: f64) -> Option<String> {
    let home = crate::paths::AgentsHome::from_env_opt()?;
    let raw = std::fs::read_to_string(crate::claude_login::login_probes_path(&home)).ok()?;
    let doc: Value = serde_json::from_str(&raw).ok()?;
    let row = doc.get("probes")?.get(id)?.as_object()?;
    let at = row.get("at_epoch").filter(|v| v.is_number())?.as_f64()?;
    let dir_matches = match (config_dir, row.get("config_dir")) {
        (None, _) | (_, None | Some(Value::Null)) => true,
        (Some(want), Some(got)) => got.as_str() == Some(want),
    };
    let refused = row.get("verdict").and_then(Value::as_str) == Some("logged_out")
        && now - at < LOGIN_REFUSAL_TTL_S
        && dir_matches;
    refused.then(|| {
        row.get("detail")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty())
            .unwrap_or("login probe refused")
            .to_string()
    })
}

/// `pick_account`.
pub fn pick(
    world: &World<'_>,
    combo: Option<&str>,
    exclude: &[String],
    if_armed: bool,
) -> Result<PickVerdict, String> {
    let cwd = world.cwd;
    let accounts = records::load(Some(cwd), cwd).map_err(|e| e.to_string())?;
    let quota = crate::backlog::quota_route::load_quota_config(cwd.to_str());
    if if_armed && !quota.pick_on_launch {
        // The caller honors the knob but cannot read config itself, so the
        // check lives here, once.
        return Ok(PickVerdict {
            account: None,
            reason: "launch picking is not armed (providers.quota.pick_on_launch = false)"
                .to_string(),
            candidates: Vec::new(),
            exit_code: EXIT_NOT_ARMED,
        });
    }
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut launchable: Vec<String> = Vec::new();
    for id in candidate_ids(cwd, &accounts.records, combo) {
        if exclude.contains(&id) {
            rows.push((id, "excluded".to_string()));
            continue;
        }
        let overlay = match env::overlay(&id, None, None, cwd, &Sources::system()) {
            Ok(overlay) => overlay,
            Err(e) => {
                let first = e.lines().next().unwrap_or("").to_string();
                rows.push((id, format!("not launchable: {first}")));
                continue;
            }
        };
        if overlay.lane == "managed-active" {
            // It reaches a worker only through the shared slot, so picking it
            // names the default; the exit-4 receipt says how to fix that.
            rows.push((
                id,
                "not a picker candidate: managed, rides the shared ~/.claude slot".to_string(),
            ));
            continue;
        }
        let dir = overlay.env.get("CLAUDE_CONFIG_DIR").map(String::as_str);
        if let Some(refusal) = login_refusal(&id, dir, world.now) {
            rows.push((id, format!("login probe refused: {refusal}")));
            continue;
        }
        launchable.push(id);
    }
    if launchable.is_empty() {
        let named = if rows.is_empty() {
            "(no records)".to_string()
        } else {
            rows.iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut reason = format!(
            "no launchable candidate among {named}: only an account with its own config dir \
             can be pinned to a worker. Register one with `fno config accounts register <id> \
             --config-dir <path>` (a full second login in its own dir)"
        );
        if rows
            .iter()
            .any(|(_, s)| s.starts_with("login probe refused"))
        {
            reason.push_str(" login refused; remedy: CLAUDE_CONFIG_DIR=<config dir> claude /login");
        }
        return Ok(PickVerdict {
            account: None,
            reason,
            candidates: rows,
            exit_code: EXIT_NO_CANDIDATE,
        });
    }
    // Fill the cache once per candidate first, so the pick acts on fresh
    // numbers; a reading inside the TTL is left alone.
    for id in &launchable {
        if let Some(record) = accounts.by_id(id) {
            (world.refresh)(record);
        }
    }
    let path = crate::provider_cap::runtime_state_path(cwd);
    let verdicts: Vec<Verdict> = launchable
        .iter()
        .map(|id| {
            state::headroom(
                &path,
                id,
                world.now,
                quota.probe_ttl_seconds,
                quota.defer_threshold_pct,
            )
            .verdict
        })
        .collect();
    for (id, v) in launchable.iter().zip(&verdicts) {
        rows.push((id.clone(), word(*v).to_string()));
    }
    // Exhaustion failover walks in priority order; unknown is never exhausted.
    let chosen = launchable
        .iter()
        .zip(&verdicts)
        .find(|(_, v)| **v != Verdict::Exhausted)
        .map(|(id, _)| id.clone());
    let Some(chosen) = chosen else {
        return Ok(PickVerdict {
            account: None,
            reason: "every launchable candidate is exhausted".to_string(),
            candidates: rows,
            exit_code: EXIT_NO_HEADROOM,
        });
    };
    // The window this account is about to spend opens here, so the window
    // projection has lead time instead of learning it from the cap.
    (world.stamp)(&chosen);
    Ok(PickVerdict {
        account: Some(chosen),
        reason: "first launchable candidate with headroom".to_string(),
        candidates: rows,
        exit_code: 0,
    })
}

/// The live pick for `cwd`: probes refresh the cache and the chosen
/// account's window is stamped.
pub fn pick_live(
    cwd: &Path,
    combo: Option<&str>,
    exclude: &[String],
    if_armed: bool,
) -> Result<PickVerdict, String> {
    let now = state::wall_now();
    let records = records::load(Some(cwd), cwd)
        .map(|a| a.records)
        .unwrap_or_default();
    let ttl = crate::backlog::quota_route::load_quota_config(cwd.to_str()).probe_ttl_seconds;
    let refresh = |r: &Record| crate::accounts::usage::refresh_one(cwd, &records, r, ttl, now);
    let stamp = |id: &str| {
        let path = crate::provider_cap::runtime_state_path(cwd);
        state::fold_legacy_files(&path, cwd, None);
        let span = state::record_window_seconds(cwd, id);
        let _ = state::stamp_window(
            &path,
            id,
            now,
            state::WINDOW_LABEL,
            span,
            now,
            state::LOCK_TIMEOUT,
        );
    };
    pick(
        &World {
            cwd,
            now,
            refresh: &refresh,
            stamp: &stamp,
        },
        combo,
        exclude,
        if_armed,
    )
}

/// `pick_env_lines`: the account's complete overlay as `(key, value)` pairs.
/// An empty value means "clear this variable": an inherited
/// ANTHROPIC_API_KEY or a routed base URL outranks CLAUDE_CONFIG_DIR, so
/// the auth vars the overlay does not set come first, cleared.
pub fn env_lines(account: &str, cwd: &Path) -> Result<Vec<(String, String)>, String> {
    let overlay = env::overlay(account, None, None, cwd, &Sources::system())?;
    let mut lines: Vec<(String, String)> = crate::pane_spawn::env_wrap::SCRUB_AUTH_VARS
        .iter()
        .filter(|k| !overlay.env.contains_key(**k))
        .map(|k| (k.to_string(), String::new()))
        .collect();
    lines.extend(overlay.env.into_iter());
    Ok(lines)
}

/// The command's stderr and stdout lines for one verdict.
pub fn render(
    verdict: &PickVerdict,
    as_json: bool,
    env: Option<&[(String, String)]>,
) -> (Vec<String>, Vec<String>) {
    let mut err: Vec<String> = verdict
        .candidates
        .iter()
        .map(|(id, state)| format!("  {id}: {state}"))
        .collect();
    err.push(format!("pick: {}", verdict.reason));
    let out = if as_json {
        let doc = json!({
            "account": verdict.account,
            "reason": verdict.reason,
            "candidates": verdict.candidates.iter()
                .map(|(id, state)| json!({"id": id, "state": state}))
                .collect::<Vec<_>>(),
        });
        vec![crate::backlog::maintenance::validity::py_dumps(
            &doc, false, true,
        )]
    } else {
        match (&verdict.account, env) {
            (None, _) => Vec::new(),
            (Some(_), Some(env)) => env.iter().map(|(k, v)| format!("{k}={v}")).collect(),
            (Some(id), None) => vec![id.clone()],
        }
    };
    (err, out)
}

/// `fno-agents spawn-overlay accounts pick [OPTIONS]`.
pub fn run(args: &[String]) -> i32 {
    let mut combo: Option<String> = None;
    let mut exclude: Vec<String> = Vec::new();
    let (mut as_json, mut print_env, mut if_armed) = (false, false, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--combo" | "--exclude" => {
                let Some(value) = inline.or_else(|| it.next().cloned()) else {
                    eprintln!("error: option '{flag}' requires an argument");
                    return 2;
                };
                if flag == "--combo" {
                    combo = Some(value);
                } else {
                    exclude.push(value);
                }
            }
            "--json" | "-J" => as_json = true,
            "--print-env" => print_env = true,
            "--if-armed" => if_armed = true,
            "-h" | "--help" => {
                println!("{PICK_HELP}");
                return 0;
            }
            other => {
                eprintln!("error: no such option: {other}");
                return 2;
            }
        }
    }
    let cwd = crate::accounts::show::pwd();
    let verdict = match pick_live(&cwd, combo.as_deref(), &exclude, if_armed) {
        Ok(verdict) => verdict,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let env = match (&verdict.account, print_env && !as_json) {
        (Some(id), true) => match env_lines(id, &cwd) {
            Ok(lines) => Some(lines),
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        },
        _ => None,
    };
    let (err, out) = render(&verdict, as_json, env.as_deref());
    for line in err {
        eprintln!("{line}");
    }
    for line in out {
        println!("{line}");
    }
    verdict.exit_code
}
