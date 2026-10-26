//! The four small `config` leaves and their renderers: `build-dir`,
//! `assert-subagent-budget`, `status-sinks`, and `active-backlog`.
//!
//! Each answers from the same loader-replay merge as `config get`; the
//! active-backlog renderer wraps the fno-agents receipt door the Python leg
//! execed through `rust_binary.call_binary_json`, byte for byte, including
//! the pre-338c list-shape fallback.

use super::{
    aliased_layers_at, chain_at, home_dir, parse_config_file, repo_root, resolve_path,
    unwrap_config_dict, worktree_local_override,
};
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The captured `--help` pages, byte for byte as typer printed them.
const BUDGET_HELP: &str = concat!(
    "Usage: fno config assert-subagent-budget [OPTIONS]\n",
    "\n",
    "  Refuse a declared fan-out wider than the provider's subagent budget.\n",
    "\n",
    "  Prints the verdict's reason and exits 0 on a permit, 1 on a refusal. Fails\n",
    "  open: no stamp, no budget entry or an unreadable config permits, with the\n",
    "  reason saying so. This is the seam skill text calls before declaring a panel\n",
    "  width (Locked Decision 7).\n",
    "\n",
    "Options:\n",
    "  -w, --width INTEGER  The fan-out width the caller declares.  [required]\n",
    "  -P, --provider TEXT  Provider whose budget applies; defaults to the\n",
    "                       FNO_ROUTE_PROVIDER stamp.\n",
    "  --help               Show this message and exit.\n",
    "",
);

const SINKS_HELP: &str = concat!(
    "Usage: fno config status-sinks [OPTIONS]\n",
    "\n",
    "  Resolve which projects the status-fanout supervisor should tick.\n",
    "\n",
    "  A project is a target when it has >=1 enabled ``status_sinks`` entry -\n",
    "  INDEPENDENT of ``config.active_backlog``. The daemon shells this to discover\n",
    "  its fanout loops. Read-only and best-effort: a malformed config yields an\n",
    "  empty list, never an error.\n",
    "\n",
    "Options:\n",
    "  -J, --json  Emit a JSON list of fanout targets for the daemon.\n",
    "  --help      Show this message and exit.\n",
    "",
);

const BACKLOG_HELP: &str = concat!(
    "Usage: fno config active-backlog [OPTIONS]\n",
    "\n",
    "  Print the active-backlog drain reading. Full contract:\n",
    "  docs/architecture/coordination.md#per-territory-team-cap\n",
    "\n",
    "  JSON readers call the binary door directly: fno-agents active-backlog-receipt\n",
    "\n",
    "Options:\n",
    "  --help  Show this message and exit.\n",
    "",
);

const BUILDDIR_HELP: &str = concat!(
    "Usage: fno config build-dir [OPTIONS]\n",
    "\n",
    "  Print the CARGO_BUILD_BUILD_DIR value: ``<base>/{workspace-path-hash}``.\n",
    "\n",
    "  The shell surface over fno.paths.cargo_build_dir_value(); the target stop hook\n",
    "  exports it for done_probes that run cargo. Hidden: consumed by the hook, not\n",
    "  by operators.\n",
    "\n",
    "Options:\n",
    "  --help  Show this message and exit.\n",
    "",
);

/// `fno config build-dir`: `<base>/{workspace-path-hash}`.
pub fn run_build_dir(args: &[OsString]) -> i32 {
    let tokens: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    for token in &tokens {
        match token.as_str() {
            "--help" | "-h" => {
                print!("{BUILDDIR_HELP}");
                return 0;
            }
            t if t.starts_with('-') && t != "-" => {
                return usage_error(
                    "fno config build-dir [OPTIONS]",
                    &format!("No such option: {t}"),
                );
            }
            t => {
                return usage_error(
                    "fno config build-dir [OPTIONS]",
                    &format!("Got unexpected extra argument ({t})"),
                );
            }
        }
    }
    println!("{}", cargo_build_dir_value());
    0
}

/// `fno.paths.cargo_build_dir_value` over the native merge: the
/// `paths.cargo_targets_base` override, else `<state_dir>/cargo-build`.
pub fn cargo_build_dir_value() -> String {
    let raw = super::merged_config(&repo_root());
    let override_base = super::raw_get(&raw, "paths.cargo_targets_base")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let base = match override_base {
        Some(configured) => resolve_path(&PathBuf::from(expanduser(&configured))),
        None => state_dir_value().join("cargo-build"),
    };
    format!("{}/{{workspace-path-hash}}", base.to_string_lossy())
}

/// `fno.paths.state_dir`: the `FNO_STATE_DIR` pin, else the merged
/// `state_dir` key, else `~/.fno`.
pub fn state_dir_value() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_STATE_DIR") {
        if !pin.is_empty() {
            return resolve_path(Path::new(&pin));
        }
    }
    let raw = super::merged_config(&repo_root());
    match super::raw_get(&raw, "state_dir").and_then(|v| v.as_str()) {
        Some(configured) => resolve_path(&PathBuf::from(expanduser(configured))),
        None => home_dir().join(".fno"),
    }
}

/// Python `os.path.expanduser` for the `~/` shapes this reader meets.
fn expanduser(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home_dir().to_string_lossy()),
        None => match path.strip_prefix('~') {
            Some(rest) => format!("{}{rest}", home_dir().to_string_lossy()),
            None => path.to_string(),
        },
    }
}

/// `fno config assert-subagent-budget`: the declaration check the skills call
/// before a fan-out. Fails open on every unreadable input, exactly like the
/// hub's `provider_subagent_budget`.
pub fn run_budget(args: &[OsString]) -> i32 {
    let tokens: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let mut width: Option<String> = None;
    let mut provider: Option<String> = None;
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].clone();
        match token.as_str() {
            "--help" | "-h" => {
                print!("{BUDGET_HELP}");
                return 0;
            }
            "--width" | "-w" => {
                index += 1;
                match tokens.get(index) {
                    Some(value) => width = Some(value.clone()),
                    None => {
                        return usage_error(
                            "fno config assert-subagent-budget [OPTIONS]",
                            "Option '--width' / '-w' requires an argument.",
                        );
                    }
                }
            }
            "--provider" | "-P" => {
                index += 1;
                match tokens.get(index) {
                    Some(value) => provider = Some(value.clone()),
                    None => {
                        return usage_error(
                            "fno config assert-subagent-budget [OPTIONS]",
                            "Option '--provider' / '-P' requires an argument.",
                        );
                    }
                }
            }
            t if t.starts_with('-') && t != "-" => {
                return usage_error(
                    "fno config assert-subagent-budget [OPTIONS]",
                    &format!("No such option: {t}"),
                );
            }
            t => {
                return usage_error(
                    "fno config assert-subagent-budget [OPTIONS]",
                    &format!("Got unexpected extra argument ({t})"),
                );
            }
        }
        index += 1;
    }
    let Some(width_text) = width else {
        return usage_error(
            "fno config assert-subagent-budget [OPTIONS]",
            "Missing option '--width' / '-w'.",
        );
    };
    let stamp = provider.unwrap_or_else(|| {
        std::env::var("FNO_ROUTE_PROVIDER")
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    if stamp.is_empty() || stamp == "unknown" {
        println!("permit: no route stamp; budget unknown");
        return 0;
    }
    let Ok(parsed) = width_text.parse::<i64>() else {
        // The typer option already enforced the integer at parse time; the
        // hub's unreadable-width permit was for the None/shape cases the
        // option rejects before the body runs.
        return usage_error(
            "fno config assert-subagent-budget [OPTIONS]",
            &format!("Invalid value for '--width' / '-w': '{width_text}' is not a valid integer."),
        );
    };
    match subagent_budget(&stamp) {
        None => {
            println!("permit: no budget entry for {stamp}; uncapped");
            0
        }
        Some(budget) if parsed <= budget => {
            println!("permit: width {parsed} within {stamp} budget {budget}");
            0
        }
        Some(budget) => {
            println!(
                "{stamp} declares a subagent budget of {budget}; declared width {parsed} exceeds it - run inline or split the work"
            );
            1
        }
    }
}

/// `provider_subagent_budget`: `agents.provider_limits.<provider>.subagents`
/// (int >= 1), the legacy `max_lanes` spelling honored, `None` (uncapped) on
/// every unreadable input.
fn subagent_budget(provider: &str) -> Option<i64> {
    let raw = super::merged_config(&repo_root());
    let agents = raw.get("agents").and_then(|agents| agents.as_object())?;
    let limits = agents
        .get("provider_limits")
        .or_else(|| agents.get("max_lanes"))
        .and_then(|limits| limits.as_object())?;
    let budget = limits.get(provider).and_then(|budget| budget.as_object())?;
    let as_int = budget.get("subagents").and_then(|value| value.as_i64())?;
    (as_int >= 1).then_some(as_int)
}

/// `fno config status-sinks`: the projects the fanout supervisor ticks, read
/// the way `resolve_fanout_targets` reads them: the work map, then each
/// project's own chain.
pub fn run_status_sinks(args: &[OsString]) -> i32 {
    let tokens: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let mut json_out = false;
    for token in &tokens {
        match token.as_str() {
            "--help" | "-h" => {
                print!("{SINKS_HELP}");
                return 0;
            }
            "--json" | "-J" => json_out = true,
            t if t.starts_with('-') && t != "-" => {
                return usage_error(
                    "fno config status-sinks [OPTIONS]",
                    &format!("No such option: {t}"),
                );
            }
            t => {
                return usage_error(
                    "fno config status-sinks [OPTIONS]",
                    &format!("Got unexpected extra argument ({t})"),
                );
            }
        }
    }
    let targets = fanout_targets();
    if json_out {
        println!("{}", py_json_list(&targets));
        return 0;
    }
    if targets.is_empty() {
        println!("status-sinks: no projects with enabled sinks");
        return 0;
    }
    for target in &targets {
        let project = target.get("project").and_then(|v| v.as_str()).unwrap_or("");
        let cwd = target.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
        let interval = target
            .get("interval_seconds")
            .and_then(|v| v.as_i64())
            .unwrap_or(1);
        println!("{project}\t{cwd}\tinterval={interval}s");
    }
    0
}

/// `fanout_targets_as_dicts`: work map, then per-project chain read. A
/// project whose config fails the shape check (a malformed sink entry, a
/// sub-minimum interval) contributes nothing, the way a refused
/// `load_settings_for_repo` skipped it.
pub fn fanout_targets() -> Vec<Value> {
    let mut targets = Vec::new();
    for (name, cwd) in workspace_map() {
        if cwd.is_empty() {
            continue;
        }
        let raw = merged_at_unpinned(Path::new(&cwd));
        let shape_ok = shape_ok(&raw);
        if !shape_ok {
            continue;
        }
        let sinks = raw
            .get("status_sinks")
            .and_then(|sinks| sinks.as_array())
            .cloned()
            .unwrap_or_default();
        let any_enabled = sinks.iter().any(|sink| {
            sink.get("enabled")
                .map(|enabled| enabled.as_bool().unwrap_or(true))
                .unwrap_or(true)
        });
        if !any_enabled {
            continue;
        }
        let interval = raw
            .get("status_fanout")
            .and_then(|fanout| fanout.get("interval_secs"))
            .and_then(|value| value.as_i64())
            .unwrap_or(5);
        let mut row = Map::new();
        row.insert("project".to_string(), Value::String(name));
        row.insert("cwd".to_string(), Value::String(cwd));
        row.insert("interval_seconds".to_string(), Value::from(interval.max(1)));
        targets.push(Value::Object(row));
    }
    targets
}

/// The slice of the settings model the fanout read depends on: every sink an
/// object with a boolean (or absent) `enabled`, and the fanout interval at
/// least one when present.
fn shape_ok(raw: &Value) -> bool {
    if let Some(sinks) = raw.get("status_sinks") {
        let Some(sinks) = sinks.as_array() else {
            return false;
        };
        for sink in sinks {
            let Some(sink) = sink.as_object() else {
                return false;
            };
            if let Some(enabled) = sink.get("enabled") {
                if !enabled.is_boolean() {
                    return false;
                }
            }
        }
    }
    if let Some(fanout) = raw
        .get("status_fanout")
        .and_then(|fanout| fanout.as_object())
    {
        if let Some(interval) = fanout.get("interval_secs") {
            match interval.as_i64() {
                Some(interval) if interval >= 1 => {}
                _ => return false,
            }
        }
    }
    true
}

/// `_workspace_paths`: project name to normalized absolute path, from the
/// work map (project-local config first, then global; multi-workspace shape,
/// then the legacy flat shape).
fn workspace_map() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let root = repo_root();
    // The work map reads the project's own config first, then the global file,
    // each location config.toml-first.
    let mut files: Vec<PathBuf> = Vec::new();
    files.push(root.join(".fno").join("config.toml"));
    files.push(root.join(".fno").join("settings.yaml"));
    files.push(global_settings_path().with_file_name("config.toml"));
    files.push(global_settings_path());
    for path in files {
        if !path.is_file() {
            continue;
        }
        let Some(parsed) = parse_config_file(&path) else {
            continue;
        };
        let flat = unwrap_config_dict(&parsed);
        let Some(work) = flat.get("work").and_then(|work| work.as_object()) else {
            continue;
        };
        if let Some(workspaces) = work
            .get("workspaces")
            .and_then(|workspaces| workspaces.as_object())
        {
            for (_name, ws) in workspaces {
                let Some(projects) = ws.get("projects").and_then(|projects| projects.as_array())
                else {
                    continue;
                };
                for project in projects {
                    let Some(project) = project.as_object() else {
                        continue;
                    };
                    let name = project
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let raw_path = project
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    out.push((name, normalize_path(&raw_path)));
                }
            }
        }
        if let Some(flat_projects) = work
            .get("projects")
            .and_then(|projects| projects.as_object())
        {
            for (name, cfg) in flat_projects {
                let Some(cfg) = cfg.as_object() else { continue };
                let raw_path = cfg
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                out.push((name.clone(), normalize_path(&raw_path)));
            }
        }
    }
    out
}

fn global_settings_path() -> PathBuf {
    match std::env::var_os("FNO_GLOBAL_SETTINGS_PATH") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home_dir().join(".fno").join("settings.yaml"),
    }
}

/// `os.path.normpath` over an absolute path: `.` and `..` folded, duplicate
/// and trailing slashes dropped.
fn normalize_path(raw: &str) -> String {
    let expanded = expanduser(raw);
    let path = PathBuf::from(&expanded);
    let absolute = path.is_absolute();
    let mut parts: Vec<&str> = Vec::new();
    for component in expanded.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if !parts.is_empty() && parts.last() != Some(&"..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    if absolute {
        let mut out = String::new();
        for part in &parts {
            out.push('/');
            out.push_str(part);
        }
        if out.is_empty() {
            out.push('/');
        }
        out
    } else {
        parts.join("/")
    }
}

/// The loader merge anchored at `root` WITHOUT the `FNO_CONFIG` pin, the way
/// `load_settings_for_repo` reads a foreign repo's config.
fn merged_at_unpinned(root: &Path) -> Value {
    let mut raw = Value::Object(Map::new());
    let layers = aliased_layers_at(root);
    for (_path, parsed) in layers.iter().rev() {
        deep_merge(&mut raw, parsed);
    }
    if let Some(top) = chain_at(root).first() {
        let local = top.parent().map(|dir| dir.join("config.local.toml"));
        if let Some(local) = local {
            if local.is_file() && !super::is_symlink(&local) {
                if let Some(parsed) = parse_config_file(&local) {
                    if let Some(override_value) = worktree_local_override(&parsed) {
                        deep_merge(&mut raw, &override_value);
                    }
                }
            }
        }
    }
    raw
}

/// `fno config active-backlog`: the drain reading rendered from the
/// fno-agents receipt door.
pub fn run_active_backlog(args: &[OsString]) -> i32 {
    let tokens: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    for token in &tokens {
        match token.as_str() {
            "--help" | "-h" => {
                print!("{BACKLOG_HELP}");
                return 0;
            }
            t => {
                return usage_error(
                    "fno config active-backlog [OPTIONS]",
                    &format!("Got unexpected extra argument ({t})"),
                );
            }
        }
    }
    let (error, reading) = call_binary_json("active-backlog-receipt");
    let Some(error) = error else {
        return render_receipt(reading.unwrap_or(Value::Null));
    };
    eprintln!("active-backlog: {error}");
    1
}

fn render_receipt(reading: Value) -> i32 {
    let reading = if reading.is_array() {
        // A pre-338c binary prints a bare target list.
        let targets = reading;
        let missions = targets.as_array().map(|items| items.len()).unwrap_or(0);
        let mut wrapped = Map::new();
        wrapped.insert("targets".to_string(), targets);
        wrapped.insert("missions".to_string(), Value::from(missions as i64));
        wrapped.insert("skip_reason".to_string(), Value::Null);
        Value::Object(wrapped)
    } else {
        reading
    };
    let empty_targets = Value::Array(Vec::new());
    let targets = reading.get("targets").unwrap_or(&empty_targets);
    let is_empty = targets
        .as_array()
        .map(|items| items.is_empty())
        .unwrap_or(true);
    if is_empty {
        let reason = reading
            .get("skip_reason")
            .and_then(|reason| reason.as_str())
            // The Python leg's `or` folded an empty string to the default
            // alongside null/absent; the fold is part of the contract.
            .filter(|reason| !reason.is_empty())
            .unwrap_or("no_missions");
        if reason == "no_missions" {
            println!("active-backlog: no territories to drain");
            return 0;
        }
        let causes: [(&str, &str); 4] = [
            (
                "drain_disabled",
                "the drain is disabled: config.active_backlog.enabled",
            ),
            (
                "bad_interval",
                "invalid interval: config.active_backlog.interval",
            ),
            (
                "project_disabled",
                "the most common drop is a disabled project: config.active_backlog.enabled",
            ),
            (
                "no_workspace_path",
                "the most common drop is a missing workspace path",
            ),
        ];
        let cause = causes
            .iter()
            .find(|(key, _)| *key == reason)
            .map(|(_, text)| *text)
            .unwrap_or(reason);
        let missions = reading
            .get("missions")
            .and_then(|missions| missions.as_i64())
            .unwrap_or(0);
        println!("active-backlog: {missions} drainable territories, 0 targets ({cause})");
        return 0;
    }
    let Some(rows) = targets.as_array() else {
        return 0;
    };
    for row in rows {
        let scope = row.get("scope").and_then(|v| v.as_str()).unwrap_or("");
        let cwd = row.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
        let interval = row
            .get("interval_seconds")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let failure_limit = row
            .get("failure_limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let mission = row.get("mission").and_then(|v| v.as_str()).unwrap_or("");
        let mission_part = if mission.is_empty() {
            String::new()
        } else {
            format!(" mission={mission}")
        };
        println!(
            "{scope}\t{cwd}\tinterval={interval}s\tfailure_limit={failure_limit}{mission_part}"
        );
    }
    0
}

/// `call_binary_json` over the sibling binary, with the Python door's error
/// strings.
fn call_binary_json(command: &str) -> (Option<String>, Option<Value>) {
    let binary = crate::digest_overlay::fno_agents_bin();
    let output = Command::new(&binary).arg(command).output();
    let Ok(output) = output else {
        return (Some("fno-agents binary not found".to_string()), None);
    };
    if !output.status.success() {
        let detail = if output.stderr.is_empty() {
            "command failed".to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).trim().to_string()
        };
        let detail: String = detail.chars().take(200).collect();
        return (Some(detail), None);
    }
    match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(parsed) => (None, Some(parsed)),
        Err(_) => (Some("unreadable JSON receipt".to_string()), None),
    }
}

fn py_json_list(items: &[Value]) -> String {
    let inner: Vec<String> = items.iter().map(super::get::py_json).collect();
    format!("[{}]", inner.join(", "))
}

pub(super) fn usage_error(usage: &str, message: &str) -> i32 {
    let leaf = usage
        .trim_end_matches(" [KEY]")
        .trim_end_matches(" [HARNESS]")
        .trim_end_matches(" [OPTIONS]");
    eprintln!("Usage: {usage}");
    eprintln!("Try '{leaf} --help' for help.");
    eprintln!();
    eprintln!("Error: {message}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanduser_matches_the_python_shapes() {
        std::env::set_var("HOME", "/tmp/fake-home");
        assert_eq!(expanduser("~/code/x"), "/tmp/fake-home/code/x");
        assert_eq!(expanduser("/abs/path"), "/abs/path");
    }

    #[test]
    fn normalize_path_folds_dots_and_slashes() {
        assert_eq!(normalize_path("/a/b/../c//d/"), "/a/c/d");
        assert_eq!(normalize_path("~/x"), "/tmp/fake-home/x");
    }
}
