//! `fno config history`: config-write receipts from the event journals.
//!
//! Reads the same three journals the writers route to (global, agents home,
//! project space) plus `.ephemeral` siblings and numeric rotations, newest
//! row first per journal, and renders the receipt table or JSONL exactly as
//! the deleted Python leg printed it.

use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The captured `--help` page, byte for byte as typer printed it.
const HELP: &str = concat!(
    "Usage: fno config history [OPTIONS] [KEY]\n",
    "\n",
    "  Read config-write receipts from the global and project journals.\n",
    "\n",
    "Arguments:\n",
    "  [KEY]  Exact config key or dotted prefix to filter.\n",
    "\n",
    "Options:\n",
    "  --limit INTEGER RANGE         Maximum rows to print.  [default: 50; x>=1]\n",
    "  -J, --json                    Emit matching rows as JSONL.\n",
    "  --scope [global|project|all]  Limit rows by the config scope that was written.\n",
    "                                [default: all]\n",
    "  --help                        Show this message and exit.\n",
    "",
);

pub fn run(args: &[OsString]) -> i32 {
    let tokens: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let mut key: Option<String> = None;
    let mut limit: i64 = 50;
    let mut json_out = false;
    let mut scope: String = "all".to_string();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].clone();
        match token.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return 0;
            }
            "--limit" => {
                index += 1;
                let Some(text) = tokens.get(index) else {
                    return option_requires_argument(&token);
                };
                let Ok(parsed) = text.parse::<i64>() else {
                    return usage_error(&format!(
                        "Invalid value for '--limit': {text} is not a valid integer."
                    ));
                };
                if parsed < 1 {
                    return usage_error(&format!(
                        "Invalid value for '--limit': {parsed} is not in the range x>=1."
                    ));
                }
                limit = parsed;
            }
            "--json" | "-J" => json_out = true,
            "--scope" => {
                index += 1;
                let Some(text) = tokens.get(index) else {
                    return option_requires_argument(&token);
                };
                if !matches!(text.as_str(), "global" | "project" | "all") {
                    return usage_error(&format!(
                        "Invalid value for '--scope': '{text}' is not one of 'global', 'project', 'all'."
                    ));
                }
                scope = text.clone();
            }
            t if t.starts_with('-') && t != "-" => {
                return usage_error(&format!("No such option: {t}"));
            }
            t => {
                if key.is_some() {
                    return usage_error(&format!("Got unexpected extra argument ({t})"));
                }
                key = Some(t.to_string());
            }
        }
        index += 1;
    }
    let rows = read_rows(key.as_deref(), limit, &scope);
    if rows.is_empty() {
        let names: Vec<String> = event_journals()
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        println!(
            "no config_write rows; searched journals: {}",
            names.join(", ")
        );
        return 0;
    }
    if json_out {
        for row in &rows {
            println!("{}", serde_json::to_string(row).unwrap_or_default());
        }
        return 0;
    }
    for row in &rows {
        let ts = row.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        let empty = Value::Object(serde_json::Map::new());
        let data = row.get("data").unwrap_or(&empty);
        let key_text = data.get("key").and_then(|v| v.as_str()).unwrap_or("");
        let old_value = display_value(data, "old_value", "present_before");
        let new_value = display_value(data, "new_value", "present_after");
        let attester = data
            .get("attester_session_id")
            .and_then(|v| v.as_str())
            .filter(|text| !text.is_empty())
            .unwrap_or("(none)");
        let witness = data
            .get("attester_witness")
            .and_then(|v| v.as_str())
            .filter(|text| !text.is_empty())
            .unwrap_or("(unknown)");
        let scope_text = data
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let root_kind = data
            .get("root_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let config_path = data
            .get("config_path")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        println!(
            "{ts} {key_text} {old_value} -> {new_value} {scope_text}/{root_kind} {config_path} session {attester} ({witness})"
        );
    }
    0
}

fn display_value(data: &Value, field: &str, presence: &str) -> String {
    let present = match data.get(presence) {
        Some(Value::Bool(false)) => false,
        Some(Value::Null) => false,
        Some(_) => true,
        None => false,
    };
    if !present {
        return "(unset)".to_string();
    }
    let Some(value) = data.get(field) else {
        return "(unset)".to_string();
    };
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// The journal rows the command answers: newest first, `limit`-bounded per
/// journal, `type == config_write` rows only, scope- and key-filtered.
fn read_rows(key: Option<&str>, limit: i64, scope: &str) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    for journal in event_journals() {
        let mut matched = 0;
        for line in reversed_lines(&journal) {
            if matched >= limit {
                break;
            }
            let Ok(parsed) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            let Some(map) = parsed.as_object() else {
                continue;
            };
            if map.get("type").and_then(|t| t.as_str()) != Some("config_write") {
                continue;
            }
            let data_empty = Value::Object(serde_json::Map::new());
            let data = map.get("data").unwrap_or(&data_empty);
            let Some(data_map) = data.as_object() else {
                continue;
            };
            let Some(row_key) = data_map.get("key").and_then(|k| k.as_str()) else {
                continue;
            };
            if scope != "all" && data_map.get("scope").and_then(|s| s.as_str()) != Some(scope) {
                continue;
            }
            if let Some(key) = key {
                let prefix = format!("{key}.");
                if row_key != key && !row_key.starts_with(&prefix) {
                    continue;
                }
            }
            rows.push(parsed);
            matched += 1;
        }
    }
    rows.sort_by(|left, right| {
        let left_ts = left.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        let right_ts = right.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        right_ts.cmp(left_ts)
    });
    rows.truncate(limit as usize);
    rows
}

/// `_reversed_lines`: the journal's lines newest-first. Bounded per journal
/// by the caller; the whole file is read here because the rotations list the
/// reader walks stays small.
fn reversed_lines(path: &Path) -> Vec<Vec<u8>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    let mut lines: Vec<Vec<u8>> = bytes
        .split(|byte| *byte == b'\n')
        .map(|line| line.to_vec())
        .collect();
    if lines.last().map(|line| line.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.reverse();
    lines
}

/// `event_journals`: every journal and retained rotation, oldest first: the
/// three live journals, each's `.ephemeral` sibling when present, and each
/// base's numeric rotations, resolved and de-duplicated.
pub fn event_journals() -> Vec<PathBuf> {
    let global = global_journal();
    let agents = agents_home_dir().join("events.jsonl");
    let project = project_journal();
    let mut resolved: Vec<PathBuf> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for live in [global, agents, project] {
        let resolved_live = or_absolute(&live);
        let mut bases: Vec<PathBuf> = vec![resolved_live.clone()];
        let name = resolved_live
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "events.jsonl".to_string());
        let ephemeral = resolved_live.with_file_name(format!("{name}.ephemeral"));
        if ephemeral.exists() {
            bases.push(ephemeral);
        }
        for base in bases {
            let parent = match base.parent() {
                Some(parent) => parent.to_path_buf(),
                None => continue,
            };
            let prefix = format!(
                "{}.",
                base.file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
            let mut rotated = numeric_rotations(&parent, &prefix);
            rotated.sort_by(|left, right| right.0.cmp(&left.0));
            let mut candidates: Vec<PathBuf> =
                rotated.into_iter().map(|(_number, path)| path).collect();
            candidates.push(base.clone());
            for candidate in candidates {
                let path = or_absolute(&candidate);
                if !seen.contains(&path) {
                    seen.insert(path.clone());
                    resolved.push(path);
                }
            }
        }
    }
    resolved
}

/// Numeric `.<n>` rotations of one journal base: an all-digit suffix,
/// existing.
fn numeric_rotations(parent: &Path, prefix: &str) -> Vec<(i64, PathBuf)> {
    let mut rotated = Vec::new();
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(_) => return rotated,
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(suffix) = name.strip_prefix(prefix) else {
            continue;
        };
        let is_digits = !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit());
        if is_digits && entry.path().exists() {
            if let Ok(number) = suffix.parse::<i64>() {
                rotated.push((number, entry.path()));
            }
        }
    }
    rotated
}

fn or_absolute(path: &Path) -> PathBuf {
    match path.canonicalize() {
        Ok(resolved) => resolved,
        Err(_) => absolutize(path),
    }
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// `global_events_json`: the `FNO_STATE_DIR` pin, else the configured
/// `state_dir` when absolute, else `~/.fno`; plus `events.jsonl`.
pub(crate) fn global_journal() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_STATE_DIR") {
        if !pin.is_empty() {
            return super::leaves::state_dir_value().join("events.jsonl");
        }
    }
    let configured = super::merged_config(&super::repo_root())
        .get("state_dir")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    match configured {
        Some(raw) if Path::new(&raw).is_absolute() => {
            super::leaves::state_dir_value().join("events.jsonl")
        }
        _ => super::home_dir().join(".fno").join("events.jsonl"),
    }
}

/// `agents_home_dir`: `FNO_AGENTS_HOME` (expanduser + resolve) else
/// `~/.fno/agents`.
fn agents_home_dir() -> PathBuf {
    match std::env::var_os("FNO_AGENTS_HOME") {
        Some(override_dir) if !override_dir.is_empty() => {
            super::resolve_path(&PathBuf::from(override_dir))
        }
        _ => super::home_dir().join(".fno").join("agents"),
    }
}

/// `project_events_json`: the `FNO_EVENTS_PATH` pin, else the space's
/// journal. The checkout-migration write the Python resolver ran is a
/// read-only fold here: a legacy checkout journal waits for the next writer.
pub(crate) fn project_journal() -> PathBuf {
    match std::env::var_os("FNO_EVENTS_PATH") {
        Some(pin) if !pin.is_empty() => PathBuf::from(pin),
        _ => crate::config_doctor::project_space_dir(&crate::config_doctor::Settings::load())
            // events-discipline:allow: a path resolver; the writes in reach are in-file test fixtures
            .join("events.jsonl"),
    }
}

fn option_requires_argument(option: &str) -> i32 {
    let usage = "fno config history [OPTIONS] [KEY]";
    eprintln!("Usage: {usage}");
    eprintln!("Try 'fno config history --help' for help.");
    eprintln!();
    eprintln!("Error: Option '{option}' requires an argument.");
    2
}

fn usage_error(message: &str) -> i32 {
    let usage = "fno config history [OPTIONS] [KEY]";
    eprintln!("Usage: {usage}");
    eprintln!("Try 'fno config history --help' for help.");
    eprintln!();
    eprintln!("Error: {message}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_rotations_reads_only_digit_suffixes() {
        let dir =
            std::env::temp_dir().join(format!("config-history-rotations-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl.1"), b"one\n").unwrap();
        std::fs::write(dir.join("events.jsonl.2"), b"two\n").unwrap();
        std::fs::write(dir.join("events.jsonl.ephemeral"), b"e\n").unwrap();
        std::fs::write(dir.join("events.jsonl.tmp"), b"t\n").unwrap();
        let mut rotations = numeric_rotations(&dir, "events.jsonl.");
        rotations.sort();
        let names: Vec<String> = rotations
            .iter()
            .map(|(number, path)| {
                format!("{number}:{}", path.file_name().unwrap().to_string_lossy())
            })
            .collect();
        assert_eq!(names, vec!["1:events.jsonl.1", "2:events.jsonl.2"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
