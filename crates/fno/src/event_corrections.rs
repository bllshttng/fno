//! `fno doctor event corrections`: correction rates per model or harness, the
//! three correctors kept apart, and the precision read of a labels file.
//!
//! No total crosses the three sources: machine rows outnumber user rows by
//! about 17 to 1, so a sum would bury the signal this report exists to show.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const SOURCES: [&str; 3] = ["user", "agent", "machine"];
/// The floor a source must clear before a reader may trust it.
const FLOOR_POINT: f64 = 0.80;
const FLOOR_LOWER: f64 = 0.70;
/// The five labelled sources the precision test draws.
const PRECISION_SOURCES: [&str; 5] = [
    "user/correct",
    "user/reject",
    "user/interrupt",
    "agent/correct",
    "machine/refused",
];

const HELP: &str = "Usage: fno doctor event corrections [OPTIONS]

  Correction rates per model, harness or day: user, agent and machine apart.

Options:
  --by <model|harness|day>    Group the rows. [default: model]
  --since <7d|ISO-8601>       Lower timestamp bound. [default: 7d]
  --source <user|agent|machine>  Print one source.
  --include-interrupt         Count interrupts, which reports exclude by default.
  --precision <file>          Print precision and a Wilson interval from a labels file.
  --events <path>             Read this journal.
  -J, --json                  Emit one structured result.
  --help                      Show this message and exit.
";

struct Args {
    by: String,
    since: String,
    source: Option<String>,
    include_interrupt: bool,
    precision: Option<PathBuf>,
    journal: Option<PathBuf>,
    json: bool,
}

fn parse(args: &[OsString]) -> Result<Option<Args>, String> {
    let mut out = Args {
        by: "model".into(),
        since: "7d".into(),
        source: None,
        include_interrupt: false,
        precision: None,
        journal: None,
        json: false,
    };
    let mut it = args.iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(tok) = it.next() {
        let (flag, inline) = match tok.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (tok, None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or(format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--help" => return Ok(None),
            "--by" => out.by = value("--by")?,
            "--since" => out.since = value("--since")?,
            "--source" => out.source = Some(value("--source")?),
            "--precision" => out.precision = Some(PathBuf::from(value("--precision")?)),
            "--events" => out.journal = Some(PathBuf::from(value("--events")?)),
            "--include-interrupt" => out.include_interrupt = true,
            "--json" | "-J" => out.json = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if !matches!(out.by.as_str(), "model" | "harness" | "day") {
        return Err(format!(
            "--by must be model, harness or day, not {}",
            out.by
        ));
    }
    if let Some(source) = out.source.as_deref().filter(|s| !SOURCES.contains(s)) {
        return Err(format!(
            "--source must be user, agent or machine, not {source}"
        ));
    }
    Ok(Some(out))
}

fn since_ms(raw: &str, now_ms: i64) -> Result<i64, String> {
    if let Some(days) = raw.strip_suffix('d').and_then(|d| d.parse::<i64>().ok()) {
        return Ok(now_ms - days * 86_400_000);
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|d| d.timestamp_millis())
        .map_err(|_| format!("--since must be like 7d or an ISO-8601 stamp, not {raw}"))
}

/// The group a row counts under. An unmeasured reading keeps its own bucket:
/// it never folds into a named model.
fn group_of(data: &Value, by: &str) -> String {
    if by == "harness" {
        return data
            .get("harness")
            .and_then(Value::as_str)
            .unwrap_or("unmeasured")
            .to_string();
    }
    match data.pointer("/model/value").and_then(Value::as_str) {
        Some(model) => {
            let basis = data
                .pointer("/model/basis")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!("{model} ({basis})")
        }
        None => "unmeasured".to_string(),
    }
}

#[derive(Default, Clone)]
struct Tally {
    correct: u64,
    reject: u64,
    interrupt: u64,
    refused: u64,
}

impl Tally {
    fn total(&self, include_interrupt: bool) -> u64 {
        self.correct
            + self.reject
            + self.refused
            + if include_interrupt { self.interrupt } else { 0 }
    }
}

/// Typed turns per group since a day, from the fold's cache: the denominator
/// of the user rate. Keys read `day|harness|model`.
fn typed_turns(cache: &Path, by: &str, since_day: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(cache) else {
        return out;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return out;
    };
    let rows = parsed.get("typed_turns").and_then(Value::as_object);
    for (key, n) in rows.into_iter().flatten() {
        let mut parts = key.splitn(3, '|');
        let (Some(day), Some(harness), Some(model)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if day < since_day {
            continue;
        }
        let group = match by {
            "harness" => harness,
            "day" => day,
            _ => model,
        };
        *out.entry(group.to_string()).or_default() += n.as_u64().unwrap_or(0);
    }
    out
}

/// The tally key names the bare model; the report names it with its basis.
fn bare_group(group: &str) -> &str {
    group.split(" (").next().unwrap_or(group)
}

fn report(args: &Args, journal: &Path, cache: &Path) -> Result<Value, String> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let since = since_ms(&args.since, now_ms)?;
    let query = crate::event_store::EventQuery {
        types: vec!["agent_corrected".into()],
        since_ms: Some(since),
        ..Default::default()
    };
    let rows = crate::event_store::query_events(journal, &query)?;
    let mut blocks: BTreeMap<String, BTreeMap<String, Tally>> = BTreeMap::new();
    for row in &rows {
        let Ok(envelope) = serde_json::from_str::<Value>(&row.line) else {
            continue;
        };
        let data = &envelope["data"];
        let source = data
            .get("corrected_by")
            .and_then(Value::as_str)
            .unwrap_or("");
        let entry = blocks
            .entry(source.to_string())
            .or_default()
            .entry(if args.by == "day" {
                envelope["ts"]
                    .as_str()
                    .and_then(|t| t.get(..10))
                    .unwrap_or("unmeasured")
                    .to_string()
            } else {
                group_of(data, &args.by)
            })
            .or_default();
        match data.get("kind").and_then(Value::as_str) {
            Some("correct") => entry.correct += 1,
            Some("reject") => entry.reject += 1,
            Some("interrupt") => entry.interrupt += 1,
            Some("refused") => entry.refused += 1,
            _ => {}
        }
    }
    let since_day = chrono::DateTime::from_timestamp_millis(since)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let turns = typed_turns(cache, &args.by, &since_day);
    // A day with typed turns and no correction is a rate of zero, not a gap.
    let by_day = args.by == "day";
    if by_day {
        let user = blocks.entry("user".to_string()).or_default();
        for day in turns.keys() {
            user.entry(day.clone()).or_default();
        }
    }
    let mut out = serde_json::Map::new();
    for source in SOURCES {
        if args.source.as_deref().is_some_and(|s| s != source) {
            continue;
        }
        let mut groups = Vec::new();
        if let Some(block) = blocks.get(source) {
            for (group, t) in block {
                let count = t.total(args.include_interrupt);
                if count == 0 && !(by_day && source == "user") {
                    continue;
                }
                let mut row = json!({
                    "group": group, "count": count, "correct": t.correct,
                    "reject": t.reject, "refused": t.refused,
                });
                if args.include_interrupt {
                    row["interrupt"] = json!(t.interrupt);
                }
                // A rate over typed turns is meaningful for the user only.
                if source == "user" {
                    let typed = turns.get(bare_group(group)).copied().unwrap_or(0);
                    row["typed_turns"] = json!(typed);
                    row["rate"] = if typed > 0 {
                        json!(t.correct.saturating_add(t.reject) as f64 / typed as f64)
                    } else {
                        Value::Null
                    };
                }
                groups.push(row);
            }
        }
        out.insert(source.to_string(), Value::Array(groups));
    }
    Ok(json!({"by": args.by, "since": args.since, "blocks": out,
        "note": "held is weak evidence: a merge shows nothing broke, never that the corrector was right. Compare rates only within one rule_version."}))
}

fn print_report(value: &Value) {
    println!("{}", value["note"].as_str().unwrap_or(""));
    for source in SOURCES {
        let Some(groups) = value["blocks"].get(source).and_then(Value::as_array) else {
            continue;
        };
        println!("\n{source}");
        if groups.is_empty() {
            println!("  no rows");
        }
        for g in groups {
            let rate = g
                .get("rate")
                .and_then(Value::as_f64)
                .map(|r| format!(" rate {r:.3} over {} typed turns", g["typed_turns"]))
                .unwrap_or_default();
            println!(
                "  {:<40} {:>6}{rate}",
                g["group"].as_str().unwrap_or(""),
                g["count"]
            );
        }
    }
}

/// The Wilson score interval at 95%.
fn wilson(hits: u64, n: u64) -> (f64, f64) {
    if n == 0 {
        return (0.0, 0.0);
    }
    let z = 1.96_f64;
    let (nf, p) = (n as f64, hits as f64 / n as f64);
    let denom = 1.0 + z * z / nf;
    let centre = p + z * z / (2.0 * nf);
    let margin = z * ((p * (1.0 - p) + z * z / (4.0 * nf)) / nf).sqrt();
    (
        ((centre - margin) / denom).max(0.0),
        ((centre + margin) / denom).min(1.0),
    )
}

/// One labelled row per line: `{"source":"user/correct","correct":true}`. A
/// row the two readers disagreed on has `correct: null` and counts as a miss.
fn precision(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut counts: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for source in PRECISION_SOURCES {
        counts.insert(source.to_string(), (0, 0));
    }
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: Value =
            serde_json::from_str(line).map_err(|e| format!("{}: bad row: {e}", path.display()))?;
        let Some(source) = row.get("source").and_then(Value::as_str) else {
            continue;
        };
        let entry = counts.entry(source.to_string()).or_default();
        entry.1 += 1;
        if row.get("correct").and_then(Value::as_bool) == Some(true) {
            entry.0 += 1;
        }
    }
    let rows: Vec<Value> = counts
        .iter()
        .map(|(source, (hits, n))| {
            let (low, high) = wilson(*hits, *n);
            let point = if *n > 0 {
                *hits as f64 / *n as f64
            } else {
                0.0
            };
            let cleared = *n > 0 && point >= FLOOR_POINT && low >= FLOOR_LOWER;
            json!({"source": source, "hits": hits, "n": n, "precision": point,
                "lower": low, "upper": high,
                "confidence": if cleared { "above_floor" } else { "below_floor" }})
        })
        .collect();
    Ok(json!({"floor": {"point": FLOOR_POINT, "lower": FLOOR_LOWER}, "sources": rows}))
}

fn print_precision(value: &Value) {
    println!(
        "floor: precision {} and lower bound {}",
        value["floor"]["point"], value["floor"]["lower"]
    );
    for s in value["sources"].as_array().into_iter().flatten() {
        println!(
            "{:<18} {:>4}/{:<4} precision {:.3}  [{:.3}, {:.3}]  {}",
            s["source"].as_str().unwrap_or(""),
            s["hits"],
            s["n"],
            s["precision"].as_f64().unwrap_or(0.0),
            s["lower"].as_f64().unwrap_or(0.0),
            s["upper"].as_f64().unwrap_or(0.0),
            s["confidence"].as_str().unwrap_or(""),
        );
    }
}

pub fn run(args: &[OsString]) -> i32 {
    let parsed = match parse(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            print!("{HELP}");
            return 0;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let state = crate::model_catalog::state_dir();
    let result = match &parsed.precision {
        Some(path) => precision(path),
        None => {
            let journal = parsed
                .journal
                .clone()
                .unwrap_or_else(|| state.join("events.jsonl"));
            let cache = crate::state_layout::place_in(&state, "corrections", "cursors.json");
            report(&parsed, &journal, &cache)
        }
    };
    match result {
        Ok(value) if parsed.json => {
            println!("{value}");
            0
        }
        Ok(value) => {
            if parsed.precision.is_some() {
                print_precision(&value);
            } else {
                print_report(&value);
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append(journal: &Path, id: &str, by: &str, kind: &str, model: Value) {
        let stamp = chrono::Utc::now().to_rfc3339();
        let envelope = json!({"ts": stamp, "type": "agent_corrected", "source": "daemon",
            "data": {"correction_id": id, "corrected_by": by, "kind": kind,
                "session_id": "s", "rule_version": "v1", "harness": "claude", "model": model}});
        crate::event_store::append_envelope(journal, &envelope.to_string(), Some(id)).unwrap();
    }

    #[test]
    fn by_day_groups_rows_and_turns_under_the_row_date() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("events.jsonl");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        append(&journal, "a", "user", "correct", json!({}));
        append(&journal, "b", "user", "reject", json!({}));
        let cache = dir.path().join("cursors.json");
        let key = format!("{today}|claude|m");
        std::fs::write(
            &cache,
            json!({"typed_turns": {key: 8, "2999-01-01|claude|m": 4}}).to_string(),
        )
        .unwrap();
        let args = parse(&["--by".into(), "day".into()]).unwrap().unwrap();
        let value = report(&args, &journal, &cache).unwrap();
        let row = &value["blocks"]["user"][0];
        assert_eq!(row["group"], today.as_str());
        assert_eq!(row["count"], 2);
        assert_eq!(row["rate"], 0.25);
        let quiet = &value["blocks"]["user"][1];
        assert_eq!(quiet["group"], "2999-01-01");
        assert_eq!(quiet["rate"], 0.0);
    }

    #[test]
    fn ac7_ac8_ac11_three_blocks_no_total_unmeasured_apart_and_precision_names_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("events.jsonl");
        let opus = json!({"value": "claude-opus-5-5", "basis": "observed"});
        let lied = json!({"value": null, "basis": "unmeasured", "reason": "routed"});
        append(&journal, "a", "user", "correct", opus.clone());
        append(&journal, "b", "user", "interrupt", opus.clone());
        append(&journal, "c", "user", "correct", lied);
        append(&journal, "d", "agent", "correct", opus.clone());
        append(&journal, "e", "machine", "refused", opus);
        let cache = dir.path().join("cursors.json");
        std::fs::write(
            &cache,
            json!({"typed_turns": {"2999-01-01|claude|claude-opus-5-5": 10}}).to_string(),
        )
        .unwrap();
        let args = parse(&[]).unwrap().unwrap();
        let value = report(&args, &journal, &cache).unwrap();
        let blocks = value["blocks"].as_object().unwrap();
        assert_eq!(blocks.len(), 3);
        assert!(value.get("total").is_none());
        let user = blocks["user"].as_array().unwrap();
        let named = user
            .iter()
            .find(|g| g["group"] == "claude-opus-5-5 (observed)")
            .unwrap();
        assert_eq!(named["count"], 1, "the interrupt stays out by default");
        assert_eq!(named["typed_turns"], 10);
        let unmeasured = user.iter().find(|g| g["group"] == "unmeasured").unwrap();
        assert_eq!(unmeasured["count"], 1, "never folded into a named model");

        // AC11: the precision read.
        let file = dir.path().join("labels.jsonl");
        let mut lines = Vec::new();
        for i in 0..100 {
            let hit = i < 95;
            lines.push(json!({"source": "user/reject", "correct": hit}).to_string());
            lines.push(json!({"source": "agent/correct", "correct": i < 60}).to_string());
        }
        std::fs::write(&file, lines.join("\n")).unwrap();
        let value = precision(&file).unwrap();
        let rows = value["sources"].as_array().unwrap();
        assert_eq!(rows.len(), 5, "all five sources print, sampled or not");
        let of = |s: &str| rows.iter().find(|r| r["source"] == s).unwrap();
        assert_eq!(of("user/reject")["confidence"], "above_floor");
        assert_eq!(of("agent/correct")["confidence"], "below_floor");
        assert_eq!(of("machine/refused")["confidence"], "below_floor");
        let lower = of("user/reject")["lower"].as_f64().unwrap();
        assert!((lower - 0.888).abs() < 0.01, "{lower}");
    }
}
