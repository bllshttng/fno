//! `fno config accounts benchmarks <refresh|show|tiers>`: the OpenRouter
//! coding benchmark snapshot, and the tier-table staleness the review doctor
//! prints. The snapshot is optional enrichment: [`route_gather`] reads it for
//! a percentile, and its absence never makes routing inert.
//!
//! [`route_gather`]: crate::route_gather

use crate::route_gather;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const OPENROUTER_BENCHMARKS_URL: &str = "https://openrouter.ai/api/v1/benchmarks";
const TASK_TYPE: &str = "coding";
const STALE_AFTER_SECONDS: f64 = 14.0 * 24.0 * 3600.0;
const API_KEY_ENV: &str = "OPENROUTER_API_KEY";
const REFRESH_HINT: &str = "run `fno config accounts benchmarks refresh`";

const USAGE: &str = "Usage: fno config accounts benchmarks [OPTIONS] COMMAND [ARGS]...

  Cache + view the OpenRouter coding benchmark snapshot (routing source of truth).

Commands:
  refresh  Fetch the OpenRouter coding benchmarks and cache them atomically.
  show     Render the cached benchmark snapshot; warn once if it is stale (>14 days).
  tiers    The tier-table ids no reachability row serves, and the bands a harness cannot serve, as JSON.";

/// Tier-table ids no reachability row serves, sorted and listed once each.
pub(crate) fn unreachable_tier_ids(
    tiers: &[(String, Vec<String>)],
    reach: &Map<String, Value>,
) -> Vec<String> {
    let mut flagged: Vec<String> = tiers
        .iter()
        .flat_map(|(_, names)| names)
        .filter(|name| !reach.contains_key(*name))
        .cloned()
        .collect();
    flagged.sort();
    flagged.dedup();
    flagged
}

/// The bands whose static tier names no row a harness serves, per harness.
/// It reads the static tables alone, never the declared inventory.
pub(crate) fn empty_bands(
    harnesses: &[&str],
    tiers: &[(String, Vec<String>)],
    reach: &Map<String, Value>,
) -> BTreeMap<String, Vec<String>> {
    let serves = |name: &str, harness: &str| {
        reach
            .get(name)
            .and_then(Value::as_array)
            .and_then(|pair| pair.first())
            .and_then(Value::as_str)
            == Some(harness)
    };
    let mut out = BTreeMap::new();
    for harness in harnesses {
        let mut empty: Vec<String> = tiers
            .iter()
            .filter(|(_, names)| !names.iter().any(|n| serves(n, harness)))
            .map(|(band, _)| band.clone())
            .collect();
        if !empty.is_empty() {
            empty.sort();
            out.insert(harness.to_string(), empty);
        }
    }
    out
}

fn tiers_answer() -> Value {
    let (tiers, reach) = route_gather::load_model_tiers();
    json!({
        "unreachable_tier_ids": unreachable_tier_ids(&tiers, &reach),
        "empty_bands": empty_bands(&["claude", "codex"], &tiers, &reach),
    })
}

/// `[{name, coding_percentile}]` from the OpenRouter answer: a bare list or
/// one under `models`; a row's name is `name`, else `model`, else `id`.
fn parse_models(payload: &Value) -> Result<Vec<Value>, String> {
    let rows = payload
        .as_object()
        .map_or(Some(payload), |o| o.get("models"))
        .and_then(Value::as_array)
        .ok_or("OpenRouter benchmarks response has no model list")?;
    let out: Vec<Value> = rows
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|r| {
            let name = ["name", "model", "id"]
                .iter()
                .filter_map(|k| r.get(*k))
                .find(|v| !is_falsy(v))?;
            let pct = r
                .get("coding_percentile")
                .or_else(|| r.get("percentile"))
                .cloned()
                .unwrap_or(Value::Null);
            Some(json!({"name": py_str(name), "coding_percentile": pct}))
        })
        .collect();
    if out.is_empty() {
        return Err("OpenRouter benchmarks response had zero usable models".into());
    }
    Ok(out)
}

fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::String(s) => s.is_empty(),
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// Python's `str()` of a JSON value, as the show table prints it.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python's `datetime.isoformat()` in UTC: microseconds only when nonzero.
fn iso(t: chrono::DateTime<chrono::Utc>) -> String {
    use chrono::Timelike;
    let format = if t.nanosecond() / 1000 == 0 {
        chrono::SecondsFormat::Secs
    } else {
        chrono::SecondsFormat::Micros
    };
    t.to_rfc3339_opts(format, false)
}

fn snapshot_path(cwd: &Path) -> Option<PathBuf> {
    Some(
        crate::agents_config::state_dir(cwd)?
            .join("cache")
            .join("benchmarks.json"),
    )
}

/// Fetch the snapshot through `http` (a curl `-K` config in, status and body
/// out) and write it whole or not at all. A 429 asks for a manual re-run:
/// at a fortnightly cadence the re-run is the backoff.
fn refresh(
    key: &str,
    dest: &Path,
    now: chrono::DateTime<chrono::Utc>,
    http: &dyn Fn(&str) -> Result<(u16, String), ()>,
) -> Result<usize, String> {
    if key.contains(['\n', '\r']) {
        return Err("OpenRouter API key holds a line break".into());
    }
    let esc = crate::claude_vault::curl_escape;
    let url = format!("{OPENROUTER_BENCHMARKS_URL}?task_type={TASK_TYPE}");
    let config = format!(
        "url = \"{}\"\nheader = \"Authorization: Bearer {}\"\n",
        esc(&url),
        esc(key)
    );
    let (status, body) = http(&config)
        .map_err(|()| "OpenRouter benchmarks fetch failed: the request did not complete")?;
    match status {
        429 => return Err("OpenRouter rate limit (HTTP 429); retry the refresh later".into()),
        200..=299 => {}
        code => return Err(format!("OpenRouter benchmarks fetch failed: HTTP {code}")),
    }
    let payload: Value = serde_json::from_str(&body)
        .map_err(|e| format!("OpenRouter benchmarks response was not JSON: {e}"))?;
    let models = parse_models(&payload)?;
    let count = models.len();
    let snapshot = json!({
        "fetched_at": iso(now),
        "source": OPENROUTER_BENCHMARKS_URL,
        "models": models,
    });
    let text = serde_json::to_string_pretty(&snapshot).map_err(|e| e.to_string())? + "\n";
    crate::claude_vault::atomic_write(dest, &text).map_err(|e| e.to_string())?;
    Ok(count)
}

/// Seconds since `fetched_at`, floored at zero; `None` when it does not
/// parse. A naive stamp reads as local time, as Python's `fromisoformat`.
fn staleness_seconds(snapshot: &Value, now: chrono::DateTime<chrono::Utc>) -> Option<f64> {
    let raw = snapshot.get("fetched_at")?.as_str()?;
    let fetched = chrono::DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f").map(|n| {
                n.and_local_timezone(chrono::Local)
                    .earliest()
                    .map_or(now, |t| t.with_timezone(&chrono::Utc))
            })
        })
        .ok()?;
    Some(((now - fetched).num_milliseconds() as f64 / 1000.0).max(0.0))
}

/// `harness:model` for a declared or built-in inventory row, or `None`.
fn route_of(name: &str, rows: &[Value]) -> Option<String> {
    let row = rows.iter().find(|r| r["name"].as_str() == Some(name))?;
    let harness = row["harness"].as_str().filter(|s| !s.is_empty())?;
    let model = row["model"].as_str().filter(|s| !s.is_empty())?;
    Some(format!("{harness}:{model}"))
}

fn show(cwd: &Path, now: chrono::DateTime<chrono::Utc>) -> i32 {
    let Some(snapshot) = route_gather::load_snapshot(cwd) else {
        eprintln!("no benchmark snapshot; {REFRESH_HINT}");
        return 1;
    };
    if let Some(age) = staleness_seconds(&snapshot, now).filter(|a| *a > STALE_AFTER_SECONDS) {
        eprintln!(
            "WARNING: benchmark snapshot is {} days old (>14); {REFRESH_HINT}",
            (age / 86400.0).floor() as i64
        );
    }
    let field = |k: &str| snapshot.get(k).map_or("None".to_string(), py_str);
    println!(
        "source: {}  fetched_at: {}",
        field("source"),
        field("fetched_at")
    );
    let inventory = route_gather::inventory(cwd);
    let rows = inventory["rows"].as_array().cloned().unwrap_or_default();
    for row in snapshot["models"].as_array().into_iter().flatten() {
        let name = row.get("name").map_or("?".to_string(), py_str);
        let pct = row
            .get("coding_percentile")
            .map_or("None".to_string(), py_str);
        let route = route_of(&name, &rows).unwrap_or_else(|| "unmapped (not routable)".into());
        println!("  {name:<24} coding={pct:<6} -> {route}");
    }
    0
}

pub fn run(args: &[String]) -> i32 {
    let cwd = crate::accounts::show::pwd();
    match args.first().map(String::as_str) {
        Some("refresh") => {
            let key = std::env::var(API_KEY_ENV).unwrap_or_default();
            let key = key.trim();
            if key.is_empty() {
                eprintln!(
                    "benchmarks refresh: no OpenRouter API key: set {API_KEY_ENV} to refresh benchmarks"
                );
                return 1;
            }
            let Some(dest) = snapshot_path(&cwd) else {
                eprintln!("benchmarks refresh: no state dir to write the snapshot into");
                return 1;
            };
            let http = |config: &str| crate::claude_vault::curl_raw(config).map_err(|_| ());
            match refresh(key, &dest, chrono::Utc::now(), &http) {
                Ok(count) => {
                    println!("refreshed {count} models -> {}", dest.display());
                    0
                }
                Err(msg) => {
                    eprintln!("benchmarks refresh: {msg}");
                    1
                }
            }
        }
        Some("show") => show(&cwd, chrono::Utc::now()),
        Some("tiers") => {
            println!("{}", tiers_answer());
            0
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiers(rows: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        rows.iter()
            .map(|(b, names)| (b.to_string(), names.iter().map(|n| n.to_string()).collect()))
            .collect()
    }

    #[test]
    fn a_planted_dead_id_is_flagged_by_name_and_the_shipped_table_is_green() {
        let (shipped, reach) = route_gather::load_model_tiers();
        let planted = tiers(&[("high", &["claude-opus", "gpt-dead-9.9"])]);
        assert_eq!(unreachable_tier_ids(&planted, &reach), ["gpt-dead-9.9"]);
        assert!(unreachable_tier_ids(&shipped, &reach).is_empty());
        assert!(empty_bands(&["claude", "codex"], &shipped, &reach).is_empty());
    }

    #[test]
    fn a_band_a_harness_cannot_serve_is_reported() {
        let (_, reach) = route_gather::load_model_tiers();
        let planted = tiers(&[("max", &["codex-sol"]), ("low", &["claude-haiku"])]);
        let holes = empty_bands(&["claude", "codex"], &planted, &reach);
        assert_eq!(holes["claude"], ["max"]);
        assert_eq!(holes["codex"], ["low"]);
    }

    #[test]
    fn refresh_writes_the_snapshot_whole_or_fails_loud_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("cache/benchmarks.json");
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-24T04:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let answer = |status: u16, body: &'static str| {
            move |config: &str| -> Result<(u16, String), ()> {
                assert!(config.contains("Authorization: Bearer k"));
                assert!(config.contains("?task_type=coding"));
                Ok((status, body.to_string()))
            }
        };
        let fails = [
            (answer(429, "{}"), "rate limit (HTTP 429)"),
            (answer(500, "{}"), "fetch failed: HTTP 500"),
            (answer(200, "not json"), "was not JSON"),
            (
                answer(200, r#"{"models": [{"id": ""}]}"#),
                "zero usable models",
            ),
        ];
        for (http, want) in fails {
            let err = refresh("k", &dest, now, &http).unwrap_err();
            assert!(err.contains(want), "{err}");
            assert!(!dest.exists(), "a failed refresh left {}", dest.display());
        }
        let dead = |_: &str| -> Result<(u16, String), ()> { Err(()) };
        assert!(refresh("k", &dest, now, &dead)
            .unwrap_err()
            .contains("did not complete"));

        let ok = answer(
            200,
            r#"[{"model": "claude-opus", "percentile": 96}, "junk"]"#,
        );
        assert_eq!(refresh("k", &dest, now, &ok).unwrap(), 1);
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(written["fetched_at"], "2026-10-24T04:00:00+00:00");
        assert_eq!(
            written["models"],
            json!([{"name": "claude-opus", "coding_percentile": 96}])
        );
        let leftovers = std::fs::read_dir(dest.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1, "the atomic write left a temp file");
    }
}
