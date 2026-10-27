//! The path diagnostic: resolved paths, suspicious values, and nine reports
//! of one shape (heading, reasons, remedy line). Exit 0 means clean.

use super::{py_repr, py_type_name, Settings};
use serde_json::{Map, Value};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Path prefixes that indicate a misconfigured path, with the reason.
const SUSPICIOUS_PATHS: [(&str, &str); 8] = [
    ("/tmp/", "temp directory; data will not survive reboot"),
    ("/var/tmp/", "temp directory; data will not survive reboot"),
    (
        "/private/tmp/",
        "temp directory; data will not survive reboot",
    ),
    (
        "~/Dropbox/",
        "Dropbox sync; conflicted copies on multi-machine setups",
    ),
    (
        "~/iCloud/",
        "iCloud sync; conflicted copies on multi-machine setups",
    ),
    (
        "~/Library/Mobile Documents/",
        "iCloud sync; conflicted copies on multi-machine setups",
    ),
    (
        "~/OneDrive/",
        "OneDrive sync; conflicted copies on multi-machine setups",
    ),
    (".git/", "git internal; may be cleaned by git gc"),
];

const TEMP_PATTERNS: [&str; 3] = ["/tmp/", "/var/tmp/", "/private/tmp/"];

const VALID_WORKTREE_POLICIES: [&str; 3] = ["never", "harness-native", "external"];

fn test_mode() -> bool {
    std::env::var("FNO_TEST_MODE").as_deref() == Ok("1")
}

/// The `config.toml`-first pair at one settings location.
fn settings_pair(path: &Path) -> Vec<PathBuf> {
    vec![path.with_file_name("config.toml"), path.to_path_buf()]
}

fn edit_value(value: &toml_edit::Value) -> Value {
    match value {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::from(*i.value()),
        toml_edit::Value::Float(f) => serde_json::Number::from_f64(*f.value())
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(d) => Value::String(d.value().to_string()),
        toml_edit::Value::Array(items) => Value::Array(items.iter().map(edit_value).collect()),
        toml_edit::Value::InlineTable(table) => {
            let mut map = Map::new();
            for (key, entry) in table.iter() {
                map.insert(key.to_string(), edit_value(entry));
            }
            Value::Object(map)
        }
    }
}

fn edit_table(table: &toml_edit::Table) -> Value {
    let mut map = Map::new();
    for (key, item) in table.iter() {
        map.insert(key.to_string(), edit_item(item));
    }
    Value::Object(map)
}

fn edit_item(item: &toml_edit::Item) -> Value {
    match item {
        toml_edit::Item::None => Value::Null,
        toml_edit::Item::Value(value) => edit_value(value),
        toml_edit::Item::Table(table) => edit_table(table),
        toml_edit::Item::ArrayOfTables(tables) => {
            Value::Array(tables.iter().map(edit_table).collect())
        }
    }
}

/// One config file in DOCUMENT order: a finding list names keys in the order
/// the operator wrote them, and the `toml` crate's table sorts them. YAML
/// mappings already keep their order.
fn parse_ordered(path: &Path) -> Option<Value> {
    if path.extension().and_then(|e| e.to_str()) != Some("toml") {
        return crate::config_cli::parse_config_file(path);
    }
    let text = std::fs::read_to_string(path).ok()?;
    let doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    Some(edit_table(doc.as_table()))
}

/// The merged tree the runtime resolves from `paths` (highest first), each
/// file read raw: an overridden layer's stale value is no finding.
fn merged_from(paths: &[PathBuf]) -> Value {
    let mut merged = Value::Object(Map::new());
    let mut seen: Vec<PathBuf> = Vec::new();
    for path in paths.iter().rev() {
        if !path.is_file() {
            continue;
        }
        let resolved = super::resolve(path);
        if seen.contains(&resolved) {
            continue;
        }
        seen.push(resolved);
        if let Some(parsed) = parse_ordered(path) {
            let flat = crate::config_cli::unwrap_config_dict(&parsed);
            if flat.is_object() {
                crate::config_cli::deep_merge(&mut merged, &flat);
            }
        }
    }
    merged
}

fn dedup(problems: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for problem in problems {
        if !out.contains(&problem) {
            out.push(problem);
        }
    }
    out
}

/// Malformed `kanban.wip_caps` entries in one flat config tree.
pub(crate) fn wip_cap_problems(data: &Value) -> Vec<String> {
    let Some(kanban) = data.get("kanban").and_then(|k| k.as_object()) else {
        return Vec::new();
    };
    let raw = match kanban.get("wip_caps") {
        None | Some(Value::Null) => return Vec::new(),
        Some(raw) => raw,
    };
    let Some(caps) = raw.as_object() else {
        return vec![format!(
            "config.kanban.wip_caps is {}, not a mapping; all columns left uncapped",
            py_type_name(raw)
        )];
    };
    let mut problems = Vec::new();
    for (key, value) in caps {
        let key_repr = super::py_repr_str(key);
        let value_repr = py_repr(value);
        match value {
            Value::Bool(_) => problems.push(format!(
                "wip_caps[{key_repr}] = {value_repr} is a boolean, not a cap; column left uncapped"
            )),
            Value::Number(n) if n.as_i64().map(|i| i > 0).unwrap_or(false) || n.as_u64().is_some_and(|u| u > 0) => {}
            _ => problems.push(format!(
                "wip_caps[{key_repr}] = {value_repr} is not a positive integer; column left uncapped"
            )),
        }
    }
    problems
}

fn check_wip_caps() -> Vec<String> {
    let paths = settings_pair(&crate::config_cli::global_settings_path());
    dedup(wip_cap_problems(&merged_from(&paths)))
}

/// An out-of-enum `worktree.policy` in one flat config tree.
pub(crate) fn worktree_policy_problems(data: &Value) -> Vec<String> {
    let policy = match data.get("worktree").and_then(|w| w.get("policy")) {
        None | Some(Value::Null) => return Vec::new(),
        Some(policy) => policy,
    };
    if policy
        .as_str()
        .map(|p| VALID_WORKTREE_POLICIES.contains(&p))
        .unwrap_or(false)
    {
        return Vec::new();
    }
    vec![format!(
        "config.worktree.policy = {} is not one of {}; worktree creation will refuse",
        py_repr(policy),
        VALID_WORKTREE_POLICIES.join(" | ")
    )]
}

/// The global config and the invoking repo's own: a per-project override
/// lives in either.
fn check_worktree_policy() -> Vec<String> {
    let mut paths = settings_pair(
        &crate::config_cli::repo_root()
            .join(".fno")
            .join("settings.yaml"),
    );
    paths.extend(settings_pair(&crate::config_cli::global_settings_path()));
    dedup(worktree_policy_problems(&merged_from(&paths)))
}

/// The harness running this shell, for the remedy line only.
fn detected_harness() -> &'static str {
    const MARKERS: [(&str, &str); 9] = [
        ("CODEX_THREAD_ID", "codex"),
        ("CLAUDE_CODE_SESSION_ID", "claude"),
        ("CODEX_SESSION_ID", "codex"),
        ("GEMINI_SESSION_ID", "gemini"),
        ("OPENCODE_SESSION_ID", "opencode"),
        ("CLAUDE_SESSION_ID", "claude"),
        ("CLAUDECODE", "claude"),
        ("CLAUDE_CONFIG_DIR", "claude"),
        ("CODEX_HOME", "codex"),
    ];
    MARKERS
        .iter()
        .find(|(env, _)| std::env::var(env).map(|v| !v.is_empty()).unwrap_or(false))
        .map(|(_, name)| *name)
        .unwrap_or("")
}

fn remedy(harness: &str) -> &'static str {
    match harness {
        "claude" => "add the state root to permissions.additionalDirectories in your ~/.claude/settings.json",
        "codex" => "add the state root to sandbox_workspace_write.writable_roots in your ~/.codex/config.toml, or run this session on a bypass posture",
        "agy" => "grant the state root write access in your agy settings",
        "opencode" => "opencode's --dir SETS the working directory rather than adding one, so launch this session from a root that contains the state directory",
        _ => "grant this session write access to the state root",
    }
}

/// `strerror` for an OS error: the Display text minus Rust's errno suffix.
fn strerror(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// Probe whether THIS session can write the claim store, by writing to it:
/// a positive marker, never an inferred absence. Advises only; it never
/// edits a settings file.
fn check_state_root_writable() -> Vec<String> {
    if test_mode() {
        return Vec::new();
    }
    let store = super::global_claims_dir();
    let probe = || -> std::io::Result<()> {
        std::fs::create_dir_all(&store)?;
        let path = store.join(format!(".doctor-probe-{}", std::process::id()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        std::fs::remove_file(&path)
    };
    match probe() {
        Ok(()) => Vec::new(),
        Err(err) => {
            let harness = detected_harness();
            let who = if harness.is_empty() {
                String::new()
            } else {
                format!(" (detected harness: {harness})")
            };
            vec![format!(
                "the claim store at {} is not writable by this session{who}: {}. A worker here takes no node claim, so `fno agents claim status` reports free while it works and a second worker can be dispatched onto the same node. Remedy: {}.",
                store.to_string_lossy(),
                strerror(&err),
                remedy(harness)
            )]
        }
    }
}

/// A config-sourced substrate must be known and honored by the provider.
pub(crate) fn substrate_compatible(substrate: &str, provider: &str) -> bool {
    let substrate = match substrate {
        "pane" | "headless" => return true,
        "thread" | "bg" => "thread",
        _ => return false,
    };
    let payload =
        serde_json::json!({"kind": "compat", "harness": provider, "substrate": substrate});
    match super::agents_call(&["spawn-overlay"], Some(&payload)) {
        Ok(answer) => answer["substrate"]["compatible"].as_bool().unwrap_or(false),
        Err(_) => provider == "claude",
    }
}

fn stripped(node: &Value, key: &str) -> String {
    node.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Stage lanes that cannot launch with their resolved posture.
fn check_agent_profiles(settings: &Settings) -> Vec<String> {
    let defaults = settings.get("agents.defaults");
    let profiles = super::obj(&settings.get("agents.profiles"));
    let mut problems = Vec::new();
    for (command, profile) in &profiles {
        let lanes = profile
            .get("lanes")
            .and_then(|l| l.as_array())
            .filter(|l| !l.is_empty());
        let targets: Vec<(Value, String)> = match lanes {
            Some(lanes) => lanes
                .iter()
                .enumerate()
                .map(|(i, lane)| {
                    (
                        lane.clone(),
                        format!("agents.profiles.{command}.lanes[{i}]"),
                    )
                })
                .collect(),
            None => vec![(profile.clone(), format!("agents.profiles.{command}"))],
        };
        for (target, path) in targets {
            let pick = |key: &str| {
                [
                    stripped(&target, key),
                    stripped(profile, key),
                    stripped(&defaults, key),
                ]
                .into_iter()
                .find(|v| !v.is_empty())
                .unwrap_or_default()
            };
            let provider = pick("provider");
            let substrate = pick("substrate");
            if !provider.is_empty()
                && !substrate.is_empty()
                && !substrate_compatible(&substrate, &provider)
            {
                problems.push(format!(
                    "{path}.substrate = {} is incompatible with resolved provider {}",
                    super::py_repr_str(&substrate),
                    super::py_repr_str(&provider)
                ));
            }
        }
    }
    problems
}

/// Blocks under accounts/providers that are not tables: the provider loader
/// coerces one to defaults rather than refusing, so nothing else says so.
pub(crate) fn accounts_problems(raw: &Value, label: &str) -> Vec<String> {
    let config = raw
        .get("config")
        .filter(|c| c.is_object())
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let mut problems = Vec::new();
    for block_key in ["accounts", "providers"] {
        for (scope, prefix) in [
            (raw, block_key.to_string()),
            (&config, format!("config.{block_key}")),
        ] {
            let Some(block) = scope.get(block_key).and_then(|b| b.as_object()) else {
                continue;
            };
            for sub in ["quota", "failover"] {
                match block.get(sub) {
                    None | Some(Value::Null) | Some(Value::Object(_)) => {}
                    Some(value) => problems.push(format!(
                        "{label}: {prefix}.{sub} is not a table (got {}); it will be coerced to defaults",
                        py_type_name(value)
                    )),
                }
            }
        }
    }
    problems
}

/// The provider loader's file set: the repo-local config, the PWD-anchored
/// one when it differs, then the global config.toml.
fn provider_candidates() -> Vec<PathBuf> {
    if let Some(pin) = std::env::var("FNO_CONFIG").ok().filter(|p| !p.is_empty()) {
        return vec![PathBuf::from(pin)];
    }
    let pwd = std::env::var("PWD")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let global = match std::env::var("FNO_GLOBAL_SETTINGS_PATH")
        .ok()
        .filter(|p| !p.is_empty())
    {
        Some(path) => PathBuf::from(path).with_file_name("config.toml"),
        None => crate::config_cli::home_dir()
            .join(".fno")
            .join("config.toml"),
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for path in [
        crate::config_cli::repo_root()
            .join(".fno")
            .join("config.toml"),
        pwd.join(".fno").join("config.toml"),
        global,
    ] {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// `_read_parsed`: config.toml then its settings.yaml sibling; the first
/// existing file answers, and an unparseable or non-table one reads empty.
fn read_provider_file(path: &Path) -> Value {
    let candidates = match path.file_name().and_then(|n| n.to_str()) {
        Some("config.toml") => vec![path.to_path_buf(), path.with_file_name("settings.yaml")],
        Some("settings.yaml") => vec![path.with_file_name("config.toml"), path.to_path_buf()],
        _ => vec![path.to_path_buf()],
    };
    for candidate in candidates {
        if candidate.is_file() {
            return crate::config_cli::parse_config_file(&candidate)
                .filter(|v| v.is_object())
                .unwrap_or(Value::Object(Map::new()));
        }
    }
    Value::Object(Map::new())
}

/// The account loader's own refusal, from the native loader that owns it
/// (`fno-agents spawn-overlay accounts ...`). A load failure exits 1 with
/// `error: <cause>`; no binary means no answer, never a finding.
fn loader_refusal(argv: &[&str]) -> Option<String> {
    let binary = crate::digest_overlay::fno_agents_bin_auto()?;
    let output = std::process::Command::new(binary)
        .args(argv)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if output.status.code() != Some(1) {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = stderr.trim();
    Some(text.strip_prefix("error: ").unwrap_or(text).to_string())
}

fn check_accounts() -> Vec<String> {
    let mut problems = Vec::new();
    for (argv, label) in [
        (
            &["spawn-overlay", "accounts", "list", "--json"][..],
            "accounts/providers",
        ),
        (
            &["spawn-overlay", "accounts", "combos", "list", "--json"][..],
            "combos",
        ),
    ] {
        if let Some(cause) = loader_refusal(argv) {
            problems.push(format!("{label}: {cause}"));
        }
    }
    for candidate in provider_candidates() {
        let data = read_provider_file(&candidate);
        if data.as_object().map(|m| !m.is_empty()).unwrap_or(false) {
            problems.extend(accounts_problems(&data, &candidate.to_string_lossy()));
        }
    }
    dedup(problems)
}

/// Settings files the loader could not read back. An empty table is legal.
fn check_config_files_read() -> Vec<String> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut problems = Vec::new();
    for path in crate::config_cli::candidate_paths(&crate::config_cli::repo_root()) {
        if !path.is_file() {
            continue;
        }
        let resolved = super::resolve(&path);
        if seen.contains(&resolved) {
            continue;
        }
        seen.push(resolved);
        let shown = path.to_string_lossy();
        let text = match std::fs::read(&path) {
            Err(err) => {
                problems.push(format!("config file at {shown} could not be read: {err}"));
                continue;
            }
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(err) => {
                    problems.push(format!("config file at {shown} failed to parse: {err}"));
                    continue;
                }
            },
        };
        let parsed: Result<Value, String> =
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                toml::from_str::<toml::Value>(&text)
                    .map(crate::config_cli::toml_to_json)
                    .map_err(|e| e.to_string().lines().next().unwrap_or("").to_string())
            } else {
                serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text)
                    .map(crate::config_cli::yaml_to_json)
                    .map_err(|e| e.to_string())
            };
        match parsed {
            Err(err) => problems.push(format!("config file at {shown} failed to parse: {err}")),
            Ok(Value::Null) | Ok(Value::Object(_)) => {}
            Ok(other) => problems.push(format!(
                "config file at {shown} parsed to a {}, not a table",
                py_type_name(&other)
            )),
        }
    }
    problems
}

/// The loader's per-file layers, path as text.
fn layers() -> Vec<(String, Value)> {
    crate::config_cli::aliased_layers(&crate::config_cli::repo_root())
        .into_iter()
        .map(|(path, value)| (path.to_string_lossy().into_owned(), value))
        .collect()
}

fn check_values(settings: &Settings) -> Vec<String> {
    let schema = settings.json_schema();
    layers()
        .into_iter()
        .filter_map(|(path, parsed)| {
            let refused = super::schema_walk::refusals(
                schema,
                &crate::config_cli::unwrap_config_dict(&parsed),
            );
            if refused.is_empty() {
                None
            } else {
                Some(super::schema_walk::describe(&refused, &[(path, parsed)]))
            }
        })
        .collect()
}

/// A switch on with nothing that can satisfy it: cross-model review with no
/// non-claude reviewer kind dispatchable.
fn check_enabled_with_empty_population(settings: &Settings) -> Vec<String> {
    if !super::truthy(&settings.get("review.cross_model.enabled")) {
        return Vec::new();
    }
    let Ok(row) = super::agents_call(&["review-policy", "kinds"], None) else {
        return Vec::new();
    };
    if !row.is_object() {
        return Vec::new();
    }
    let kinds: Vec<String> = row
        .get("available")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .map(|k| super::py_str(k).trim().to_lowercase())
                .collect()
        })
        .unwrap_or_default();
    if kinds.iter().any(|k| k != "claude") {
        return Vec::new();
    }
    let listed = if kinds.is_empty() {
        "none".to_string()
    } else {
        kinds.join(", ")
    };
    vec![format!(
        "review.cross_model.enabled is true and no non-claude provider is dispatchable. available reviewer kinds: {listed}. The switch buys nothing for a claude-written change. Only where codex or gemini wrote the code is a claude reviewer a different family."
    )]
}

/// Keys whose configured value degraded to the model default: the sweep
/// validators that record rather than raise.
pub(crate) fn degraded(raw: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let get = |dotted: &str| crate::config_cli::raw_get(raw, dotted).filter(|v| !v.is_null());
    if let Some(value) = get("agents.reap.roster_scope") {
        let ok = value
            .as_str()
            .map(|s| {
                matches!(
                    s.trim().to_lowercase().as_str(),
                    "off" | "provenanced" | "all"
                )
            })
            .unwrap_or(false);
        if !ok {
            out.push(("agents.reap.roster_scope".to_string(), py_repr(value)));
        }
    }
    if let Some(value) = get("agents.state_reap.enabled") {
        if !value.is_boolean() {
            out.push(("agents.state_reap.enabled".to_string(), py_repr(value)));
        }
    }
    for name in [
        "locks_retain_days",
        "expired_claims_retain_days",
        "pr_status_cache_retain_days",
    ] {
        if let Some(value) = get(&format!("agents.state_reap.{name}")) {
            if !value.as_i64().map(|i| i > 0).unwrap_or(false) || value.is_boolean() {
                out.push((format!("agents.state_reap.{name}"), py_repr(value)));
            }
        }
    }
    for name in [
        "single_flight_ttl_seconds",
        "single_flight_join_budget_seconds",
        "orphan_reap_after_seconds",
    ] {
        if let Some(value) = get(&format!("agents.{name}")) {
            let seconds = match value {
                Value::Bool(b) => i64::from(*b),
                Value::Number(n) => n
                    .as_i64()
                    .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
                    .unwrap_or(0),
                Value::String(s) => s.trim().parse::<i64>().unwrap_or(0),
                _ => 0,
            };
            if seconds <= 0 {
                out.push((format!("agents.{name}"), py_repr(value)));
            }
        }
    }
    out.sort();
    out
}

/// `run_doctor`: the diagnostic into `out`; 0 clean, 1 on any finding.
pub fn run_doctor(out: &mut String) -> i32 {
    let repo = crate::config_cli::repo_root();
    let candidates = crate::config_cli::candidate_paths(&repo);
    let Some(found) = candidates.iter().find(|c| c.is_file()).cloned() else {
        match std::env::var("FNO_CONFIG").ok().filter(|p| !p.is_empty()) {
            Some(pin) => {
                let _ = writeln!(out, "[doctor] error: settings.yaml not found at {pin}");
            }
            None => out.push_str("[doctor] error: no settings.yaml found\n"),
        }
        out.push_str("[doctor] run 'fno config set <key> <value>' to write one\n");
        return 1;
    };

    let settings = Settings::load();
    let layer_rows = layers();
    let refused = super::schema_walk::refusals(settings.json_schema(), &settings.raw);
    if !refused.is_empty() {
        let _ = writeln!(
            out,
            "[doctor] {}",
            super::schema_walk::describe(&refused, &layer_rows)
        );
        return 1;
    }

    let contributors: Vec<String> = layer_rows.iter().map(|(path, _)| path.clone()).collect();
    let source = if contributors.is_empty() {
        found.to_string_lossy().into_owned()
    } else {
        contributors.join(", ")
    };
    let _ = writeln!(out, "[doctor] settings source: {source}");
    let schema_version = settings.get("schema_version");
    let _ = writeln!(
        out,
        "[doctor] schema_version: {}",
        super::py_str(&schema_version)
    );
    for (key, raw) in degraded(&settings.raw) {
        let _ = writeln!(
            out,
            "[doctor] {key}: bad value {raw} ignored; using the default"
        );
    }
    let _ = writeln!(
        out,
        "[doctor] state_dir: {}",
        super::state_dir(&settings).to_string_lossy()
    );
    let _ = writeln!(
        out,
        "[doctor] space_dir: {}  worktree: {}",
        super::space_dir(&settings).to_string_lossy(),
        super::worktree_space_dir(&settings).to_string_lossy()
    );

    let accessors: [(&str, PathBuf); 9] = [
        ("state_dir", super::state_dir(&settings)),
        ("graph_json", super::graph_json(&settings)),
        ("ledger_json", super::ledger_json(&settings)),
        (
            "briefs_dir",
            super::state_child(&settings, "briefs_dir", "briefs"),
        ),
        (
            "fleet_dir",
            super::state_child(&settings, "fleet_dir", "fleet"),
        ),
        (
            "postmortems_dir",
            super::state_child(&settings, "postmortems_dir", "postmortems"),
        ),
        (
            "worktrees_base",
            super::state_child(&settings, "worktrees_base", "worktrees"),
        ),
        (
            "memory_dir",
            super::state_child(&settings, "memory_dir", "memory"),
        ),
        (
            "hook_logs_dir",
            super::state_child(&settings, "hook_logs_dir", "hook-logs"),
        ),
    ];
    let suspicious: Vec<(&str, &str)> = SUSPICIOUS_PATHS
        .iter()
        .filter(|(pattern, _)| !(test_mode() && TEMP_PATTERNS.contains(pattern)))
        .copied()
        .collect();
    let mut issues: Vec<(&str, String, &str)> = Vec::new();
    for (name, resolved) in &accessors {
        let resolved = resolved.to_string_lossy().into_owned();
        let key = if *name == "state_dir" {
            name.to_string()
        } else {
            format!("paths.{name}")
        };
        let note = super::source_note(&key).unwrap_or_else(|| "default".to_string());
        let _ = writeln!(out, "[doctor]   {name}: {resolved}  (config.{key} {note})");
        for (pattern, reason) in &suspicious {
            let expanded = super::resolve(Path::new(&super::expanduser(pattern)))
                .to_string_lossy()
                .into_owned();
            if resolved.starts_with(&expanded) || resolved.starts_with(*pattern) {
                issues.push((*name, resolved.clone(), *reason));
                break;
            }
        }
    }
    if !issues.is_empty() {
        let _ = writeln!(
            out,
            "\n[doctor] {} suspicious path(s) detected:",
            issues.len()
        );
        for (name, path, reason) in &issues {
            let _ = writeln!(out, "  - {name} = {path}: {reason}");
        }
        out.push_str("\nSet a durable path with 'fno config set <key> <path>'.\n");
    }

    let schema = settings.json_schema();
    let leaves = settings.leaves();
    let reports: [(&str, Vec<String>, &str); 9] = [
        (
            "unreadable settings file(s)",
            check_config_files_read(),
            "A file that does not parse contributes NOTHING; every key in it is silently at its default. config.toml is TOML, settings.yaml is YAML.",
        ),
        (
            "unknown config key(s)",
            super::schema_walk::unknown_key_problems(schema, &leaves, &layer_rows),
            "An unknown key is ignored for forward compatibility, so it sets nothing.",
        ),
        (
            "config value(s) the schema refuses",
            check_values(&settings),
            "Each line names its file, key, offending value and legal set.",
        ),
        (
            "switch(es) enabled with an empty population",
            check_enabled_with_empty_population(&settings),
            "",
        ),
        (
            "malformed config.kanban.wip_caps entr(ies)",
            check_wip_caps(),
            "Each column expects a positive integer (e.g. `now = 20`).",
        ),
        (
            "worktree-policy issue(s)",
            check_worktree_policy(),
            "Valid policy values: never | harness-native | external.",
        ),
        (
            "agent-profile issue(s)",
            check_agent_profiles(&settings),
            "Set a substrate each lane's resolved provider can actually launch.",
        ),
        (
            "state-root write issue(s)",
            check_state_root_writable(),
            "fno prints this line and never edits a harness settings file; the grant is yours to make.",
        ),
        (
            "account / provider issue(s)",
            check_accounts(),
            "Fix the accounts or combos configuration in config.toml.",
        ),
    ];
    let mut reported = false;
    for (heading, problems, remedy) in &reports {
        if problems.is_empty() {
            continue;
        }
        reported = true;
        let _ = writeln!(out, "\n[doctor] {} {heading}:", problems.len());
        for reason in problems {
            let _ = writeln!(out, "  - {reason}");
        }
        if !remedy.is_empty() {
            let _ = writeln!(out, "\n{remedy}");
        }
    }
    if !issues.is_empty() || reported {
        return 1;
    }
    out.push_str("\n[doctor] OK; no suspicious paths detected.\n");
    0
}
