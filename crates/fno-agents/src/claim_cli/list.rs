//! The `claim list` leaf, ported from `cli/src/fno/claims/cli.py::list_cmd`.
//! Reads every candidate claims root (an explicit `--root`, else the global
//! root plus the node-aware root for the prefix), merges the rows under the
//! Python state priority (first key wins ties), and renders the four output
//! branches in the Python order: the `--silent-after` report, `--json`, the
//! empty message, the human rows. The goldens frozen from the Python leaf
//! (`tests/claim_list_parity.rs`) are the contract.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// One key's merge result: the winning state, the claims dir that owns it
/// (the UNRESOLVED spelling, which is what the human rows print), and the
/// visible row (`None` when the state is hidden and `--include-stale` is
/// absent).
struct MergeEntry {
    state: String,
    root: String,
    row: Option<Value>,
}

pub fn run(args: &[String]) -> i32 {
    let mut prefix = String::new();
    let mut include_stale = false;
    let mut json_output = false;
    let mut root: Option<PathBuf> = None;
    let mut silent_after: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--prefix" => match it.next() {
                Some(v) => prefix = v.clone(),
                None => return bare_error("Error: Option '--prefix' requires an argument."),
            },
            "--include-stale" => include_stale = true,
            "--json" | "-J" => json_output = true,
            "--root" => match it.next() {
                Some(v) => root = Some(PathBuf::from(v)),
                None => return bare_error("Error: Option '--root' requires an argument."),
            },
            "--silent-after" => match it.next() {
                Some(v) => silent_after = Some(v.clone()),
                None => return bare_error("Error: Option '--silent-after' requires an argument."),
            },
            other => return usage_error(&format!("Error: No such option: {other}")),
        }
    }

    let mut threshold_s = 0.0f64;
    if let Some(raw) = &silent_after {
        match super::parse_ttl_expression(raw) {
            Ok(Some(ms)) => threshold_s = ms as f64 / 1000.0,
            Ok(None) => {
                return usage_error(
                    "Error: Invalid value: --silent-after requires a duration, e.g. '15m'",
                )
            }
            Err(e) => return usage_error(&format!("Error: Invalid value: {e}")),
        }
    }

    // The candidate roots, in the Python order: an explicit --root names the
    // one store; the bare form folds the global root and the prefix's
    // node-aware root (None for an empty or repo-local prefix).
    let candidates: Vec<Option<PathBuf>> = match &root {
        Some(r) => vec![Some(r.clone())],
        None => vec![
            crate::claims_root::global_claims_root(),
            super::node_aware_root(&prefix),
        ],
    };
    // Mirror `dedup_claims_roots`: the key is the resolved claims dir, the
    // first occurrence wins. Under pinned roots both bare-form candidates
    // collapse to one; the branch exists for production's split roots.
    let mut deduped: Vec<(Option<PathBuf>, PathBuf)> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        let cdir = claims_dir_of(candidate.as_deref());
        let key = cdir.canonicalize().unwrap_or_else(|_| cdir.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        deduped.push((candidate, cdir));
    }

    // Every candidate scans with stale rows INCLUDED (the totals and the
    // include-stale hint count states the default view hides); the
    // visibility filter applies to each row inside the merge, exactly where
    // the Python side filters (its counts stay whole while the hidden rows
    // drop out of the listing).
    let prefix_opt: Option<&str> = (!prefix.is_empty()).then_some(prefix.as_str());
    let mut best: BTreeMap<String, MergeEntry> = BTreeMap::new();
    for (candidate, cdir) in &deduped {
        let listed = match candidate {
            Some(r) => {
                crate::claims::list_in(&[r.join(crate::claims::CLAIMS_DIRNAME)], prefix_opt, true)
            }
            None => crate::claims::list(prefix_opt, None, true),
        };
        let rows = match listed {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("fno-agents: claim list: {error}");
                return 1;
            }
        };
        for rec in &rows {
            let row = crate::claim_verbs::claim_status_value(rec);
            let state = row.get("state").and_then(Value::as_str).unwrap_or("");
            let visible = include_stale || matches!(state, "live" | "suspect");
            let owned_root = cdir.display().to_string();
            match best.get_mut(&row_key(&row)) {
                Some(entry) if state_priority(state) >= state_priority(&entry.state) => continue,
                Some(entry) => {
                    entry.state = state.to_string();
                    entry.root = owned_root;
                    entry.row = visible.then_some(row);
                }
                None => {
                    best.insert(
                        row_key(&row),
                        MergeEntry {
                            state: state.to_string(),
                            root: owned_root,
                            row: visible.then_some(row),
                        },
                    );
                }
            }
        }
    }

    let mut stale = 0usize;
    let mut corrupted = 0usize;
    let mut free = 0usize;
    for entry in best.values() {
        match entry.state.as_str() {
            "stale" => stale += 1,
            "corrupted" => corrupted += 1,
            "free" => free += 1,
            _ => {}
        }
    }
    let visible: Vec<(&String, &MergeEntry)> =
        best.iter().filter(|(_, e)| e.row.is_some()).collect();

    if silent_after.is_some() {
        return run_silence_report(&visible, threshold_s, json_output);
    }
    if json_output {
        let rows: Vec<Value> = visible
            .iter()
            .map(|(_, e)| e.row.clone().unwrap_or(Value::Null))
            .collect();
        println!("{}", Value::Array(rows));
        return 0;
    }
    if visible.is_empty() {
        let roots = deduped.len();
        let plural = if roots != 1 { "s" } else { "" };
        let hint = if !include_stale && (stale > 0 || corrupted > 0) {
            "; --include-stale to list them"
        } else {
            ""
        };
        println!(
            "no live claims ({stale} stale, {corrupted} corrupted, \
             {free} released mid-scan across {roots} root{plural}){hint}"
        );
        return 0;
    }
    for (key, entry) in &visible {
        let row = entry.row.as_ref().unwrap();
        println!(
            "{:<9} {:<32} holder={} pid={} host={} root={}",
            entry.state,
            key,
            render_field(row.get("holder")),
            render_field(row.get("pid")),
            render_field(row.get("host")),
            entry.root,
        );
    }
    0
}

/// The `--silent-after` branch: live rows whose transcript's last event is
/// older than the threshold. A row with no target-session holder or no
/// worktree/cwd metadata cannot be aged and lands in `unreadable`; so does
/// one whose transcript cannot be resolved or read.
fn run_silence_report(
    visible: &[(&String, &MergeEntry)],
    threshold_s: f64,
    json_output: bool,
) -> i32 {
    let mut scanned = 0usize;
    let mut silent: Vec<Value> = Vec::new();
    let mut unreadable: Vec<Value> = Vec::new();
    for (key, entry) in visible {
        let row = entry.row.as_ref().unwrap();
        if row.get("state").and_then(Value::as_str) != Some("live") {
            continue;
        }
        scanned += 1;
        let holder = row.get("holder").and_then(Value::as_str).unwrap_or("");
        let session = holder
            .strip_prefix("target-session:")
            .filter(|s| !s.is_empty());
        let cwd = row.get("metadata").and_then(|m| {
            m.get("worktree")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    m.get("cwd")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
        });
        let age_s = session.zip(cwd).and_then(|(s, c)| {
            crate::backlog::worked::transcript_age_s(s, c, &super::harness_for_session(s))
        });
        match age_s {
            None => unreadable.push(json!({
                "key": key,
                "holder": holder,
                "age_s": Value::Null,
            })),
            Some(age) if age > threshold_s => silent.push(json!({
                "key": key,
                "holder": holder,
                "age_s": age,
            })),
            Some(_) => {}
        }
    }
    if json_output {
        println!(
            "{}",
            json!({"scanned": scanned, "silent": silent, "unreadable": unreadable})
        );
        return 0;
    }
    let minutes = fmt_g(threshold_s / 60.0);
    let mut out = format!(
        "scanned {scanned} live claim(s); {} silent past {minutes}m",
        silent.len()
    );
    for r in &silent {
        out.push_str(&format!(
            "\n  SILENT {} holder={} age={:.1}m",
            r["key"].as_str().unwrap_or(""),
            r["holder"].as_str().unwrap_or(""),
            r["age_s"].as_f64().unwrap_or(0.0) / 60.0
        ));
    }
    for r in &unreadable {
        out.push_str(&format!(
            "\n  unreadable {} holder={}",
            r["key"].as_str().unwrap_or(""),
            r["holder"].as_str().unwrap_or("")
        ));
    }
    println!("{out}");
    0
}

/// The claims directory one list candidate reads, mirroring Python
/// `io.claims_dir`: an explicit or global root nests `.fno/claims` under it;
/// the rootless fallback takes the `$FNO_CLAIMS_ROOT` override, else the
/// repo's space (which joins `claims` directly, no nested segment). An
/// unreadable cwd yields a placeholder that only shapes the dedup key; the
/// scan behind that candidate reports the read failure itself.
fn claims_dir_of(root: Option<&Path>) -> PathBuf {
    if let Some(r) = root {
        return r.join(crate::claims::CLAIMS_DIRNAME);
    }
    if let Some(override_root) = std::env::var_os("FNO_CLAIMS_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(override_root).join(crate::claims::CLAIMS_DIRNAME);
    }
    match std::env::current_dir() {
        Ok(cwd) => crate::paths::space_dir(&cwd).join("claims"),
        Err(_) => PathBuf::from(crate::claims::CLAIMS_DIRNAME),
    }
}

/// The Python merge priority: a healthier state beats an unhealthier one,
/// and an unknown state loses to everything (first occurrence wins ties).
fn state_priority(state: &str) -> usize {
    match state {
        "live" => 0,
        "suspect" => 1,
        "stale" => 2,
        "corrupted" => 3,
        "free" => 4,
        _ => 5,
    }
}

/// Python `r.get(field, "-")`: the "-" default only covers an ABSENT key; a
/// present null prints Python's `None`, a bool its `True`/`False`.
fn render_field(v: Option<&Value>) -> String {
    match v {
        None => "-".to_string(),
        Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => "-".to_string(),
    }
}

/// Python's `{m:g}`: six significant digits, scientific below 1e-4 or from
/// 1e6 up. The exponent comes from the `{:e}` form, so the six-digit
/// rounding carry is already in the exponent and 999999.9 formats as
/// `1e+06`, like %g does.
fn fmt_g(m: f64) -> String {
    if m == 0.0 {
        return "0".to_string();
    }
    let sci = format!("{m:.5e}");
    let (mant_s, exp_s) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let mut exp: i32 = exp_s.parse().unwrap_or(0);
    let mut mant: f64 = mant_s.parse().unwrap_or(1.0);
    if mant >= 10.0 {
        mant /= 10.0;
        exp += 1;
    }
    let trim = |mut s: String| -> String {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
        s
    };
    let mantissa = trim(format!("{mant:.5}"));
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    } else {
        trim(format!("{:.*}", (5 - exp).max(0) as usize, m))
    }
}

#[cfg(test)]
mod fmt_g_tests {
    use super::fmt_g;

    #[test]
    fn matches_python_g() {
        assert_eq!(fmt_g(0.0), "0");
        assert_eq!(fmt_g(60.0), "60");
        assert_eq!(fmt_g(0.25), "0.25");
        assert_eq!(fmt_g(1.5), "1.5");
        assert_eq!(fmt_g(1.0 / 60.0), "0.0166667");
        assert_eq!(fmt_g(61.0 / 60.0), "1.01667");
        assert_eq!(fmt_g(1e-4), "0.0001");
        assert_eq!(fmt_g(1.0 / 60000.0), "1.66667e-05");
        assert_eq!(fmt_g(999999.0), "999999");
        assert_eq!(fmt_g(999999.9), "1e+06");
        assert_eq!(fmt_g(1e6), "1e+06");
        assert_eq!(fmt_g(1234567.0), "1.23457e+06");
    }
}

fn row_key(row: &Value) -> String {
    row.get("key")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn usage_error(detail: &str) -> i32 {
    super::usage_refusal(
        "fno agents claim list [OPTIONS]",
        "fno agents claim list --help",
        detail,
    )
}

/// typer's missing-parameter refusal prints the bare Error line with no
/// Usage block (unlike an unknown option, which gets the full layout).
fn bare_error(detail: &str) -> i32 {
    eprintln!("{detail}");
    2
}
