//! The doctor collectors: one machine read per advisory section, ported
//! from the deleted `cli/src/fno/doctor.py` at the field-order the frozen
//! goldens pin. Every collector degrades to a named-unknown shape.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use super::super::bounded_command;
use super::{command_call, home_dir, resolve_binary, s, which, Probes};
// The sibling collectors: report re-exports every submodule, so one glob
// covers verdict (report's own) and the surface/dead-letter/advisory fns.
use super::*;

// ---------------------------------------------------------------------------
// The assembled report
// ---------------------------------------------------------------------------

pub(crate) struct Report {
    pub(crate) value: Value,
}

impl Report {
    /// `build_report` (doctor.py:4234): assemble the verdict plus every
    /// advisory section, in the frozen field order.
    pub(crate) fn build(source: Option<&str>) -> Report {
        let probes = Probes::compute(source);
        let src = probes.str_field("source").map(PathBuf::from);
        let source_resolved = src.is_some();
        let source_rev = source_resolved
            .then(|| probes.str_field("source_rev"))
            .flatten();
        let marker = probes.str_field("installed_rev");
        let capture_present = probe_installed_command();
        let rust = rust_report();
        let rust_src_rev = source_resolved
            .then(|| probes.str_field("rust_subtree_rev"))
            .flatten();
        let cargo_bin = cargo_bin_present_flag(&probes);

        let deployed_config_keys = deployed_config_keys();
        let source_config_keys = source_config_keys(src.as_deref());
        let content_drift = python_content_drift(src.as_deref());
        let components = component_convergence(
            src.as_deref(),
            &rust,
            marker.as_deref(),
            content_drift,
            &probes,
        );
        let component_statuses: Vec<(String, String)> = components
            .iter()
            .filter_map(|c| {
                Some((
                    c.get("component")?.as_str()?.to_string(),
                    c.get("status")?.as_str()?.to_string(),
                ))
            })
            .collect();

        let mut head = verdict(
            source_resolved,
            source_rev.as_deref(),
            marker.as_deref(),
            &capture_present,
            rust.get("binary")
                .and_then(Value::as_str)
                .map(str::to_string),
            rust.get("revision")
                .and_then(Value::as_str)
                .map(str::to_string),
            rust_src_rev.clone(),
            cargo_bin,
            deployed_config_keys,
            source_config_keys,
            content_drift,
            &component_statuses,
        );
        if let Value::Object(map) = &mut head {
            map.insert("components".into(), Value::Array(components));
            for (k, v) in mux_front_door_report(&probes) {
                map.insert(k, v);
            }
            map.insert("daemon_drift".into(), daemon_drift_warning());
            map.insert(
                "mux_server_stale".into(),
                Value::Array(probes.or_empty_vec("mux_server_stale")),
            );
            map.insert(
                "running_components".into(),
                Value::Array(probes.or_empty_vec("running_components")),
            );
            map.insert("orphan_files".into(), Value::Array(orphan_report()));
            map.insert("pr_watch".into(), pr_watch_liveness());
            map.insert("control_plane_arms".into(), control_plane_arms_report());
            map.insert("fd_limit".into(), fd_limit_report());
            map.insert("dead_letter".into(), dead_letter_report());
            map.insert("codex_app_server".into(), super::codex_app_server_report());
            map.insert("managed_block".into(), managed_block_report());
            map.insert("harness_surface".into(), harness_surface_report());
            map.insert("plugin_hooks".into(), plugin_hooks_launch_report());
            map.insert("pre_push_hook".into(), pre_push_hook_report());
            map.insert("groom".into(), groom_health());
            map.insert("archive_id_collisions".into(), archive_id_collisions());
            map.insert("post_merge_sync".into(), post_merge_sync_health());
            map.insert("evals".into(), evals_summary());
            map.insert(
                "source_checkout_sync".into(),
                source_checkout_sync(src.as_deref()),
            );
            map.insert("launch_agents".into(), launch_agent_failures());
            map.insert("disk_free".into(), disk_free_report());
            map.insert("git_version".into(), git_version_report());
            let plugin_cache = plugin_cache_report(&probes);
            let armed_manifests = auto_merge_armed_manifests();
            map.insert(
                "silent_switches".into(),
                silent_switch_report(&plugin_cache, &armed_manifests),
            );
            map.insert(
                "auto_merge_review_gap".into(),
                auto_merge_review_gap(&armed_manifests),
            );
            map.insert("merge_gating_optouts".into(), merge_gating_optout_report());
            map.insert("plugin_cache".into(), plugin_cache);
        }
        Report { value: head }
    }
}

// ---------------------------------------------------------------------------
// Verdict-input probes
// ---------------------------------------------------------------------------

fn probe_installed_command() -> String {
    let Some(fno_py) = which("fno-py") else {
        return "unknown".to_string();
    };
    let output = match Command::new(&fno_py)
        .args(["backlog", "capture", "--help"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return "unknown".to_string(),
    };
    if output.status.code() == Some(0) {
        return "present".to_string();
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    )
    .to_lowercase();
    if combined.contains("no such command") {
        return "missing".to_string();
    }
    "unknown".to_string()
}

/// The `_rust_report` shape: which binary `auto` resolves, plus the cargo
/// binary's crates/ rev (the verdict-driving one).
fn rust_report() -> Value {
    let binary = resolve_installed_binary();
    let binary_str = binary.clone();
    let cargo_str = Probes::compute(None).str_field("cargo_bin_path");
    let mut versions: HashMap<String, Value> = HashMap::new();
    let mut version_json = |path: &Option<String>| -> Value {
        match path {
            Some(p) => versions
                .entry(p.clone())
                .or_insert_with(|| binary_version_json(p))
                .clone(),
            None => Value::Null,
        }
    };
    let resolved_ver = version_json(&binary_str);
    let cargo_ver = if cargo_str == binary_str {
        resolved_ver.clone()
    } else {
        version_json(&cargo_str)
    };
    json!({
        "binary": binary_str,
        "revision": clean_rev(cargo_ver.get("crates_rev")),
        "binary_rev": clean_rev(resolved_ver.get("git_rev")),
    })
}

fn resolve_installed_binary() -> Option<String> {
    // The installed set only: sibling of this binary, then PATH. The cargo
    // dev target and $FNO_AGENTS_BIN are deliberately absent, matching the
    // Python resolver's narrower installed-first contract.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("fno-agents");
            if sibling.is_file() {
                return Some(sibling.to_string_lossy().into_owned());
            }
        }
    }
    which("fno-agents")
}

fn binary_version_json(binary: &str) -> Value {
    match Command::new(binary).args(["version", "--json"]).output() {
        Ok(out) if out.status.code() == Some(0) => {
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

fn clean_rev(rev: Option<&Value>) -> Value {
    match rev {
        Some(Value::String(s)) if !s.is_empty() && s != "unknown" => Value::String(s.clone()),
        _ => Value::Null,
    }
}

fn cargo_bin_present_flag(probes: &Probes) -> bool {
    matches!(probes.get("cargo_bin"), Value::Bool(true))
}

/// One `component-verdict` probe against the resolved binary; None when it
/// cannot answer (the caller renders the named gap, never fresh).
fn component_convergence(
    src: Option<&Path>,
    rust: &Value,
    marker: Option<&str>,
    content_drift: Option<i64>,
    probes: &Probes,
) -> Vec<Value> {
    let Some(src) = src else { return Vec::new() };
    let subtree = probes.str_field("rust_subtree_rev");
    let verdict_bin = rust
        .get("binary")
        .and_then(Value::as_str)
        .map(str::to_string);
    let (Some(subtree), Some(verdict_bin)) = (subtree, verdict_bin) else {
        return unknown_components();
    };
    if !Path::new(&verdict_bin).is_file() {
        return unknown_components();
    }
    let cargo_bin = probes.str_field("cargo_bin_path");
    let bindir = match &cargo_bin {
        Some(p) => PathBuf::from(p)
            .parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(&verdict_bin)),
        None => PathBuf::from(&verdict_bin)
            .parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_default(),
    };
    let agents_dir = src
        .parent()
        .unwrap_or(Path::new("/"))
        .join("crates/fno-agents");
    let mut argv: Vec<String> = vec![
        verdict_bin.clone(),
        "component-verdict".into(),
        "--bindir".into(),
        bindir.to_string_lossy().into_owned(),
        "--expected".into(),
        subtree,
        "--agents-dir".into(),
        agents_dir.to_string_lossy().into_owned(),
        "--include-mux".into(),
    ];
    if let Some(rev) = marker {
        argv.extend(["--python-rev".into(), rev.to_string()]);
    }
    if let Some(expected) = probes.str_field("source_rev") {
        argv.extend(["--python-expected".into(), expected]);
    }
    if let Some(drift) = content_drift.filter(|d| *d != 0) {
        argv.extend([
            "--python-evidence".into(),
            format!("{drift} .py file(s) on disk differ from source"),
        ]);
    }
    if marker.is_none() {
        argv.extend([
            "--python-error".into(),
            "the installed-rev marker could not be read".into(),
        ]);
    }
    let output = match Command::new(&argv[0]).args(&argv[1..]).output() {
        Ok(o) if o.status.code() == Some(0) => o,
        _ => return unknown_components(),
    };
    let parsed: Value = match serde_json::from_str(&String::from_utf8_lossy(&output.stdout)) {
        Ok(v) => v,
        Err(_) => return unknown_components(),
    };
    match parsed.get("components").and_then(Value::as_array) {
        Some(rows) if !rows.is_empty() || parsed.get("components").is_some() => rows.clone(),
        _ => unknown_components(),
    }
}

fn unknown_components() -> Vec<Value> {
    ["python-tool", "fno", "fno-agents", "fno-agents-daemon", "fno-agents-worker"]
        .iter()
        .map(|c| {
            json!({
                "component": c,
                "status": "unknown",
                "line": format!("component {c}: unknown (the deployed fno-agents could not answer the convergence verdict)"),
            })
        })
        .collect()
}

/// The deployed CLI's FIELD_META keyset, read through the installed Python
/// (this is the one diagnostic crossing that keeps the schema fingerprint
/// honest; recorded in rust-python-seam.md).
fn deployed_config_keys() -> Option<Vec<String>> {
    let fno_py = which("fno-py")?;
    let output = Command::new(&fno_py)
        .args([
            "-c",
            "import json; from fno.config.registry import FIELD_META; print(json.dumps(sorted(FIELD_META)))",
        ])
        .output()
        .ok()?;
    if output.status.code() != Some(0) {
        return None;
    }
    let keys: Vec<String> =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).ok()?;
    Some(keys)
}

/// The source checkout's FIELD_META keys, parsed from the committed
/// registry.py (`git show HEAD:...`), without importing it.
fn source_config_keys(source: Option<&Path>) -> Option<Vec<String>> {
    let source = source?;
    let output = Command::new("git")
        .args([
            "-C",
            &source.to_string_lossy(),
            "show",
            "HEAD:./src/fno/config/registry.py",
        ])
        .output()
        .ok()?;
    if output.status.code() != Some(0) || output.stdout.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    parse_field_meta_keys(&text)
}

/// Extract FIELD_META's string keys from registry.py source: find the
/// assignment, then collect the top-level string keys of its dict literal.
/// None (skip) when the shape is not a flat literal of constant keys.
fn parse_field_meta_keys(text: &str) -> Option<Vec<String>> {
    let mut keys: BTreeSet<String> = BTreeSet::new();
    let mut assigned = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let is_assign = trimmed.starts_with("FIELD_META")
            && (trimmed["FIELD_META".len()..].trim_start().starts_with('=')
                || trimmed.contains("FIELD_META:"));
        if !is_assign {
            continue;
        }
        let Some(brace) = line.find('{') else {
            continue;
        };
        let mut depth = 0i32;
        let mut current = String::new();
        let mut in_string = false;
        let mut escaped = false;
        for ch in line[brace..].chars() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                current.push(ch);
                continue;
            }
            match ch {
                '"' => {
                    in_string = true;
                    current.push(ch);
                }
                '{' | '(' | '[' => {
                    depth += 1;
                    current.clear();
                }
                '}' | ')' | ']' => {
                    depth -= 1;
                    if depth == 0 && !current.is_empty() {
                        keys.insert(current.trim_matches('"').to_string());
                        current.clear();
                    }
                    current.clear();
                    if depth == 0 && ch == '}' {
                        assigned = true;
                        break;
                    }
                }
                ',' if depth == 1 => {
                    let key = current.trim().trim_matches('"').to_string();
                    if !key.is_empty() {
                        keys.insert(key);
                    }
                    current.clear();
                }
                ':' => current.clear(),
                c if !c.is_whitespace() => current.push(c),
                _ => {}
            }
        }
        if assigned {
            break;
        }
    }
    if !assigned {
        return None;
    }
    Some(keys.into_iter().collect())
}

fn installed_pkg_dir() -> Option<PathBuf> {
    // The installed package dir, through the deployed interpreter (the one
    // diagnostic crossing; see rust-python-seam.md).
    let fno_py = which("fno-py")?;
    let output = Command::new(&fno_py)
        .args([
            "-c",
            "import fno, pathlib; print(pathlib.Path(fno.__file__).parent)",
        ])
        .output()
        .ok()?;
    if output.status.code() != Some(0) {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

fn python_content_drift(source: Option<&Path>) -> Option<i64> {
    let source = source?;
    let inst = installed_pkg_dir()?;
    let inst_fp = pkg_py_fingerprint(&inst)?;
    let src_fp = pkg_py_fingerprint(&source.join("src/fno"))?;
    let mut differing = 0i64;
    let all: BTreeSet<&String> = inst_fp.keys().chain(src_fp.keys()).collect();
    for key in all {
        if inst_fp.get(key) != src_fp.get(key) {
            differing += 1;
        }
    }
    Some(differing)
}

fn pkg_py_fingerprint(dir: &Path) -> Option<BTreeMap<String, String>> {
    if !dir.is_dir() {
        return None;
    }
    let mut map = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "py").unwrap_or(false) {
                let bytes = std::fs::read(&path).ok()?;
                let rel = path.strip_prefix(dir).ok()?;
                let rel = rel.to_string_lossy().replace('\\', "/");
                map.insert(rel, sha256_hex(&bytes));
            }
        }
    }
    Some(map)
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// Advisory collectors
// ---------------------------------------------------------------------------

fn daemon_drift_warning() -> Value {
    let binary = if std::env::var("FNO_AGENTS_RUNTIME")
        .map(|v| v == "rust")
        .unwrap_or(false)
    {
        resolve_binary()
    } else {
        resolve_installed_binary()
    };
    let Some(binary) = binary else {
        return Value::Null;
    };
    let Ok(out) = Command::new(&binary).args(["status", "--json"]).output() else {
        return Value::Null;
    };
    if out.status.code() != Some(0) {
        return Value::Null;
    }
    let payload: Value = match serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()) {
        Ok(v) => v,
        Err(_) => return Value::Null,
    };
    if payload.get("drift").and_then(Value::as_str) != Some("drifted") {
        return Value::Null;
    }
    let stderr_text = String::from_utf8_lossy(&out.stderr);
    let warning = stderr_text.lines().map(str::trim).find(|line| {
        line.starts_with("fno agents: ")
            && line.contains(" is an older build than the installed binary; ")
            && line.split("; ").count() == 2
    });
    let Some(warning) = warning else {
        return Value::Null;
    };
    let uptime = payload
        .get("daemon")
        .and_then(|d| d.get("uptime_secs"))
        .and_then(Value::as_i64);
    match uptime {
        Some(secs) if (0..(10 * 365 * 24 * 3600)).contains(&secs) => Value::String(format!(
            "{warning} (daemon up {}; running its startup build, not this one)",
            fmt_age(secs)
        )),
        _ => Value::String(warning.to_string()),
    }
}

pub(crate) fn fmt_age(seconds: i64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3600 {
        return format!("{}m", seconds / 60);
    }
    format!("{}h", seconds / 3600)
}

fn mux_front_door_report(probes: &Probes) -> Vec<(String, Value)> {
    let mux = probes.str_field("cargo_mux_path");
    let path_fno = which("fno");
    let path_is_mux = path_fno.as_ref().is_some_and(|p| match (&mux, p) {
        (Some(m), _) => same_file(Path::new(p), Path::new(m)),
        (None, _) => probe_is_mux(p),
    });
    let state = if path_is_mux {
        "active"
    } else if mux.is_some() {
        "shadowed"
    } else {
        "not-installed"
    };
    vec![
        (
            "mux_binary".into(),
            s(mux.as_deref().map(Value::from).as_ref()),
        ),
        (
            "path_fno".into(),
            s(path_fno.as_deref().map(Value::from).as_ref()),
        ),
        ("mux_front_door".into(), Value::String(state.into())),
    ]
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn probe_is_mux(fno_path: &str) -> bool {
    Command::new(fno_path)
        .args(["mux", "ls", "--json"])
        .output()
        .map(|o| o.status.code() == Some(0))
        .unwrap_or(false)
}

const ORPHAN_BASENAMES: &[&str] = &[
    "convo-signals.jsonl",
    "tasks.json",
    "tasks.md",
    "metrics.jsonl",
];

fn orphan_report() -> Vec<Value> {
    let mut found = BTreeSet::new();
    for base in [
        home_dir(),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    ] {
        for name in ORPHAN_BASENAMES {
            let p = base.join(".fno").join(name);
            if p.exists() {
                found.insert(p.to_string_lossy().into_owned());
            }
        }
    }
    found.into_iter().map(Value::String).collect()
}

fn pr_watch_liveness() -> Value {
    if let Some(binary) = resolve_binary() {
        if let Ok(out) = Command::new(&binary)
            .args(["pr-watch", "status", "--json"])
            .output()
        {
            if let Ok(report) =
                serde_json::from_str::<Value>(String::from_utf8_lossy(&out.stdout).trim())
            {
                if report.get("verdict").is_some() {
                    return report;
                }
            }
        }
    }
    json!({
        "enabled": false,
        "verdict": "unknown",
        "detail": "",
        "fix": null,
        "loaded": false,
        "last_tick": null,
    })
}

fn control_plane_arms_report() -> Value {
    let Some(binary) = resolve_binary() else {
        return json!({"red": [], "unknown_reason": "fno-agents binary not found"});
    };
    let out = match Command::new(&binary).args(["status", "--json"]).output() {
        Ok(o) => o,
        Err(e) => return json!({"red": [], "unknown_reason": format!("read failed: {e}")}),
    };
    let payload: Value = if out.stdout.is_empty() {
        Value::Null
    } else {
        match serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()) {
            Ok(v) => v,
            Err(e) => return json!({"red": [], "unknown_reason": format!("read failed: {e}")}),
        }
    };
    let attention = payload
        .get("arms_attention")
        .and_then(Value::as_array)
        .cloned();
    let Some(attention) = attention else {
        return json!({"red": [], "unknown_reason": "status payload carries no arms_attention"});
    };
    let red: Vec<Value> = attention.into_iter().filter(|a| a.is_object()).collect();
    json!({"red": red, "unknown_reason": null})
}

fn fd_limit_report() -> Value {
    #[repr(C)]
    struct RLimit {
        soft: u64,
        hard: u64,
    }
    extern "C" {
        fn getrlimit(resource: i32, rlim: *mut RLimit) -> i32;
    }
    // RLIMIT_NOFILE is resource 8 on macOS and 7 on Linux.
    #[cfg(target_os = "macos")]
    const RLIMIT_NOFILE: i32 = 8;
    #[cfg(not(target_os = "macos"))]
    const RLIMIT_NOFILE: i32 = 7;
    const RLIM_INFINITY: u64 = u64::MAX;
    let mut limit = RLimit { soft: 0, hard: 0 };
    // SAFETY: getrlimit on RLIMIT_NOFILE only writes the caller's struct.
    let rc = unsafe { getrlimit(RLIMIT_NOFILE, &mut limit) };
    let soft: u64 = if rc == 0 { limit.soft } else { 0 };
    let hard: u64 = limit.hard;
    let floor: u64 = 1024;
    let mut launchd_soft: Value = Value::Null;
    let mut kern_maxfiles: Value = Value::Null;
    if cfg!(target_os = "macos") {
        if let Some((code, out, _)) =
            bounded_command(&["launchctl".into(), "limit".into(), "maxfiles".into()])
        {
            if code == 0 {
                launchd_soft = parse_launchctl_maxfiles(&out);
            }
        }
        if let Some((code, out, _)) =
            bounded_command(&["sysctl".into(), "-n".into(), "kern.maxfiles".into()])
        {
            if code == 0 {
                if let Ok(n) = out.trim().parse::<u64>() {
                    kern_maxfiles = Value::from(n);
                }
            }
        }
    }
    let as_u64 = |v: &Value| v.as_u64();
    let healthy = RLIM_INFINITY;
    let mut low = false;
    if soft != healthy && soft <= floor {
        low = true;
    }
    if let Some(l) = as_u64(&launchd_soft) {
        if l != healthy && l <= floor {
            low = true;
        }
    }
    json!({
        "soft": soft,
        "hard": if hard == RLIM_INFINITY { Value::String("unlimited".into()) } else { Value::from(hard) },
        "threshold": floor,
        "launchd_soft": launchd_soft,
        "kern_maxfiles": kern_maxfiles,
        "verdict": if low { "low" } else { "ok" },
    })
}

fn parse_launchctl_maxfiles(stdout: &str) -> Value {
    for line in stdout.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 2 && fields[0] == "maxfiles" {
            return fields[1]
                .parse::<u64>()
                .map(Value::from)
                .unwrap_or(Value::Null);
        }
    }
    Value::Null
}

fn groom_command(flag: &str) -> Result<(i32, Value), String> {
    let Some(binary) = resolve_binary() else {
        return Err("fno-agents binary not found".into());
    };
    let mut argv = vec![binary, "backlog".into(), "groom".into(), flag.to_string()];
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| e.to_string())?;
    argv.clear();
    let parsed: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .map_err(|e| e.to_string())?;
    Ok((output.status.code().unwrap_or(-1), parsed))
}

fn groom_health() -> Value {
    match groom_command("--check") {
        Ok((code, probe)) => {
            let plist = home_dir()
                .join("Library")
                .join("LaunchAgents")
                .join("sh.fno.groom.plist");
            json!({
                "state": probe.get("state").cloned().unwrap_or(Value::Null),
                "hours": probe.get("hours").cloned().unwrap_or(Value::Null),
                "stale": code == 0,
                "agent_installed": plist.exists(),
            })
        }
        Err(_) => {
            json!({"state": "unknown", "hours": null, "stale": false, "agent_installed": false})
        }
    }
}

pub(crate) fn groom_install_agent() -> Value {
    match groom_command("--install-agent") {
        Ok((_, receipt)) => receipt,
        Err(e) => json!({"status": "failed", "detail": e.chars().take(200).collect::<String>()}),
    }
}

/// Working-graph ids straight from the store's nodes table (the same db the
/// backlog reads; a torn store reads as empty, never a false alarm).
fn working_graph_ids() -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let path = super::graph_store_path();
    if !path.exists() {
        return ids;
    }
    if let Ok(conn) =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    {
        if let Ok(mut stmt) = conn.prepare("SELECT id FROM nodes") {
            if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                for row in rows.flatten() {
                    ids.insert(row);
                }
            }
        }
    }
    ids
}

pub(crate) fn state_dir() -> PathBuf {
    if let Some(carrier) = std::env::var_os("FNO_STATE_DIR") {
        let path = PathBuf::from(carrier);
        return expand_tilde(&path);
    }
    // The config's state_dir key, else ~/.fno (the default the Python
    // loader resolves; the pinned runs have no config keys set).
    expand_tilde(&PathBuf::from("~/.fno"))
}

/// `fno.paths.project_events_json()`, minus the one-shot checkout migration:
/// the FNO_EVENTS_PATH pin, then the repo space's journal. Every worktree of
/// a repo shares the space, so the canonical root decides the slug.
pub(crate) fn project_events_path() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_EVENTS_PATH") {
        return PathBuf::from(pin);
    }
    space_dir().join("events.jsonl")
}

/// `fno.paths.global_events_json()`: the retired root journal family's
/// anchor and stray-hunt root, a read-only resident the writers abandoned
/// for the routed store. It resolves without the root fence the same way
/// Python's accessor does: the fence refuses new writers, and this retired
/// name is not one.
pub(crate) fn global_events_path() -> PathBuf {
    state_dir().join("events.jsonl")
}

fn space_dir() -> PathBuf {
    spaces_root().join(space_slug(&canonical_repo_root()))
}

fn spaces_root() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_SPACES_DIR") {
        return expand_tilde(&PathBuf::from(pin));
    }
    state_dir().join("spaces")
}

fn space_slug(root: &Path) -> String {
    root.to_string_lossy().replace('/', "-")
}

/// FNO_REPO_ROOT, else the main worktree (git-common-dir's parent), else the
/// checkout root: `fno.paths.resolve_canonical_repo_root`'s order.
fn canonical_repo_root() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT") {
        return PathBuf::from(pin);
    }
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
    {
        if out.status.code() == Some(0) {
            let gitdir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if let Some(parent) = gitdir.parent() {
                if !parent.as_os_str().is_empty() {
                    return parent.to_path_buf();
                }
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn expand_tilde(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

fn archive_id_collisions() -> Value {
    // The canonical resolver reads db/graph-archive.json (graph_json's
    // parent), the layout table's row, never the bare root spelling.
    let state = state_dir();
    let graph = graph_store_path();
    let archive_dir = graph.parent().unwrap_or(state.as_path());
    let archive_path = archive_dir.join("graph-archive.json");
    if !archive_path.exists() {
        return json!({"count": 0, "ids": []});
    }
    let working_ids = working_graph_ids();
    let document: Value = match std::fs::read_to_string(&archive_path)
        .map_err(|_| ())
        .and_then(|t| serde_json::from_str(&t).map_err(|_| ()))
    {
        Ok(v) => v,
        Err(_) => return json!({"count": 0, "ids": [], "unreadable": true}),
    };
    if !document.is_object() {
        return json!({"count": 0, "ids": [], "unreadable": true});
    }
    let archive_ids: BTreeSet<String> = document
        .get("entries")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let collisions: Vec<String> = working_ids.intersection(&archive_ids).cloned().collect();
    json!({"count": collisions.len(), "ids": collisions})
}

fn post_merge_sync_health() -> Value {
    match command_call(
        &["sync-canonical"],
        json!({"action": "staleness", "cwd": std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(), "fetch": true}),
        120,
    ) {
        Ok(st) => json!({
            "state": st.get("state").cloned().unwrap_or(Value::Null),
            "stale": st.get("state").and_then(Value::as_str) == Some("stale"),
            "behind": st.get("behind").cloned().unwrap_or(Value::Null),
            "detail": st.get("detail").cloned().unwrap_or(Value::Null),
        }),
        Err(_) => json!({"state": "unknown", "stale": false, "behind": null, "detail": ""}),
    }
}

fn source_checkout_sync(source: Option<&Path>) -> Value {
    let mut report = json!({
        "status": "unknown",
        "behind": null,
        "source_head": null,
        "remote_head": null,
        "detail": "",
    });
    let Some(source) = source else {
        if let Value::Object(map) = &mut report {
            map.insert(
                "detail".into(),
                Value::String("source checkout not resolved".into()),
            );
        }
        return report;
    };
    let Some(binary) = resolve_binary() else {
        if let Value::Object(map) = &mut report {
            map.insert(
                "detail".into(),
                Value::String(
                    "source-pin helper unavailable (missing or pre-source-pin fno-agents)".into(),
                ),
            );
        }
        return report;
    };
    let out = Command::new(&binary)
        .args(["source-pin", "sync", "--source", &source.to_string_lossy()])
        .output();
    let parsed: Value = match out {
        Ok(o) if o.status.code() == Some(0) => {
            serde_json::from_str(String::from_utf8_lossy(&o.stdout).trim()).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };
    if parsed.is_null() {
        if let Value::Object(map) = &mut report {
            map.insert(
                "detail".into(),
                Value::String(
                    "source-pin helper unavailable (missing or pre-source-pin fno-agents)".into(),
                ),
            );
        }
        return report;
    }
    if let Value::Object(map) = &mut report {
        for key in ["status", "behind", "source_head", "remote_head"] {
            map.insert(key.into(), parsed.get(key).cloned().unwrap_or(Value::Null));
        }
        map.insert(
            "detail".into(),
            parsed
                .get("detail")
                .cloned()
                .unwrap_or(Value::String(String::new())),
        );
    }
    report
}

fn launch_agent_failures() -> Value {
    let Some(binary) = resolve_binary() else {
        return json!({"applicable": false, "dead": []});
    };
    let out = match Command::new(&binary)
        .args(["loops", "table", "--json"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return json!({"applicable": false, "dead": []}),
    };
    if !matches!(out.status.code(), Some(0) | Some(1)) {
        return json!({"applicable": false, "dead": []});
    }
    let payload: Value = match serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()) {
        Ok(v) => v,
        Err(_) => return json!({"applicable": false, "dead": []}),
    };
    let Some(section) = payload.get("launchd").cloned() else {
        return json!({"applicable": false, "dead": []});
    };
    if !section.is_object() {
        return json!({"applicable": false, "dead": []});
    }
    let dead = section
        .get("dead")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "applicable": section.get("applicable").and_then(Value::as_bool).unwrap_or(false),
        "dead": dead,
    })
}

fn disk_free_report() -> Value {
    let target = home_dir().join(".fno");
    #[repr(C)]
    struct StatVfs {
        f_bsize: u64,
        f_frsize: u64,
        f_blocks: u64,
        f_bfree: u64,
        f_bavail: u64,
        f_files: u64,
        f_ffree: u64,
        f_favail: u64,
        f_fsid: u64,
        f_flag: u64,
        f_namemax: u64,
        __pad: [u32; 6],
    }
    extern "C" {
        fn statvfs(path: *const u8, buf: *mut StatVfs) -> i32;
    }
    let mut buf = StatVfs {
        f_bsize: 0,
        f_frsize: 0,
        f_blocks: 0,
        f_bfree: 0,
        f_bavail: 0,
        f_files: 0,
        f_ffree: 0,
        f_favail: 0,
        f_fsid: 0,
        f_flag: 0,
        f_namemax: 0,
        __pad: [0; 6],
    };
    let mut cpath = target.to_string_lossy().into_owned().into_bytes();
    cpath.push(0);
    let free_gb: Option<f64> = unsafe {
        // macOS's statvfs differs in layout; both platforms ship the same
        // symbol and the fields this reads (f_frsize, f_bavail) are stable.
        if statvfs(cpath.as_ptr(), &mut buf) == 0 && buf.f_frsize > 0 {
            Some(buf.f_bavail as f64 * buf.f_frsize as f64 / 1e9)
        } else {
            None
        }
    };
    let Some(free_gb) = free_gb else {
        return json!({"verdict": "unreadable"});
    };
    let rounded = (free_gb * 10.0).round() / 10.0;
    json!({
        "free_gb": rounded,
        "floor_gb": 20,
        "verdict": if rounded < 20.0 { "low" } else { "ok" },
    })
}

fn git_version_report() -> Value {
    let Some((code, out, _)) = bounded_command(&["git".into(), "--version".into()]) else {
        return json!({"verdict": "unreadable"});
    };
    if code != 0 {
        return json!({"verdict": "unreadable"});
    }
    let found = out
        .split_whitespace()
        .skip_while(|w| *w != "version")
        .nth(1)
        .and_then(|v| v.split_once('.'))
        .and_then(|(maj, rest)| {
            let minor: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            maj.parse::<i64>()
                .ok()
                .map(|m| (m, minor.parse::<i64>().unwrap_or(0)))
        });
    let Some((major, minor)) = found else {
        return json!({"verdict": "unreadable"});
    };
    json!({
        "version": format!("{major}.{minor}"),
        "verdict": if (major, minor) < (2, 31) { "old" } else { "ok" },
    })
}

pub(crate) fn preamble_budget_line(source: Option<&str>) -> Option<String> {
    let source_root = source.map(PathBuf::from).or_else(|| None)?;
    let cwd = std::env::current_dir().ok()?;
    let source_id = git_checkout_identity(&source_root)?;
    let current = git_checkout_identity(&cwd)?;
    if source_id.1 != current.1 {
        return None;
    }
    let gate = current.0.join("scripts/ci/check-preamble-budget.sh");
    if !gate.is_file() {
        return None;
    }
    let result = bounded_command(&[
        "bash".into(),
        gate.to_string_lossy().into_owned(),
        "--quiet".into(),
        current.0.to_string_lossy().into_owned(),
    ])?;
    match result {
        (code, out, _) if code == 0 || out.lines().any(|l| l.starts_with("preamble:")) => out
            .lines()
            .find(|l| l.starts_with("preamble:"))
            .map(str::to_string)
            .or_else(|| Some("preamble: unavailable (check emitted no report)".into())),
        (code, _, err) => {
            let detail = err
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            if !detail.is_empty() {
                Some(format!(
                    "preamble: unavailable ({})",
                    &detail.chars().take(160).collect::<String>()
                ))
            } else {
                Some(format!("preamble: unavailable (check exited {code})"))
            }
        }
    }
}

fn git_checkout_identity(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let result = bounded_command(&[
        "git".into(),
        "-C".into(),
        path.to_string_lossy().into_owned(),
        "rev-parse".into(),
        "--show-toplevel".into(),
    ])?;
    if result.0 != 0 {
        return None;
    }
    let lines: Vec<&str> = result.1.lines().collect();
    if lines.len() != 1 {
        return None;
    }
    let common = git_common_dir(path)?;
    Some((
        PathBuf::from(lines[0]).canonicalize().ok()?,
        PathBuf::from(common).canonicalize().ok()?,
    ))
}

fn git_common_dir(path: &Path) -> Option<String> {
    let out = Command::new("git")
        .args([
            "-C",
            &path.to_string_lossy(),
            "rev-parse",
            "--git-common-dir",
        ])
        .output()
        .ok()?;
    if out.status.code() != Some(0) {
        return None;
    }
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if dir.is_empty() {
        return None;
    }
    let dir_path = PathBuf::from(&dir);
    if dir_path.is_absolute() {
        Some(dir)
    } else {
        Some(path.join(dir_path).to_string_lossy().into_owned())
    }
}

pub(crate) fn session_start_bytes_line(preamble_line: &str) -> Option<String> {
    let re = regex::Regex::new(r"preamble:\s*(\d+)\s*/").ok()?;
    let captures = re.captures(preamble_line)?;
    let gate_bytes = captures.get(1)?.as_str().to_string();
    let binary = resolve_binary()?;
    let out = Command::new(&binary)
        .args(["session-start-bytes", "--gate-bytes", &gate_bytes])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    line.starts_with("preamble:").then_some(line)
}
