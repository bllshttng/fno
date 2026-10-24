//! `fno config accounts required-bot-check [--json]`: the pre-promise warning
//! that a required review bot's provider is out of quota. Ported from the
//! `required-bot-check` command in `fno.adapters.providers.cli`; the parity
//! cases live in `tests/accounts_required_bot_parity.rs`.
//!
//! Read-only, advisory, exit 0 always. It reads the cached runtime state and
//! never probes. A bot warns only when EVERY account record of its CLI kind
//! is exhausted: one healthy account keeps the review gate unwedged. Any
//! unreadable config reads as no warning, because a promise-time read must
//! never block a promise.

use crate::accounts::records::Record;
use crate::accounts::state;
use crate::accounts::usage::resets_in;
use crate::fallback_chain::Verdict;
use serde_json::{json, Map, Value};
use std::path::Path;

const HELP: &str = "Usage: fno config accounts required-bot-check [--json]

  Warn (pre-promise) if a required review bot's provider is out of quota.

  --json, -J    Emit a JSON list of exhausted required-bot providers.";

/// A GitHub login substring and the CLI kind that backs it, in match order.
const BOT_CLI: [(&str, &str); 3] = [
    ("codex", "codex"),
    ("gemini", "gemini"),
    ("claude", "claude"),
];

/// One bot whose every account of its kind is exhausted. `provider` is the
/// first such record; `retry_at` is its reset.
#[derive(Debug, PartialEq)]
pub struct Warning {
    pub bot: String,
    pub provider: String,
    pub retry_at: Option<f64>,
}

fn bot_cli(bot: &str) -> Option<&'static str> {
    let low = bot.to_lowercase();
    BOT_CLI
        .iter()
        .find(|(needle, _)| low.contains(needle))
        .map(|(_, kind)| *kind)
}

/// `config.review.github_apps`, or its legacy alias `required_bots` when
/// github_apps is unset, deduped in order. A value that is not an array of
/// strings names no bot. The bool is true when both keys are set.
pub fn configured_bots(
    github_apps: Option<Value>,
    required_bots: Option<Value>,
) -> (Vec<String>, bool) {
    let both = github_apps.is_some() && required_bots.is_some();
    let mut bots: Vec<String> = Vec::new();
    for bot in github_apps
        .or(required_bots)
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !bots.iter().any(|b| b == bot) {
            bots.push(bot.to_string());
        }
    }
    (bots, both)
}

/// The warnings, one per bot whose every record of its CLI kind reads
/// exhausted. A bot with no CLI kind, or no record of its kind, is skipped.
pub fn warnings(bots: &[String], records: &[Record], state_path: &Path, now: f64) -> Vec<Warning> {
    let mut out = Vec::new();
    for bot in bots {
        let Some(kind) = bot_cli(bot) else {
            continue;
        };
        let ids: Vec<&str> = records
            .iter()
            .filter(|r| r.harness == kind)
            .map(|r| r.id.as_str())
            .collect();
        let mut first: Option<(&str, Option<f64>)> = None;
        let all_exhausted = !ids.is_empty()
            && ids.iter().all(|id| {
                let v = state::headroom(
                    state_path,
                    id,
                    now,
                    state::DEFAULT_USAGE_TTL_SECONDS,
                    state::DEFAULT_THRESHOLD_PCT,
                );
                if v.verdict != Verdict::Exhausted {
                    return false;
                }
                first.get_or_insert((*id, v.resets_at));
                true
            });
        if let (true, Some((provider, retry_at))) = (all_exhausted, first) {
            out.push(Warning {
                bot: bot.clone(),
                provider: provider.to_string(),
                retry_at,
            });
        }
    }
    out
}

/// The `quota_required_bot_exhausted` event data for one warning.
pub fn event_data(w: &Warning) -> Map<String, Value> {
    let mut data = Map::new();
    data.insert("bot".into(), json!(w.bot));
    data.insert("provider".into(), json!(w.provider));
    data.insert("headroom".into(), json!("exhausted"));
    if let Some(at) = w.retry_at {
        data.insert("retry_at".into(), json!(at));
    }
    data
}

/// The `--json` list as `json.dumps` printed it, or one line per warning;
/// `None` when the text form has nothing to say.
pub fn render(warnings: &[Warning], as_json: bool, now: f64) -> Option<String> {
    if as_json {
        let rows: Vec<Value> = warnings
            .iter()
            .map(|w| json!({"bot": w.bot, "provider": w.provider, "retry_at": w.retry_at}))
            .collect();
        return Some(crate::backlog::maintenance::validity::py_dumps(
            &Value::Array(rows),
            false,
            true,
        ));
    }
    if warnings.is_empty() {
        return None;
    }
    let lines: Vec<String> = warnings
        .iter()
        .map(|w| {
            // A zero reset reads unknown, as the Python truthiness test did.
            let reset = match w.retry_at.filter(|at| *at != 0.0) {
                Some(at) => resets_in(Some(at), now),
                None => "unknown".to_string(),
            };
            format!(
                "required-bot {}: provider {} EXHAUSTED, review gate will wedge until reset ({reset})",
                w.bot, w.provider
            )
        })
        .collect();
    Some(lines.join("\n"))
}

/// `[--json]`: `None` for `--help`, else the JSON flag. `Err` is the usage
/// line; it exits 2.
pub fn parse(args: &[String]) -> Result<Option<bool>, String> {
    let mut as_json = false;
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--json" | "-J" => as_json = true,
            other => return Err(format!("error: no such option: {other}")),
        }
    }
    Ok(Some(as_json))
}

/// `fno-agents spawn-overlay accounts required-bot-check [--json]`.
pub fn run(args: &[String]) -> i32 {
    let as_json = match parse(args) {
        Ok(Some(as_json)) => as_json,
        Ok(None) => {
            println!("{HELP}");
            return 0;
        }
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let cwd = crate::accounts::show::pwd();
    let now = state::wall_now();
    let deep = |key: &str| crate::agents_config::config_value_deep(&cwd, &["review", key]);
    let (bots, both) = configured_bots(
        deep("github_apps").and_then(|v| serde_json::to_value(v).ok()),
        deep("required_bots").and_then(|v| serde_json::to_value(v).ok()),
    );
    if both {
        eprintln!(
            "settings.yaml: both config.review.github_apps and the legacy config.review.required_bots are set; using github_apps"
        );
    }
    let found = if bots.is_empty() {
        Vec::new()
    } else {
        match crate::accounts::records::load(Some(&cwd), &cwd) {
            Ok(accounts) => warnings(
                &bots,
                &accounts.records,
                &crate::provider_cap::runtime_state_path(&cwd),
                now,
            ),
            Err(_) => Vec::new(),
        }
    };
    if !found.is_empty() {
        let emitter = crate::events::EventEmitter::new(
            crate::claims::claim_events_path(None, &cwd),
            "daemon",
        );
        for w in &found {
            // Best effort: a warning that cannot be logged was still printed.
            let _ = emitter.emit_fields("quota_required_bot_exhausted", event_data(w));
        }
    }
    if let Some(text) = render(&found, as_json, now) {
        println!("{text}");
    }
    0
}
