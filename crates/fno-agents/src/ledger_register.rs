//! The target terminal's ledger row writer (the ported `fno.cost._register`).
//!
//! `finalize` calls [`register_target_entry`] in-process on every terminal
//! exit: parse the manifest, build the row from target-state + git + the
//! native session-cost leg, append it under the shared `/tmp/fno-ledger.lock`
//! (the lock [`crate::ledger_axes`] holds too), then emit the
//! `ledger_updated` gate transition event. The append keeps the Python
//! writer's recovery contract: a corrupt ledger is backed up to `.json.bak`
//! and reset - a raised error on the stop-hook completion path is exactly the
//! silent-skip-registration failure this writer exists to remove.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::org_board::NODE_ID_BODY;

/// Python truthiness over a JSON value, for `or`-chains and truthy guards
/// translated from the Python writer.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The first truthy `state[key]` in `keys` - the ported `a or b or ""` chain.
fn or_state<'a>(state: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|k| state.get(*k).filter(|v| truthy(Some(v))))
}

fn state_str(state: &Map<String, Value>, key: &str) -> Option<String> {
    state
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The same read over one JSON row (registry rows are Values, not maps).
fn value_str(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Python `str()` over a parsed manifest value, for the status compare and
/// the raw_status echo.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The delivered-terminal vocabulary, answered by the one owner
/// (`TerminationReason::is_delivered`): parse the reason string back into the
/// enum and ask it. An unparseable reason is not a delivered terminal.
fn reason_is_delivered(reason: &str) -> bool {
    serde_json::from_str::<crate::loopcheck::TerminationReason>(&format!("\"{reason}\""))
        .map(|r| r.is_delivered())
        .unwrap_or(false)
}

/// The scalar a ledger row dedups on: `fno_id` first, `session_id` fallback.
fn scalar_id(row: &Value) -> Option<&str> {
    row.get("fno_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            row.get("session_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
}

/// Parse target-state.md YAML frontmatter into a map, the ported
/// `parse_target_state`: keys between the `---` markers with quote strip and
/// null/true/false coercion, the body `completion_gates:` block under
/// `_gates`, and a second whole-file scan for `graph_node_id` (init writes it
/// BELOW the closing delimiter) validated against the canonical node shape.
fn parse_target_state(state_path: &Path) -> Result<Map<String, Value>, String> {
    let content = std::fs::read_to_string(state_path)
        .map_err(|e| format!("{}: {e}", state_path.display()))?;
    let mut result = Map::new();

    let parts: Vec<&str> = content.split("---").collect();
    let frontmatter = if parts.len() >= 3 {
        parts[1]
    } else {
        content.as_str()
    };

    let key_re = regex::Regex::new(r"^(\w[\w_]*):\s*(.*)$").expect("static regex");
    for line in frontmatter.trim().lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(c) = key_re.captures(line) else {
            continue;
        };
        let key = c[1].to_string();
        let mut value = c[2].trim().to_string();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = value[1..value.len() - 1].to_string();
        }
        let parsed = match value.as_str() {
            "null" | "" => Value::Null,
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(value),
        };
        result.insert(key, parsed);
    }

    let mut gates = Map::new();
    let mut in_gates = false;
    let gate_re = regex::Regex::new(r"^(\w+):\s*(.*)$").expect("static regex");
    for line in content.lines() {
        let stripped = line.trim();
        if stripped == "completion_gates:" {
            in_gates = true;
            continue;
        }
        if !in_gates {
            continue;
        }
        if stripped.is_empty() || (!stripped.starts_with('#') && !stripped.contains(':')) {
            in_gates = false;
            continue;
        }
        if let Some(c) = gate_re.captures(stripped) {
            gates.insert(c[1].to_string(), Value::String(c[2].trim().to_string()));
        }
    }
    result.insert("_gates".to_string(), Value::Object(gates));

    let has_node = matches!(result.get("graph_node_id"), Some(v) if !v.is_null());
    if !has_node {
        let node_line_re =
            regex::Regex::new(r"^\s*graph_node_id:\s*(.*?)\s*$").expect("static regex");
        let shape = regex::Regex::new(&format!("^{NODE_ID_BODY}$")).expect("static regex");
        for line in content.lines() {
            let Some(c) = node_line_re.captures(line) else {
                continue;
            };
            let mut raw = c[1].to_string();
            if raw.len() >= 2
                && ((raw.starts_with('"') && raw.ends_with('"'))
                    || (raw.starts_with('\'') && raw.ends_with('\'')))
            {
                raw = raw[1..raw.len() - 1].to_string();
            }
            if !raw.is_empty() && raw != "null" && shape.is_match(&raw) {
                result.insert("graph_node_id".to_string(), Value::String(raw));
            }
            break;
        }
    }

    Ok(result)
}

/// Run a git command under `cwd` and return stripped output; any failure
/// reads as empty, like the Python `git_cmd`.
fn git_cmd(cwd: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Read lane axes from the registry row owned by this target session: match a
/// row on harness_session_id / short_id (with the registry's legacy
/// per-harness keys synced in) against the manifest's session ids.
fn registry_axes(state: &Map<String, Value>) -> Map<String, Value> {
    let mut candidates: std::collections::HashSet<String> = std::collections::HashSet::new();
    for key in [
        "harness_session_id",
        "claude_transcript_id",
        "codex_thread_id",
    ] {
        if let Some(v) = state_str(state, key) {
            candidates.insert(v);
        }
    }
    if candidates.is_empty() {
        return Map::new();
    }
    let registry = crate::paths::AgentsHome::from_env().registry_json();
    let Ok(rows) = crate::client_commands::read_registry_entries(&registry) else {
        return Map::new();
    };
    for row in &rows {
        let legacy_key = match row.get("harness").and_then(Value::as_str) {
            Some("claude") => Some("claude_session_uuid"),
            Some("codex") => Some("codex_session_id"),
            Some("gemini") => Some("gemini_session_id"),
            _ => None,
        };
        let sid = value_str(row, "harness_session_id")
            .or_else(|| legacy_key.and_then(|k| value_str(row, k)));
        let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(sid) = sid {
            ids.insert(sid);
        }
        if let Some(short) = value_str(row, "short_id") {
            ids.insert(short);
        }
        if candidates.intersection(&ids).next().is_none() {
            continue;
        }
        let mut axes = Map::new();
        for key in ["provider", "model", "effort"] {
            if let Some(v) = row.get(key).and_then(Value::as_str) {
                axes.insert(key.to_string(), Value::String(v.to_string()));
            }
        }
        return axes;
    }
    Map::new()
}

/// `owner/repo` from a git remote URL, or None for a non-GitHub remote: strip
/// scheme and credentials, anchor on the host, and require exactly two path
/// segments (the ported `_slug_from_remote`).
fn slug_from_remote(raw: &str) -> Option<String> {
    let scheme = regex::Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*://").expect("static regex");
    let creds = regex::Regex::new(r"^[^/@]*@").expect("static regex");
    let host = regex::Regex::new(r"(?i)^github\.com(?::\d+)?[:/](.+)$").expect("static regex");
    let s = scheme.replace(raw.trim(), "");
    let s = creds.replace(&s, "");
    let path = host.captures(&s)?[1].to_string();
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    Some(format!("{}/{}", parts[0], parts[1]))
}

/// Read pr_number from the ship handoff artifact the target skill writes at
/// `{root}/.fno/artifacts/handoff/ship-{session_id}.md` (the immutable
/// manifest no longer carries pr_number post-wedge).
fn pr_number_from_ship_artifact(root_path: &str, session_id: &str) -> Option<i64> {
    if root_path.is_empty() || session_id.is_empty() {
        return None;
    }
    let art = Path::new(root_path)
        .join(".fno")
        .join("artifacts")
        .join("handoff")
        .join(format!("ship-{session_id}.md"));
    let text = std::fs::read_to_string(art).ok()?;
    let re = regex::Regex::new(r"^\s*pr_number:\s*(\d+)\s*$").expect("static regex");
    for line in text.lines() {
        if let Some(c) = re.captures(line) {
            return c[1].parse().ok();
        }
    }
    None
}

/// Last-resort PR number: ask gh for the current branch's PR. Best-effort and
/// silent - a no_ship run or a detached worktree simply yields None.
fn pr_number_from_gh(cwd: &Path) -> Option<i64> {
    let out = Command::new("gh")
        .args(["pr", "view", "--json", "number", "-q", ".number"])
        .current_dir(cwd)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Derive completed and skipped phases from what the run did (the ported
/// `derive_phases`): execute always; think/plan skipped when a plan or a
/// node-shaped input bound planning before this session; review+ship
/// completed when a PR resolved; ship skipped when no_ship; the three
/// optional phases skipped per their flags.
fn derive_phases(state: &Map<String, Value>, pr_number: Option<i64>) -> (Vec<String>, Vec<String>) {
    let mut completed: Vec<String> = vec!["execute".into()];
    let mut skipped: Vec<String> = Vec::new();
    let shape = regex::Regex::new(&format!("^{NODE_ID_BODY}$")).expect("static regex");
    let input = match state.get("input") {
        Some(v) if truthy(Some(v)) => py_str(v),
        _ => String::new(),
    };
    let planned_before =
        state.get("plan_path").is_some_and(|v| truthy(Some(v))) || shape.is_match(&input);
    if planned_before {
        skipped.push("think".into());
        skipped.push("plan".into());
    } else {
        completed.push("think".into());
        completed.push("plan".into());
    }
    if pr_number.is_some() {
        completed.push("review".into());
        completed.push("ship".into());
    } else if matches!(state.get("no_ship"), Some(Value::Bool(true))) {
        skipped.push("ship".into());
    }
    for (phase, flag) in [
        ("external", "no_external"),
        ("browser", "no_browser"),
        ("docs", "no_docs"),
    ] {
        if matches!(state.get(flag), Some(Value::Bool(true))) {
            skipped.push(phase.into());
        }
    }
    (completed, skipped)
}

/// Sum estimated_points from plan phase files (the ported
/// `sum_plan_points`): the first `estimated_points:` line per sorted `*.md`,
/// or None when no file carries one.
fn sum_plan_points(plan_path: Option<&str>) -> Option<i64> {
    let dir = Path::new(plan_path?);
    if !dir.is_dir() {
        return None;
    }
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .collect(),
        Err(_) => return None,
    };
    files.sort();
    let re = regex::Regex::new(r"^estimated_points:\s*(\d+)").expect("static regex");
    let mut total = 0i64;
    let mut found_any = false;
    for md in files {
        let Ok(text) = std::fs::read_to_string(&md) else {
            continue;
        };
        for line in text.lines() {
            if let Some(c) = re.captures(line) {
                if let Ok(points) = c[1].parse::<i64>() {
                    total += points;
                    found_any = true;
                }
                break;
            }
        }
    }
    found_any.then_some(total)
}

/// Convert a value to f64 (or truncated int) safely; None on failure (the
/// ported `safe_number`).
fn safe_number(val: Option<&Value>, as_int: bool, decimals: u32) -> Option<f64> {
    let f = match val? {
        Value::Null => return None,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if as_int {
        Some(f.trunc())
    } else {
        Some(crate::session_cost::py_round(f, decimals))
    }
}

/// The PR's GitHub url, or None when no repo slug resolves (the ported
/// `_pr_url_for`): the loose origin regex first, then the checkout slug via
/// `_slug_from_remote` and the `gh repo view` fallback.
fn pr_url_for(cwd: &Path, pr_number: Option<i64>, remote_url: &str) -> Option<String> {
    let n = pr_number?;
    let remote_re = regex::Regex::new(r"github\.com[:/](.+?)(?:\.git)?$").expect("static regex");
    if !remote_url.is_empty() {
        if let Some(c) = remote_re.captures(remote_url) {
            return Some(format!("https://github.com/{}/pull/{n}", &c[1]));
        }
    }
    let slug = slug_from_remote(remote_url).or_else(|| {
        Command::new("gh")
            .args([
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "-q",
                ".nameWithOwner",
            ])
            .current_dir(cwd)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    });
    slug.map(|s| format!("https://github.com/{s}/pull/{n}"))
}

/// Build a ledger.json entry from target-state and git (the ported
/// `build_entry`). `termination_reason` records WHY the session ended;
/// `cost_json` carries the native session-cost output so cost lands without
/// the immutable manifest holding it. Both optional.
pub(crate) fn build_entry(
    cwd: &Path,
    state: &Map<String, Value>,
    session_id: &str,
    termination_reason: Option<&str>,
    cost_json: Option<&Value>,
) -> Value {
    // Git metadata
    let branch = git_cmd(cwd, &["branch", "--show-current"]);
    let remote_url = git_cmd(cwd, &["remote", "get-url", "origin"]);
    let mut project = String::new();
    if !remote_url.is_empty() {
        let re = regex::Regex::new(r"/([^/]+?)(?:\.git)?$").expect("static regex");
        if let Some(c) = re.captures(&remote_url) {
            project = c[1].to_string();
        }
    }
    let worktree_top = git_cmd(cwd, &["rev-parse", "--show-toplevel"]);
    let root_path = worktree_top;
    let canonical_root_path: Option<String> = if root_path.is_empty() {
        None
    } else {
        let base: PathBuf = crate::paths::canonical_repo_root(Path::new(&root_path))
            .unwrap_or_else(|| PathBuf::from(&root_path));
        let resolved = match base.canonicalize() {
            Ok(p) => p,
            Err(_) => base,
        };
        Some(resolved.to_string_lossy().to_string())
    };

    // PR number: the manifest (legacy/back-compat), then the ship handoff
    // artifact, then gh.
    let mut pr_number = state
        .get("pr_number")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .and_then(|s| s.parse::<i64>().ok());
    if pr_number.is_none() {
        let sid = or_state(state, &["fno_id", "session_id"])
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        pr_number = pr_number_from_ship_artifact(&root_path, &sid);
    }
    if pr_number.is_none() {
        pr_number = pr_number_from_gh(cwd);
    }
    let pr_url = pr_url_for(cwd, pr_number, &remote_url);

    // Phases and plan points
    let (phases_completed, phases_skipped) = derive_phases(state, pr_number);
    let plan_path_val = or_state(state, &["plan_path", "plan_dir"]);
    let plan_path = plan_path_val.and_then(Value::as_str).map(str::to_string);
    let points = sum_plan_points(plan_path.as_deref());

    // Iterations
    let iteration = match state.get("iteration") {
        Some(Value::String(s)) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => {
            s.parse::<i64>().unwrap_or(1)
        }
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(1),
        _ => 1,
    };

    // Cost fields: cost_json wins when present; otherwise the manifest values
    // the LLM wrote during pre-promise.
    let empty_cost = Map::new();
    let cj = cost_json.and_then(Value::as_object).unwrap_or(&empty_cost);
    let empty_tokens = Map::new();
    let cj_tokens = match cj.get("tokens") {
        Some(Value::Object(t)) => t,
        _ => &empty_tokens,
    };
    let cost_usd = if cj.contains_key("cost_usd") {
        safe_number(cj.get("cost_usd"), false, 2)
    } else {
        safe_number(state.get("total_cost"), false, 2)
    };
    let tokens_total = if cj_tokens.contains_key("total") {
        safe_number(cj_tokens.get("total"), true, 2)
    } else {
        safe_number(state.get("total_tokens"), true, 2)
    };
    let cache_read = if cj_tokens.contains_key("cache_read") {
        safe_number(cj_tokens.get("cache_read"), true, 2)
    } else {
        safe_number(state.get("cache_read_tokens"), true, 2)
    };
    let duration = if cj.contains_key("duration_minutes") {
        safe_number(cj.get("duration_minutes"), false, 1)
    } else {
        safe_number(state.get("duration_minutes"), false, 1)
    };
    let compactions = if cj.contains_key("compactions") {
        safe_number(cj.get("compactions"), true, 2)
    } else {
        safe_number(state.get("compactions"), true, 2)
    };

    // Model/effort/provider: the registry row wins over cost/manifest values.
    let axes = registry_axes(state);
    let model = state_str(&axes, "model")
        .or_else(|| state_str(cj, "primary_model"))
        .or_else(|| state_str(state, "model"));
    let effort = state_str(&axes, "effort").or_else(|| state_str(state, "effort"));
    let manifest_provider = state
        .get("provider")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut lane_provider =
        state_str(&axes, "provider").or_else(|| state_str(state, "lane_provider"));
    let harness = state.get("harness").and_then(Value::as_str);
    if lane_provider.is_none() && manifest_provider.as_deref() != harness {
        lane_provider = manifest_provider.clone();
    }

    // Sessions: every distinct identifier so hook observations and cost
    // lookups join on every harness.
    let target_sid = or_state(state, &["fno_id", "session_id"])
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let harness_sid = state
        .get("harness_session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let claude_tid = or_state(state, &["claude_session_id", "claude_transcript_id"])
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let codex_tid = state
        .get("codex_thread_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let sessions = crate::ledger_axes::sessions_or_unresolved(&[
        session_id.to_string(),
        target_sid,
        harness_sid,
        claude_tid,
        codex_tid,
    ]);

    // Completion summary path and text
    let summary_path = std::env::var("TARGET_SUMMARY_PATH")
        .ok()
        .filter(|s| !s.is_empty());
    let mut summary_text: Option<String> = None;
    if let Some(sp) = &summary_path {
        if Path::new(sp).is_file() {
            match std::fs::read_to_string(sp) {
                Ok(content) => {
                    let re = regex::Regex::new(r"(?s)## What Was Built\s*\n\s*(.+?)(?:\n\n|\n##)")
                        .expect("static regex");
                    if let Some(c) = re.captures(&content) {
                        summary_text = Some(c[1].trim().chars().take(200).collect());
                    }
                }
                Err(e) => eprintln!("Warning: could not extract summary: {e}"),
            }
        }
    }

    // Terminal status resolution: ABORTED -> aborted, COMPLETE/empty/
    // IN_PROGRESS -> done (the stop hook only calls on terminal paths),
    // anything else -> unknown with the raw value preserved.
    let raw_status = state.get("status").filter(|v| !v.is_null());
    let raw_status_str = match &raw_status {
        Some(v) => py_str(v).to_uppercase(),
        None => String::new(),
    };
    let entry_status = match raw_status_str.as_str() {
        "ABORTED" => "aborted",
        "COMPLETE" | "" | "IN_PROGRESS" => "done",
        _ => "unknown",
    };

    let mut abort_reason = std::env::var("TARGET_ABORT_REASON")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| state_str(state, "abort_reason"));
    if entry_status == "aborted" && abort_reason.is_none() {
        abort_reason = Some("unspecified".to_string());
    }

    // Scalar identity fields: dedup and graph sync join on these.
    let scalar_session_id = or_state(state, &["fno_id", "session_id"])
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let scalar_graph_node_id = state_str(state, "graph_node_id");

    let mut e = Map::new();
    e.insert("type".into(), json!("execution"));
    e.insert("status".into(), json!(entry_status));
    match state.get("input") {
        Some(v) => {
            e.insert("title".into(), v.clone());
        }
        None => {
            e.insert("title".into(), json!("Untitled"));
        }
    }
    e.insert(
        "summary".into(),
        summary_text.map(|s| json!(s)).unwrap_or(Value::Null),
    );
    e.insert(
        "summary_path".into(),
        summary_path.map(|s| json!(s)).unwrap_or(Value::Null),
    );
    e.insert(
        "plan_path".into(),
        plan_path_val.cloned().unwrap_or(Value::Null),
    );
    e.insert(
        "pr_number".into(),
        pr_number.map(|n| json!(n)).unwrap_or(Value::Null),
    );
    e.insert(
        "pr_url".into(),
        pr_url.map(|u| json!(u)).unwrap_or(Value::Null),
    );
    e.insert(
        "branch".into(),
        if branch.is_empty() {
            Value::Null
        } else {
            json!(branch)
        },
    );
    e.insert(
        "project".into(),
        if project.is_empty() {
            Value::Null
        } else {
            json!(project)
        },
    );
    e.insert(
        "root_path".into(),
        if root_path.is_empty() {
            Value::Null
        } else {
            json!(root_path)
        },
    );
    e.insert(
        "canonical_root_path".into(),
        canonical_root_path.map(|s| json!(s)).unwrap_or(Value::Null),
    );
    // worktree key kept (rendered as null) for legacy JSON readers; root_path
    // is the authoritative worktree path.
    e.insert("worktree".into(), Value::Null);
    e.insert(
        "started".into(),
        crate::ledger_axes::utc_iso(state.get("created_at").and_then(Value::as_str))
            .map(|s| json!(s))
            .unwrap_or(Value::Null),
    );
    let completed = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    e.insert("completed".into(), json!(completed));
    e.insert(
        "duration_minutes".into(),
        match duration {
            Some(d) if d != 0.0 => json!(crate::session_cost::py_round(d, 1)),
            _ => Value::Null,
        },
    );
    e.insert("iterations".into(), json!(iteration));
    e.insert(
        "compactions".into(),
        compactions.map(|f| json!(f as i64)).unwrap_or(Value::Null),
    );
    e.insert(
        "cost_usd".into(),
        cost_usd
            .map(|f| json!(crate::session_cost::py_round(f, 2)))
            .unwrap_or(Value::Null),
    );
    e.insert(
        "tokens_total".into(),
        tokens_total.map(|f| json!(f as i64)).unwrap_or(Value::Null),
    );
    e.insert(
        "cache_read_tokens".into(),
        cache_read.map(|f| json!(f as i64)).unwrap_or(Value::Null),
    );
    e.insert(
        "model".into(),
        model.map(|s| json!(s)).unwrap_or(Value::Null),
    );
    e.insert(
        "effort".into(),
        effort.map(|s| json!(s)).unwrap_or(Value::Null),
    );
    // Dual-write for one release: new rows carry both keys (same value).
    e.insert(
        "fno_id".into(),
        scalar_session_id
            .clone()
            .map(|s| json!(s))
            .unwrap_or(Value::Null),
    );
    e.insert(
        "session_id".into(),
        scalar_session_id.map(|s| json!(s)).unwrap_or(Value::Null),
    );
    e.insert(
        "graph_node_id".into(),
        scalar_graph_node_id
            .map(|s| json!(s))
            .unwrap_or(Value::Null),
    );
    e.insert("sessions".into(), sessions);
    e.insert("phases_completed".into(), json!(phases_completed));
    e.insert("phases_skipped".into(), json!(phases_skipped));
    e.insert(
        "points".into(),
        points.map(|p| json!(p)).unwrap_or(Value::Null),
    );
    e.insert("notes".into(), Value::Null);
    if let Some(lp) = &lane_provider {
        e.insert("provider".into(), json!(lp));
    }
    // Abort bookkeeping only when the run actually aborted.
    if entry_status == "aborted" {
        e.insert(
            "reason".into(),
            abort_reason.map(|s| json!(s)).unwrap_or(Value::Null),
        );
    }
    if entry_status == "unknown" {
        let raw = match &raw_status {
            Some(v) => py_str(v),
            None => String::new(),
        };
        e.insert("raw_status".into(), json!(raw));
    }
    // Provider attribution: propagate from target-state.md when present;
    // omit keys entirely when absent, never write them as null.
    let provider_id = or_state(state, &["provider_id", "provider"])
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(pid) = provider_id {
        e.insert("provider_id".into(), json!(pid));
    }
    if let Some(aid) = state.get("account_id").filter(|v| !v.is_null()) {
        e.insert("account_id".into(), aid.clone());
    }
    if let Some(tr) = termination_reason.filter(|t| !t.is_empty()) {
        e.insert("termination_reason".into(), json!(tr));
    }
    Value::Object(e)
}

/// Load a ledger.json into the canonical `{"entries": [...]}` shape (the
/// ported `_load_ledger_data`). A missing file yields an empty ledger; a
/// corrupt file or a shape that cannot satisfy the append contract is backed
/// up to `.json.bak` and reset. A bare-list legacy ledger is normalized into
/// the wrapper.
fn load_ledger(tasks_path: &Path) -> Result<Value, String> {
    if !tasks_path.exists() {
        return Ok(json!({ "entries": [] }));
    }
    let text = std::fs::read_to_string(tasks_path)
        .map_err(|e| format!("{} unreadable: {e}", tasks_path.display()))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(entries)) => Ok(json!({ "entries": entries })),
        Ok(v @ Value::Object(_)) => {
            if v.get("entries").is_some_and(Value::is_array) {
                Ok(v)
            } else {
                eprintln!(
                    "Warning: {} unexpected shape, creating backup",
                    tasks_path.display()
                );
                Ok(backup_and_reset(tasks_path))
            }
        }
        Ok(_) => {
            eprintln!(
                "Warning: {} unexpected shape, creating backup",
                tasks_path.display()
            );
            Ok(backup_and_reset(tasks_path))
        }
        Err(_) => {
            eprintln!("Warning: {} corrupt, creating backup", tasks_path.display());
            Ok(backup_and_reset(tasks_path))
        }
    }
}

fn backup_and_reset(tasks_path: &Path) -> Value {
    let stem = tasks_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "ledger".to_string());
    let bak = tasks_path.with_file_name(format!("{stem}.json.bak"));
    let _ = std::fs::rename(tasks_path, bak);
    json!({ "entries": [] })
}

fn drop_backstop_row(row: &Value, node: &str) -> bool {
    truthy(row.get("backstop")) && row.get("graph_node_id").and_then(Value::as_str) == Some(node)
}

/// Append entry to a ledger.json file atomically under the ledger flock (the
/// ported `append_to_tasks_json`): same-session dedup with delivered-terminal
/// promotion, and the collapse rule that keeps a node's backstop row from
/// coexisting with a full-fidelity row.
fn append_to_tasks_json(tasks_path: &Path, entry: &Value) -> Result<(), String> {
    if let Some(parent) = tasks_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::ledger_axes::LOCK_PATH)
        .map_err(|e| format!("cannot open {}: {e}", crate::ledger_axes::LOCK_PATH))?;
    let result = {
        use std::os::fd::AsRawFd;
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
        let r = append_locked(tasks_path, entry);
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
        r
    };
    result
}

fn append_locked(tasks_path: &Path, entry: &Value) -> Result<(), String> {
    let mut data = load_ledger(tasks_path)?;
    let entries = data
        .get_mut("entries")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| format!("{}: no entries list", tasks_path.display()))?;

    // Same-session race dedupe under flock: reject a second write from the
    // SAME target session that raced past the caller's pre-check. Promotion
    // applies when the new reason is a delivered terminal and the old is not.
    if let Some(scalar) = scalar_id(entry).map(str::to_string) {
        for index in 0..entries.len() {
            if scalar_id(&entries[index]) != Some(scalar.as_str()) {
                continue;
            }
            // Copied out before the &mut promote: the reason strings are
            // read off the old row, which promote_entry replaces. A missing
            // or non-string reason reads "None", which parses as no terminal.
            let old_disp = entries[index]
                .get("termination_reason")
                .and_then(Value::as_str)
                .unwrap_or("None")
                .to_string();
            let new_disp = entry
                .get("termination_reason")
                .and_then(Value::as_str)
                .unwrap_or("None")
                .to_string();
            let new_delivered = reason_is_delivered(&new_disp);
            let old_delivered = reason_is_delivered(&old_disp);
            if new_delivered && !old_delivered {
                promote_entry(entries, index, entry);
                crate::ledger_axes::write_ledger(tasks_path, &data)?;
                eprintln!("Promoted target fno_id {scalar} from {old_disp} to {new_disp}");
                return Ok(());
            }
            eprintln!("Skipping duplicate entry for target fno_id: {scalar}");
            return Ok(());
        }
    }

    // Collapse rule: a full-fidelity row supersedes reconcile's backstop floor
    // for the same node - the backstop has no fno_id, so the scalar dedup
    // above cannot catch it and this drop is the only guard against a
    // double-counted cost rollup.
    if let Some(node) = value_str(entry, "graph_node_id") {
        entries.retain(|e| !drop_backstop_row(e, &node));
    }
    entries.push(entry.clone());
    let count = entries.len();
    crate::ledger_axes::write_ledger(tasks_path, &data)?;
    eprintln!("Appended entry #{count} to {}", tasks_path.display());
    Ok(())
}

/// Replace `entries[index]` with the promoted row: existing fields kept, the
/// new entry's non-null fields layered on, `sessions` unioned, and any
/// backstop row for the same node dropped (a promoted full-fidelity row must
/// never coexist with the node's backstop row).
fn promote_entry(entries: &mut Vec<Value>, index: usize, entry: &Value) {
    let mut promoted = entries[index].clone();
    if let (Some(dst), Some(src)) = (promoted.as_object_mut(), entry.as_object()) {
        for (k, v) in src {
            if !v.is_null() {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    let mut aliases: Vec<Value> = Vec::new();
    for source in [promoted.get("sessions"), entry.get("sessions")] {
        if let Some(Value::Array(vals)) = source {
            for v in vals {
                if !aliases.contains(v) {
                    aliases.push(v.clone());
                }
            }
        }
    }
    let promoted_node = value_str(&promoted, "graph_node_id");
    if !aliases.is_empty() {
        if let Some(dst) = promoted.as_object_mut() {
            dst.insert("sessions".into(), Value::Array(aliases));
        }
    }
    entries[index] = promoted;
    if let Some(node) = &promoted_node {
        let mut kept: Vec<Value> = Vec::new();
        for (j, e) in entries.iter().enumerate() {
            if j != index && drop_backstop_row(e, node) {
                continue;
            }
            kept.push(e.clone());
        }
        *entries = kept;
    }
}

/// Emit a phase_transition event for the ledger_updated gate (the ported
/// `_emit_ledger_transition`): read the provenance_nonce from the session's
/// state file via the entry's root_path and commit the event through the
/// store. Silent no-op when anything is missing - the ledger write itself
/// already succeeded, so missing telemetry never fails the caller.
fn emit_ledger_transition(entry: &Value) {
    let Some(root_path) = value_str(entry, "root_path") else {
        return;
    };
    let state_path = Path::new(&root_path).join(".fno").join("target-state.md");
    if !state_path.is_file() {
        return;
    }
    let Ok(content) = std::fs::read_to_string(&state_path) else {
        return;
    };
    let nonce = content
        .lines()
        .find(|l| l.starts_with("provenance_nonce:"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string());
    let Some(nonce) = nonce.filter(|n| !n.is_empty() && n != "null") else {
        return;
    };
    // The target session_id (scalar), never sessions[0]: the stop hook's
    // verify_provenance greps events.jsonl by the target session_id read from
    // target-state.md, so emitting with the transcript UUID makes the event
    // invisible to the ledger_updated gate.
    let Some(session_id) = value_str(entry, "session_id") else {
        eprintln!(
            "Warning: _emit_ledger_transition skipped - entry missing target session_id; ledger_updated gate event NOT emitted"
        );
        return;
    };
    let data = json!({
        "gate_bearing": true,
        "phase": "register",
        "nonce": nonce,
        "session_id": session_id,
        "gate": "ledger_updated",
    });
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let event = json!({
        "ts": ts,
        "type": "phase_transition",
        "source": "target",
        "data": data,
    });
    let events = Path::new(&root_path).join(".fno").join("events.jsonl");
    // Best-effort: the ledger append already succeeded.
    let _ = crate::event_store::append_envelope(&events, &event.to_string(), None);
}

/// Write the entry to the global ledger.json and emit the gate transition
/// (the ported `register_entry`). The ledger is cross-project by definition
/// (one row per terminal session across every repo), so the single writer
/// path is the global `paths.ledger_json()` answer.
pub(crate) fn register_target_entry(
    cwd: &Path,
    manifest: &Path,
    transcript_uuid: &str,
    termination_reason: &str,
    cost_json: Option<&Value>,
) -> Result<(), String> {
    let state = parse_target_state(manifest)?;
    let entry = build_entry(
        cwd,
        &state,
        transcript_uuid,
        Some(termination_reason),
        cost_json,
    );
    let ledger = crate::paths_cli::ledger_json(cwd);
    append_to_tasks_json(&ledger, &entry)?;
    emit_ledger_transition(&entry);
    println!(
        "{}",
        serde_json::to_string_pretty(&entry).unwrap_or_default()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn minimal_state() -> Map<String, Value> {
        let mut s = Map::new();
        s.insert("session_id".into(), json!("tid-1"));
        s
    }

    fn write_manifest(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("target-state.md");
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Hold the env lock and clear the two env reads build_entry makes, so a
    /// developer's exported TARGET_SUMMARY_PATH / TARGET_ABORT_REASON cannot
    /// leak into a fixture.
    struct EntryEnv {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EntryEnv {
        fn clean() -> Self {
            let lock = crate::claims::test_env_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for key in ["TARGET_SUMMARY_PATH", "TARGET_ABORT_REASON"] {
                std::env::remove_var(key);
            }
            Self { _lock: lock }
        }
    }

    impl Drop for EntryEnv {
        fn drop(&mut self) {
            for key in ["TARGET_SUMMARY_PATH", "TARGET_ABORT_REASON"] {
                std::env::remove_var(key);
            }
        }
    }

    #[test]
    fn parse_target_state_parses_frontmatter_gates_and_body_node() {
        let dir = TempDir::new().unwrap();
        let body = concat!(
            "---\n",
            "session_id: s1\n",
            "input: \"a quoted input\"\n",
            "no_ship: true\n",
            "iteration: 3\n",
            "model: null\n",
            "---\n",
            "# Target Session State\n",
            "completion_gates:\n",
            "think: done\n",
            "review: pending\n",
            "graph_node_id: ab-testnode\n"
        );
        let p = write_manifest(dir.path(), body);
        let state = parse_target_state(&p).unwrap();
        assert_eq!(state["session_id"], "s1");
        assert_eq!(state["input"], "a quoted input");
        assert_eq!(state["no_ship"], Value::Bool(true));
        assert_eq!(state["iteration"], "3");
        assert_eq!(state["model"], Value::Null);
        let gates = state["_gates"].as_object().unwrap();
        assert_eq!(gates["think"], "done");
        assert_eq!(gates["review"], "pending");
        assert_eq!(state["graph_node_id"], "ab-testnode");
    }

    #[test]
    fn parse_target_state_rejects_parenthetical_node_id() {
        let dir = TempDir::new().unwrap();
        let body = concat!(
            "---\n",
            "session_id: s1\n",
            "---\n",
            "# Target Session State\n",
            "graph_node_id: x (deprecated)\n",
            "graph_node_id: ab-testnode\n"
        );
        let p = write_manifest(dir.path(), body);
        let state = parse_target_state(&p).unwrap();
        assert!(state.get("graph_node_id").is_none());
    }

    #[test]
    fn derive_phases_ports_the_python_shape() {
        let mut state = minimal_state();
        let (completed, skipped) = derive_phases(&state, None);
        // Bare input: think/plan ran in-session.
        assert_eq!(completed, vec!["execute", "think", "plan"]);
        assert!(skipped.is_empty());

        // A bound plan skips think/plan.
        state.insert("plan_path".into(), json!("plans/x/"));
        let (completed, skipped) = derive_phases(&state, None);
        assert_eq!(completed, vec!["execute"]);
        assert_eq!(skipped, vec!["think", "plan"]);

        // A node-shaped input binds planning the same way.
        state.remove("plan_path");
        state.insert("input".into(), json!("ab-1234 ship the thing"));
        let (completed, _) = derive_phases(&state, Some(7));
        assert_eq!(completed, vec!["execute", "review", "ship"]);

        // no_ship + optional-phase flags skip without a PR.
        let mut state = minimal_state();
        state.insert("no_ship".into(), Value::Bool(true));
        state.insert("no_external".into(), Value::Bool(true));
        let (completed, skipped) = derive_phases(&state, None);
        assert_eq!(completed, vec!["execute", "think", "plan"]);
        assert_eq!(skipped, vec!["ship", "external"]);
    }

    #[test]
    fn build_entry_minimal_run_reads_done_and_unresolved_sessions() {
        let _env = EntryEnv::clean();
        let dir = TempDir::new().unwrap();
        let state = minimal_state();
        let entry = build_entry(dir.path(), &state, "transcript-uuid", None, None);
        assert_eq!(entry["type"], "execution");
        assert_eq!(entry["status"], "done");
        assert_eq!(entry["title"], "Untitled");
        assert_eq!(entry["sessions"], json!(["unresolved:no-harness-session"]));
        assert_eq!(
            entry["phases_completed"],
            json!(["execute", "think", "plan"])
        );
        assert_eq!(entry["phases_skipped"], json!([]));
        // Non-repo tempdir: git metadata degrades to null.
        assert_eq!(entry["branch"], Value::Null);
        assert_eq!(entry["root_path"], Value::Null);
        assert_eq!(entry["pr_number"], Value::Null);
        assert_eq!(entry["cost_usd"], Value::Null);
        assert_eq!(entry["model"], Value::Null);
        assert_eq!(entry["iterations"], json!(1));
        assert_eq!(entry["fno_id"], Value::Null);
        assert_eq!(entry["session_id"], Value::Null);
        // No provider key: manifest_provider (absent) == harness (absent), so
        // the lane fallback never fires.
        assert!(entry.get("provider").is_none());
    }

    #[test]
    fn build_entry_cost_json_wins_over_manifest() {
        let _env = EntryEnv::clean();
        let dir = TempDir::new().unwrap();
        let mut state = minimal_state();
        state.insert("total_cost".into(), json!("99.99"));
        state.insert("total_tokens".into(), json!("111"));
        let cost = json!({
            "cost_usd": 1.234,
            "tokens": {"total": 100, "cache_read": 10},
            "duration_minutes": 5.0,
            "compactions": 0,
            "primary_model": "m-1",
        });
        let entry = build_entry(dir.path(), &state, "tid", None, Some(&cost));
        assert_eq!(entry["cost_usd"], json!(1.23));
        assert_eq!(entry["tokens_total"], json!(100));
        assert_eq!(entry["cache_read_tokens"], json!(10));
        assert_eq!(entry["duration_minutes"], json!(5.0));
        assert_eq!(entry["compactions"], json!(0));
        assert_eq!(entry["model"], "m-1");
    }

    #[test]
    fn build_entry_status_resolution_ports_the_python_chain() {
        let _env = EntryEnv::clean();
        let dir = TempDir::new().unwrap();

        // ABORTED with no reason resolves to "unspecified".
        let mut state = minimal_state();
        state.insert("status".into(), json!("ABORTED"));
        let entry = build_entry(dir.path(), &state, "tid", None, None);
        assert_eq!(entry["status"], "aborted");
        assert_eq!(entry["reason"], "unspecified");

        // TARGET_ABORT_REASON overrides the manifest. The EntryEnv guard
        // above already holds the env lock, and a std Mutex is not
        // re-entrant: set the var directly instead of taking it again.
        std::env::set_var("TARGET_ABORT_REASON", "operator-cancelled");
        let entry = build_entry(dir.path(), &state, "tid", None, None);
        std::env::remove_var("TARGET_ABORT_REASON");
        drop(lock);
        assert_eq!(entry["reason"], "operator-cancelled");

        // An unknown status preserves the raw value.
        let mut state = minimal_state();
        state.insert("status".into(), json!("WEIRD"));
        let entry = build_entry(dir.path(), &state, "tid", None, None);
        assert_eq!(entry["status"], "unknown");
        assert_eq!(entry["raw_status"], "WEIRD");
    }

    fn seed_ledger(path: &Path, entries: Value) {
        std::fs::write(path, serde_json::to_string_pretty(&entries).unwrap() + "\n").unwrap();
    }

    fn read_ledger(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn append_dedup_skips_a_second_write() {
        let dir = TempDir::new().unwrap();
        let ledger = dir.path().join("ledger.json");
        seed_ledger(
            &ledger,
            json!({"entries": [{"fno_id": "t1", "termination_reason": "Budget"}]}),
        );
        let entry = json!({"fno_id": "t1", "termination_reason": "Budget", "cost_usd": 5.0});
        append_to_tasks_json(&ledger, &entry).unwrap();
        let doc = read_ledger(&ledger);
        let entries = doc["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["termination_reason"], "Budget");
        // The skip must not have layered the new row's fields onto the old one.
        assert!(entries[0].get("cost_usd").is_none());
    }

    #[test]
    fn append_promotes_a_delivered_terminal_and_collapses_backstop() {
        let dir = TempDir::new().unwrap();
        let ledger = dir.path().join("ledger.json");
        seed_ledger(
            &ledger,
            json!({"entries": [
                {"fno_id": "t1"},
                {"backstop": true, "graph_node_id": "ab-node"}
            ]}),
        );
        let entry = json!({
            "fno_id": "t1",
            "graph_node_id": "ab-node",
            "termination_reason": "DonePRGreen",
            "sessions": ["u2"],
        });
        append_to_tasks_json(&ledger, &entry).unwrap();
        let doc = read_ledger(&ledger);
        let entries = doc["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "backstop row collapsed: {doc}");
        assert_eq!(entries[0]["fno_id"], "t1");
        assert_eq!(entries[0]["termination_reason"], "DonePRGreen");
        assert_eq!(entries[0]["sessions"], json!(["u2"]));
    }

    #[test]
    fn append_collapses_backstop_on_plain_append() {
        let dir = TempDir::new().unwrap();
        let ledger = dir.path().join("ledger.json");
        seed_ledger(
            &ledger,
            json!({"entries": [{"backstop": true, "graph_node_id": "ab-node"}]}),
        );
        let entry = json!({
            "fno_id": "t2",
            "graph_node_id": "ab-node",
            "termination_reason": "Budget",
        });
        append_to_tasks_json(&ledger, &entry).unwrap();
        let doc = read_ledger(&ledger);
        let entries = doc["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].get("backstop").is_none());
    }

    #[test]
    fn append_corrupt_ledger_backed_up_and_reset() {
        let dir = TempDir::new().unwrap();
        let ledger = dir.path().join("ledger.json");
        std::fs::write(&ledger, "not json").unwrap();
        let entry = json!({"fno_id": "t3"});
        append_to_tasks_json(&ledger, &entry).unwrap();
        assert!(dir.path().join("ledger.json.bak").exists(), "backup made");
        let doc = read_ledger(&ledger);
        assert_eq!(doc["entries"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn sum_plan_points_sums_sorted_md_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("01-a.md"), "estimated_points: 2\n").unwrap();
        std::fs::write(
            dir.path().join("02-b.md"),
            "intro\nestimated_points: 3\nmore\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.txt"), "estimated_points: 9\n").unwrap();
        assert_eq!(sum_plan_points(Some(dir.path().to_str().unwrap())), Some(5));
        assert_eq!(sum_plan_points(None), None);
        assert_eq!(sum_plan_points(Some("/nonexistent-dir")), None);
    }

    #[test]
    fn slug_from_remote_parses_every_git_form() {
        assert_eq!(
            slug_from_remote("git@github.com:o/r.git"),
            Some("o/r".into())
        );
        assert_eq!(
            slug_from_remote("https://github.com/o/r"),
            Some("o/r".into())
        );
        assert_eq!(
            slug_from_remote("ssh://git@github.com:22/o/r.git"),
            Some("o/r".into())
        );
        assert_eq!(
            slug_from_remote("https://github.com/o/r/"),
            Some("o/r".into())
        );
        assert_eq!(slug_from_remote("https://gitlab.com/o/r.git"), None);
    }

    #[test]
    fn pr_url_for_uses_the_loose_origin_regex_first() {
        let dir = TempDir::new().unwrap();
        let url = pr_url_for(dir.path(), Some(358), "git@github.com:o/r.git");
        assert_eq!(url, Some("https://github.com/o/r/pull/358".to_string()));
    }

    #[test]
    fn reason_is_delivered_asks_the_enum_owner() {
        assert!(reason_is_delivered("DonePRGreen"));
        assert!(reason_is_delivered("DoneAdvisory"));
        assert!(!reason_is_delivered("Budget"));
        assert!(!reason_is_delivered("delegated"));
        assert!(!reason_is_delivered(""));
    }
}
