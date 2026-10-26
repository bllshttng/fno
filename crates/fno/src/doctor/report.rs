//! The doctor report: `build_report` and its collectors, the verdict, and
//! the blockers, ported from the deleted `cli/src/fno/doctor.py`. Field
//! order in the JSON follows the Python dict's insertion order (the frozen
//! goldens pin it); every collector degrades to a named-unknown shape, never
//! to a green.

mod advisors;
mod bus;
mod codex;
mod collectors;
mod dl_bus;
mod dl_hooks;
mod dl_report;
mod hooks_probe;
mod ops;
mod surface;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};

use super::{bounded_command, echo_stderr};

pub(crate) use codex::*;
pub(crate) use collectors::*;

// ---------------------------------------------------------------------------
// Transports
// ---------------------------------------------------------------------------

pub(crate) fn which(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub(crate) fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home_dir().join(".codex"),
    }
}

/// The native front door: PATH first, then the cargo install.
pub(crate) fn front_door() -> Option<String> {
    if let Some(found) = which("fno") {
        return Some(found);
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".cargo"));
    let candidate = cargo_home.join("bin").join("fno");
    candidate
        .is_file()
        .then(|| candidate.to_string_lossy().into_owned())
}

/// Resolve `fno-agents` in the `resolve_binary` order (env, sibling of this
/// binary, PATH, FRONT env, cargo-dev target; the bundled-wheel leg has no
/// native analog). Newest runnable candidate wins inside a finder group.
pub(crate) fn resolve_binary() -> Option<String> {
    fn runnable(path: &Path) -> bool {
        path.is_file()
            && std::fs::metadata(path)
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o111 != 0
                })
                .unwrap_or(false)
    }
    // env override (FNO_AGENTS_BIN), empty or non-executable falls through.
    if let Some(raw) = std::env::var_os("FNO_AGENTS_BIN") {
        let raw = raw.to_string_lossy().into_owned();
        if !raw.trim().is_empty() {
            let candidate = shellexpand_home(&raw);
            if runnable(&candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    // sibling of the running binary (the co-installed cargo triad).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("fno-agents");
            if runnable(&sibling) {
                return Some(sibling.to_string_lossy().into_owned());
            }
        }
    }
    if let Some(found) = which("fno-agents") {
        return Some(found);
    }
    // FNO_AGENTS_FRONT (the smoke lanes' export), behind PATH.
    if let Some(raw) = std::env::var_os("FNO_AGENTS_FRONT") {
        let raw = raw.to_string_lossy().into_owned();
        if !raw.trim().is_empty() {
            let candidate = shellexpand_home(&raw);
            if runnable(&candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    // cargo dev target under the nearest repo ancestor (newest wins).
    let cwd = std::env::current_dir().ok()?;
    let mut root = cwd.as_path();
    loop {
        let marker = root.join("Cargo.toml");
        let crates = root.join("crates");
        if marker.is_file() || (crates.is_dir() && root.join("crates/fno-agents").is_dir()) {
            let candidates = [
                root.join("crates/fno-agents/target/release/fno-agents"),
                root.join("target/release/fno-agents"),
                root.join("crates/fno-agents/target/debug/fno-agents"),
                root.join("target/debug/fno-agents"),
            ];
            let best = candidates.iter().filter(|c| runnable(c)).max_by_key(|c| {
                std::fs::metadata(c)
                    .and_then(|m| m.modified())
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis())
                            .unwrap_or(0)
                    })
                    .unwrap_or(0)
            });
            if let Some(found) = best {
                return Some(found.to_string_lossy().into_owned());
            }
        }
        root = root.parent()?;
    }
}

fn shellexpand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(raw)
}

/// One `fno-agents <command>` round-trip: JSON payload in, parsed JSON out
/// (the `command_call` shape). Dev binary resolution: a dev build outranks
/// an installed copy.
pub(crate) fn command_call(
    command: &[&str],
    payload: Value,
    timeout_secs: u64,
) -> Result<Value, String> {
    let Some(binary) = resolve_binary() else {
        return Err("the fno-agents binary was not found; reinstall fno, run `fno doctor update --rust`, or set FNO_AGENTS_BIN".to_string());
    };
    let mut argv: Vec<String> = vec![binary];
    argv.extend(command.iter().map(|s| s.to_string()));
    let input = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
    let mut child = match Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Err(format!("fno-agents {} failed: {e}", argv[1..].join(" "))),
    };
    use std::io::Write as _;
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!(
                    "fno-agents {} failed: timed out after {timeout_secs}s",
                    argv[1..].join(" ")
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => break Err(format!("fno-agents {} failed: {e}", argv[1..].join(" "))),
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        use std::io::Read;
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        use std::io::Read;
        let _ = err.read_to_string(&mut stderr);
    }
    let status = status?;
    if !status.success() {
        let detail = stderr.trim().chars().take(200).collect::<String>();
        return Err(format!(
            "fno-agents {} exited {}: {detail}",
            argv[1..].join(" "),
            status.code().unwrap_or(-1)
        ));
    }
    serde_json::from_str(&stdout)
        .map_err(|e| format!("fno-agents {} bad output: {e}", argv[1..].join(" ")))
}

/// One `fno-agents <command> <args...>` exec capturing stdout, with the
/// exit code returned beside it (`call_binary_json`'s shape for transports
/// that answer non-JSON or nonzero).
pub(crate) fn call_binary_json(
    command: &str,
    args: &[&str],
    timeout_secs: u64,
) -> (Option<String>, Value) {
    let Some(binary) = resolve_binary() else {
        return (
            Some("the fno-agents binary was not found".to_string()),
            Value::Null,
        );
    };
    let mut argv: Vec<String> = vec![binary, command.to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    let mut child = match Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return (Some(format!("{command} failed: {e}")), Value::Null),
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return (Some(format!("{command} timed out")), Value::Null);
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return (Some(format!("{command} failed to run")), Value::Null),
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        use std::io::Read;
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        use std::io::Read;
        let _ = err.read_to_string(&mut stderr);
    }
    if !status.success() {
        return (
            Some(format!(
                "{command} exited {}: {}",
                status.code().unwrap_or(-1),
                stderr.trim()
            )),
            Value::Null,
        );
    }
    match serde_json::from_str(&stdout) {
        Ok(v) => (None, v),
        Err(_) => (
            Some(format!("{command} emitted unparseable output")),
            Value::Null,
        ),
    }
}

/// Read a JSON file as an object.
pub(crate) fn read_json_object(path: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The probes feed (`fno doctor update --check` payload, read in process)
// ---------------------------------------------------------------------------

/// The memoized readiness payload the collectors read (the PR 2943 shape).
pub(crate) struct Probes(Value);

impl Probes {
    pub(crate) fn compute(source: Option<&str>) -> Probes {
        let source = source.map(PathBuf::from);
        Probes(crate::doctor_update::update_readiness(source.as_deref()))
    }
    pub(crate) fn get(&self, key: &str) -> Value {
        self.0
            .get("probes")
            .and_then(|p| p.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    }
    fn or_empty(&self, key: &str) -> Vec<Value> {
        match self.get(key) {
            Value::Array(items) => items,
            _ => Vec::new(),
        }
    }
    pub(crate) fn or_empty_vec(&self, key: &str) -> Vec<Value> {
        self.or_empty(key)
    }
    pub(crate) fn str_field(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Value::String(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
    fn bool_field(&self, key: &str) -> bool {
        matches!(self.get(key), Value::Bool(true))
    }
}

// ---------------------------------------------------------------------------
// plugin-file
// ---------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn plugin_file_report(active_raw: &str) -> Value {
    let active_path = PathBuf::from(active_raw);
    let active_path = match active_path.expand_home().canonicalize() {
        Ok(p) => p,
        // A nonexistent path stays literal (Python resolve() never raises).
        Err(_) => active_path.expand_home(),
    };
    let parts: Vec<String> = active_path
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect();
    let relative_path: Option<String> = if parts.len() >= 3
        && parts[parts.len() - 3] == "skills"
        && !parts[parts.len() - 2].is_empty()
        && parts[parts.len() - 1] == "SKILL.md"
    {
        Some(
            parts[parts.len() - 3..]
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("/"),
        )
    } else {
        None
    };
    let unknown = |detail: &str| -> Value {
        json!({
            "record": "PLUGIN_FILE_UNKNOWN",
            "status": "unknown",
            "active_path": active_path.to_string_lossy(),
            "relative_path": relative_path.clone(),
            "detail": detail,
        })
    };
    let Some(relative) = relative_path.clone() else {
        return unknown("active path must end in skills/<name>/SKILL.md");
    };
    let Some(source) = resolve_source(None) else {
        return unknown("source checkout could not be resolved");
    };
    let source_path = source
        .join(&relative)
        .is_file()
        .then(|| source.join(&relative))
        .unwrap_or_else(|| source.parent().unwrap_or(&source).join(&relative));
    let (active_bytes, source_bytes) =
        match (std::fs::read(&active_path), std::fs::read(&source_path)) {
            (Ok(a), Ok(s)) => (a, s),
            (Err(e), _) | (_, Err(e)) => {
                let mut rep = unknown(&format!("could not read active/source bytes: {e}"));
                if let Value::Object(map) = &mut rep {
                    map.insert(
                        "source_path".into(),
                        Value::String(source_path.to_string_lossy().into_owned()),
                    );
                    // keep the field order: source_path after relative_path, detail last
                    if let Some(detail) = map.remove("detail") {
                        map.insert("detail".into(), detail);
                    }
                }
                return rep;
            }
        };
    let active_digest = sha256_hex(&active_bytes);
    let source_digest = sha256_hex(&source_bytes);
    let stale = active_digest != source_digest;
    let mut rep = json!({
        "record": if stale { "PLUGIN_FILE_STALE" } else { "PLUGIN_FILE_FRESH" },
        "status": if stale { "stale" } else { "fresh" },
        "active_path": active_path.to_string_lossy(),
        "relative_path": relative_path,
        "source_path": source_path.to_string_lossy(),
        "active_digest": active_digest,
        "source_digest": source_digest,
    });
    if stale && is_claude_plugin_cache(&active_path) {
        // A cache under the pre-rename "footnote" marketplace updates by its old id.
        let posix = active_path.to_string_lossy();
        let plugin_id = if posix.contains("/.claude/plugins/cache/footnote/") {
            "fno@footnote"
        } else {
            "fno@fno"
        };
        if let Value::Object(map) = &mut rep {
            map.insert(
                "detail".into(),
                Value::String(format!(
                    "active Claude plugin bytes are stale; run `claude plugin update {plugin_id}`, then restart the session; `fno doctor update` does NOT refresh this artifact"
                )),
            );
        }
    }
    rep
}

fn is_claude_plugin_cache(path: &Path) -> bool {
    let parts: Vec<String> = path
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect();
    if parts.len() < 3 {
        return false;
    }
    (0..=parts.len() - 3).any(|i| parts[i..i + 3] == [".claude", "plugins", "cache"])
}

pub(crate) fn plugin_file_text(rep: &Value) -> String {
    let record = rep.get("record").and_then(Value::as_str).unwrap_or("");
    let relative = rep
        .get("relative_path")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut fields = vec![record.to_string(), format!("path={relative}")];
    if let (Some(a), Some(s)) = (
        rep.get("active_digest").and_then(Value::as_str),
        rep.get("source_digest").and_then(Value::as_str),
    ) {
        fields.push(format!("active_sha256={a}"));
        fields.push(format!("source_sha256={s}"));
    }
    if let Some(detail) = rep.get("detail").and_then(Value::as_str) {
        fields.push(detail.to_string());
    }
    fields.join(" ")
}

// ---------------------------------------------------------------------------
// Source resolution + the verdict
// ---------------------------------------------------------------------------

pub(crate) fn resolve_source(source: Option<&str>) -> Option<PathBuf> {
    let explicit = source.map(PathBuf::from);
    let probes = Probes::compute(source);
    let probe = probes.str_field("source");
    match (explicit, probe) {
        (_, Some(p)) => Some(PathBuf::from(p)),
        (Some(e), None) => Some(e),
        (None, None) => None,
    }
}

pub(crate) fn cargo_bin_present() -> bool {
    Probes::compute(None).bool_field("cargo_bin")
}

/// The `_verdict` decision matrix (doctor.py:1921-2041), pure over its
/// inputs. Returns the insertion-ordered head of the report dict.
#[allow(clippy::too_many_arguments)]
fn verdict(
    source_resolved: bool,
    source_rev: Option<&str>,
    marker: Option<&str>,
    capture_present: &str,
    rust_binary: Option<String>,
    rust_installed_rev: Option<String>,
    rust_source_rev: Option<String>,
    cargo_bin_present: bool,
    deployed_config_keys: Option<Vec<String>>,
    source_config_keys: Option<Vec<String>>,
    content_drift: Option<i64>,
    component_statuses: &[(String, String)],
) -> Value {
    let mut missing_commands: Vec<String> = Vec::new();
    let mut python_stale = false;
    let mut status = "unknown".to_string();

    if capture_present == "missing" {
        python_stale = true;
        missing_commands.push("backlog capture".to_string());
        status = "stale".to_string();
    } else if source_resolved && source_rev.is_some() && marker.is_some() {
        if marker == source_rev {
            status = "fresh".to_string();
        } else {
            python_stale = true;
            status = "stale".to_string();
        }
    }

    let mut missing_config_keys: Vec<String> = Vec::new();
    if let (Some(deployed), Some(source_keys)) = (&deployed_config_keys, &source_config_keys) {
        missing_config_keys = source_keys
            .iter()
            .filter(|k| !deployed.contains(*k))
            .cloned()
            .collect();
        missing_config_keys.sort();
    }
    if !missing_config_keys.is_empty() {
        python_stale = true;
        status = "stale".to_string();
    }

    let content_stale = content_drift.map(|n| n > 0).unwrap_or(false);
    if content_stale {
        python_stale = true;
        status = "stale".to_string();
    }
    let content_indeterminate = content_drift.is_none();
    if content_indeterminate && status == "fresh" {
        status = "unknown".to_string();
    }

    let rust_stale = (cargo_bin_present
        && rust_installed_rev.is_some()
        && rust_source_rev.is_some()
        && rust_installed_rev != rust_source_rev)
        || component_statuses.iter().any(|(name, word)| {
            name.starts_with("fno-agents")
                && matches!(word.as_str(), "stale" | "missing" | "failed")
        });
    if rust_stale && status != "stale" {
        status = "stale".to_string();
    }

    let opt_str = |v: &Option<String>| match v {
        Some(s) => Value::String(s.clone()),
        None => Value::Null,
    };
    json!({
        "status": status,
        "python_stale": python_stale,
        "rust_stale": rust_stale,
        "content_stale": content_stale,
        "content_drift_count": content_drift.map(Value::from).unwrap_or(Value::Null),
        "content_indeterminate": content_indeterminate,
        "missing_commands": missing_commands,
        "missing_config_keys": missing_config_keys,
        "source_rev": opt_str(&source_rev.map(str::to_string)),
        "installed_rev": opt_str(&marker.map(str::to_string)),
        "rust_binary": opt_str(&rust_binary),
        "rust_installed_rev": opt_str(&rust_installed_rev),
        "rust_source_rev": opt_str(&rust_source_rev),
        "component_statuses": component_statuses.iter().map(|(name, word)| json!({
            "component": name,
            "status": word,
        })).collect::<Vec<_>>(),
    })
}

/// `s(v)`: null-safe string field for a collector Value.
pub(crate) fn s(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Value::String(s.clone()),
        _ => Value::Null,
    }
}

// ---------------------------------------------------------------------------
// The blockers
// ---------------------------------------------------------------------------

pub(crate) struct Blockers(pub Vec<String>);

impl Blockers {
    pub(crate) fn of(rep: &Value) -> Blockers {
        let mut blockers: Vec<String> = Vec::new();
        if rep.get("status").and_then(Value::as_str) == Some("stale") {
            blockers.push(
                "installed fno is behind source; hooks run pre-HEAD bytes. Fix: fno doctor --fix"
                    .to_string(),
            );
        }
        let sync = rep
            .get("source_checkout_sync")
            .cloned()
            .unwrap_or(Value::Null);
        if sync.get("status").and_then(Value::as_str) == Some("behind") {
            let behind = sync.get("behind").and_then(Value::as_i64).unwrap_or(0);
            let unit = if behind == 1 { "commit" } else { "commits" };
            blockers.push(format!(
                "resolved source checkout is {behind} {unit} behind origin/main; freshness is relative to stale local source. Sync the checkout before trusting verification."
            ));
        }
        for agent in rep
            .get("launch_agents")
            .and_then(|a| a.get("dead"))
            .and_then(Value::as_array)
            .map(|a| a.clone())
            .unwrap_or_default()
        {
            let exit_code = agent.get("exit");
            let exit_word = match exit_code {
                Some(Value::Number(n)) => n.to_string(),
                _ => "exit unknown".to_string(),
            };
            blockers.push(format!(
                "LaunchAgent {} last exited {}.",
                agent.get("label").and_then(Value::as_str).unwrap_or(""),
                exit_word
            ));
        }
        let fd = rep.get("fd_limit").cloned().unwrap_or(Value::Null);
        if fd.get("verdict").and_then(Value::as_str) == Some("low") {
            blockers.push(format!(
                "open-file limit is low (launchd soft {}); reaches spawned workers only, not this shell.",
                fd.get("launchd_soft").map(|v| v.to_string()).unwrap_or_else(|| "null".into())
            ));
        }
        let disk = rep.get("disk_free").cloned().unwrap_or(Value::Null);
        if disk.get("verdict").and_then(Value::as_str) == Some("low") {
            blockers.push(format!(
                "free disk is {} GB, under the {} GB floor; builds, stores and downloads fail near zero. Fix: fno doctor reclaim --apply",
                disk.get("free_gb").map(|v| v.to_string()).unwrap_or_default(),
                disk.get("floor_gb").map(|v| v.to_string()).unwrap_or_default(),
            ));
        }
        let git = rep.get("git_version").cloned().unwrap_or(Value::Null);
        if git.get("verdict").and_then(Value::as_str) == Some("old") {
            blockers.push(format!(
                "git {} is under 2.31: shell hooks take its echoed --path-format flag as a repo root. Fix: upgrade git.",
                git.get("version").and_then(Value::as_str).unwrap_or("?")
            ));
        }
        let ph = rep.get("plugin_hooks").cloned().unwrap_or(Value::Null);
        if ph.get("failed").and_then(Value::as_i64).unwrap_or(0) > 0 {
            blockers.push(format!(
                "{} plugin hook(s) cannot launch.",
                ph.get("failed").and_then(Value::as_i64).unwrap_or(0)
            ));
        }
        for root in rep
            .get("plugin_cache")
            .and_then(|pc| pc.get("roots"))
            .and_then(Value::as_array)
            .map(|a| a.clone())
            .unwrap_or_default()
        {
            if let Some(blocker) = root.get("blocker").and_then(Value::as_str) {
                blockers.push(blocker.to_string());
            }
        }
        Blockers(blockers)
    }
}

pub(crate) fn emit_blockers(blockers: &[String], err: bool) {
    let mut lines = vec![format!(
        "fno doctor: {} BLOCKER(S) - these will make the fleet misbehave:",
        blockers.len()
    )];
    for (i, line) in blockers.iter().enumerate() {
        lines.push(format!("  {}. {line}", i + 1));
    }
    for line in &lines {
        if err {
            echo_stderr(line);
        } else {
            println!("{line}");
        }
    }
}

/// `Path::expanduser` for the collectors that take a user path.
pub(crate) trait ExpandHome {
    fn expand_home(&self) -> PathBuf;
}

impl ExpandHome for Path {
    fn expand_home(&self) -> PathBuf {
        if let Ok(rest) = self.strip_prefix("~") {
            return home_dir().join(rest);
        }
        self.to_path_buf()
    }
}

impl ExpandHome for PathBuf {
    fn expand_home(&self) -> PathBuf {
        self.as_path().expand_home()
    }
}
