//! The managed-block and evals collectors (doctor.py:3696-3728 and the
//! evals leg of build_report).

use serde_json::{json, Value};

use super::command_call;
use super::state_dir;
use super::{resolve_source, Probes};

const BLOCK_VERSION: i64 = 1;

pub(crate) fn managed_block_report() -> Value {
    let root = walk_to_repo_root();
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let path = root.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(v) = stamped_version(&text) else {
            continue;
        };
        return json!({
            "file": name,
            "stamped": v,
            "current": BLOCK_VERSION,
            "stale": v < BLOCK_VERSION,
        });
    }
    json!({})
}

fn stamped_version(text: &str) -> Option<i64> {
    let re = regex::Regex::new(r"<!-- fno:begin v=(\d+) -->").expect("valid pattern");
    re.captures(text)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

fn walk_to_repo_root() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut root = cwd.clone();
    let mut cur: Option<std::path::PathBuf> = Some(cwd);
    while let Some(dir) = cur {
        let is_repo = dir.join(".git").exists() || dir.join(".fno").is_dir();
        if is_repo {
            root = dir;
            break;
        }
        cur = dir.parent().map(|p| p.to_path_buf());
    }
    root
}

pub(crate) fn evals_summary() -> Value {
    let path = state_dir().join("history/evals-history.jsonl");
    if !path.exists() {
        return Value::Null;
    }
    let payload = command_call(
        &["evals-trend"],
        json!({"op": "summary", "history": path.to_string_lossy(), "stale_days": 7}),
        30,
    )
    .ok()
    .filter(|p| p.get("row_count").and_then(Value::as_i64).unwrap_or(0) > 0)
    .unwrap_or(Value::Null);
    match payload {
        Value::Null => Value::Null,
        p => json!({
            "regression_pass_rate": p.get("regression_pass_rate").cloned().unwrap_or(Value::Null),
            "flake_count": p.get("flake_count").cloned().unwrap_or(Value::from(0)),
            "regression_alarm": p.get("regression_alarm").cloned().unwrap_or(Value::Array(vec![])),
            "regressed": p.get("regressed").cloned().unwrap_or(Value::Array(vec![])),
            "window_days": 7,
            "age_days": p.get("age_days").cloned().unwrap_or(Value::Null),
            "stale": p.get("stale").cloned().unwrap_or(Value::Bool(false)),
            "never_ran": p.get("never_ran").cloned().unwrap_or(Value::Bool(false)),
        }),
    }
}

/// Root freshness, folded natively by the Rust command; transport only
/// (doctor.py:828-855). The verdict comes from the cargo-installed binary's
/// own stage check, not from whichever binary auto-resolves.
pub(crate) fn plugin_cache_report(probes: &Probes) -> Value {
    fn unknown(detail: &str) -> Value {
        json!({
            "status": "unknown",
            "sha": null,
            "installed_at": null,
            "detail": detail,
            "kind": "stage",
            "stage": null,
            "roots": [],
        })
    }
    let binary = probes.str_field("cargo_bin_path");
    let src = resolve_source(None);
    let have_binary = binary.is_some();
    let (Some(binary), Some(src)) = (binary, src) else {
        return unknown(if !have_binary {
            "no cargo fno-agents binary to run the stage check"
        } else {
            "no source checkout to compare against"
        });
    };
    let argv: Vec<String> = vec![
        binary,
        "plugin-install".into(),
        "--check".into(),
        "--json".into(),
        "--source".into(),
        src.to_string_lossy().into_owned(),
    ];
    let Some((code, out, err)) = super::super::bounded_command_for(&argv, 30) else {
        return unknown("plugin-install --check exited -1: transport failure");
    };
    if !matches!(code, 0 | 3 | 4) {
        let tail = if err.trim().is_empty() {
            out.trim()
        } else {
            err.trim()
        };
        return unknown(&format!("plugin-install --check exited {code}: {tail}"));
    }
    let verdict: Option<Value> = serde_json::from_str(&out).ok();
    let named_root = verdict.as_ref().is_some_and(|v| {
        v.is_object()
            && v.get("roots")
                .and_then(Value::as_array)
                .is_some_and(|r| !r.is_empty())
    });
    if !named_root {
        return unknown("plugin-install --check named no readable plugin root");
    }
    verdict.unwrap()
}
