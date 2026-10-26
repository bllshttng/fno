//! The `fno config` read-side doors that answer natively on the front door.
//!
//! One classify, one dispatch line in `main.rs`, bodies beside this module:
//! [`get`], [`history`], [`leaves`] and [`plugin_install`]. The write side
//! (`set`, `unset`, `rm`) and the doctor verb stay Python for now: the writer
//! belongs to the config-writer port, and the bare doctor engine
//! (`run_doctor` in `cli/src/fno/setup/doctor.py`) is the setup family's
//! file, so the verb moves with its engine rather than forking a second
//! doctor leg.
//!
//! The reader replays the Python loader's merge: candidate chain, per-layer
//! legacy alias, deep merge, worktree-local allowlist, then the schema table
//! (`config_schema.json`, generated from the pydantic model) for defaults and
//! field order. Deliberately no YAML parser and no migrate-on-read: the
//! loader converts a legacy `settings.yaml` to flat `config.toml` on its own
//! first read, so a healthy install never serves YAML through this chain; a
//! YAML file that survived migration (malformed, or `FNO_CONFIG`-pinned)
//! contributes nothing here, and the parity test folds that case by name.

pub mod get;
pub mod history;
pub mod leaves;
pub mod plugin_install;

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One claimed read-side door. The payload is the argv AFTER the door words,
/// byte-verbatim from the caller.
#[derive(Debug, PartialEq, Eq)]
pub enum Door {
    Get(Vec<OsString>),
    History(Vec<OsString>),
    BuildDir,
    Budget(Vec<OsString>),
    StatusSinks(Vec<OsString>),
    ActiveBacklog,
    PluginInstall(Vec<OsString>),
}

/// Claim the read-side `config` leaves lexically, before clap, the way
/// `config_defaults::classify` does. `None` forwards to the Python shell
/// untouched: the writer verbs, the doctor verb, the lazy mounts (accounts,
/// paths, route, routing) and the codex arm of `plugin install`.
pub fn classify(args: &[OsString]) -> Option<Door> {
    let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    if words.len() != args.len() {
        return None;
    }
    match words.as_slice() {
        ["config", "get", rest @ ..] => {
            // `config get --defaults ...` is already claimed upstream by
            // `config_defaults::classify`; reaching here means the argv is
            // the plain read, whatever flags ride along.
            let _ = rest;
            Some(Door::Get(args[2..].to_vec()))
        }
        ["config", "history", rest @ ..] => Some(Door::History(
            rest.iter().map(|w| OsString::from(*w)).collect(),
        )),
        ["config", "build-dir"] => Some(Door::BuildDir),
        ["config", "assert-subagent-budget", rest @ ..] => Some(Door::Budget(
            rest.iter().map(|w| OsString::from(*w)).collect(),
        )),
        ["config", "status-sinks", rest @ ..] => Some(Door::StatusSinks(
            rest.iter().map(|w| OsString::from(*w)).collect(),
        )),
        ["config", "active-backlog"] => Some(Door::ActiveBacklog),
        ["config", "plugin", "install", tail @ ..] => {
            // The codex arm stays on the Python converge engine (ship-phase
            // ruling, setup family): forward whenever the harness word is or
            // could be codex, claim every other known word. Flag modes carry
            // the harness after them (`--stage-only codex`), and `--source`
            // consumes a value that may legally spell "codex".
            match harness_word(tail) {
                Some("codex") | None => None,
                Some(_) => Some(Door::PluginInstall(args[3..].to_vec())),
            }
        }
        _ => None,
    }
}

/// The harness word of a `plugin install` tail: the first token that is not a
/// flag-mode flag, not `--force`/`-F`, and not the value of a `--source`.
/// `None` means no harness word (or only flags) - forward, so Python prints
/// its usage line byte-identically.
fn harness_word<'a>(tail: &[&'a str]) -> Option<&'a str> {
    let mut expect_source_value = false;
    for token in tail {
        if expect_source_value {
            expect_source_value = false;
            continue;
        }
        match *token {
            "--check" | "--restage" | "--stage-only" | "--env-only" | "--force" | "-F" => {}
            "--source" => expect_source_value = true,
            t if t.starts_with('-') => {}
            t => return Some(t),
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Repo roots
// ---------------------------------------------------------------------------

/// `fno.paths.resolve_repo_root`: the `FNO_REPO_ROOT` pin, then
/// `git rev-parse --show-toplevel`, then the cwd.
pub fn repo_root() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT") {
        if !pin.is_empty() {
            return resolve_path(Path::new(&pin));
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output()
    {
        if out.status.success() {
            let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !line.is_empty() {
                return resolve_path(Path::new(&line));
            }
        }
    }
    cwd
}

/// `fno.paths.resolve_canonical_repo_root`: the main working tree from
/// `git worktree list`, for project-level config lookup only.
pub fn canonical_repo_root(worktree: &Path) -> PathBuf {
    let out = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(worktree)
        .output();
    if let Ok(out) = out {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut current: Option<PathBuf> = None;
            for line in text.lines() {
                if let Some(path) = line.strip_prefix("worktree ") {
                    current = Some(PathBuf::from(path));
                } else if line == "bare" {
                    current = None;
                } else if line.is_empty() {
                    if let Some(path) = current.take() {
                        if path.join(".git").exists() {
                            return resolve_path(&path);
                        }
                    }
                }
            }
            if let Some(path) = current {
                if path.join(".git").exists() {
                    return resolve_path(&path);
                }
            }
        }
    }
    resolve_path(worktree)
}

/// `Path.resolve()`: symlink-collapsed absolute form, best-effort on failure.
pub fn resolve_path(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    // A missing tail is legal (Python's resolve() normalizes anyway): collapse
    // the existing prefix and append the rest.
    let mut parts: Vec<PathBuf> = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        match cursor.canonicalize() {
            Ok(resolved) => {
                let mut out = resolved;
                for part in parts.iter().rev() {
                    out = out.join(part);
                }
                return out;
            }
            Err(_) => {
                let Some(name) = cursor.file_name() else {
                    return path.to_path_buf();
                };
                parts.push(PathBuf::from(name));
                if !cursor.pop() {
                    return path.to_path_buf();
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The candidate chain and its layers
// ---------------------------------------------------------------------------

fn global_settings_path() -> PathBuf {
    if let Some(env) = std::env::var_os("FNO_GLOBAL_SETTINGS_PATH") {
        if !env.is_empty() {
            return PathBuf::from(env);
        }
    }
    home_dir().join(".fno").join("settings.yaml")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `_settings_yaml_locations`: the ordered settings locations, highest
/// priority first. With a `FNO_CONFIG` pin, the chain is the one pinned file.
pub fn settings_yaml_locations(repo_root: &Path) -> Vec<PathBuf> {
    if pinned() {
        return vec![pinned_path()];
    }
    chain_at(repo_root)
}

fn pinned() -> bool {
    std::env::var("FNO_CONFIG")
        .map(|p| !p.is_empty())
        .unwrap_or(false)
}

fn pinned_path() -> PathBuf {
    PathBuf::from(std::env::var("FNO_CONFIG").unwrap_or_default())
}

/// The chain anchored at `repo_root` WITHOUT the `FNO_CONFIG` pin, the way
/// `load_settings_for_repo` reads a foreign repo's config.
pub fn chain_at(repo_root: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![repo_root.join(".fno").join("settings.yaml")];
    if std::env::var("FNO_NO_CANONICAL_CONFIG") != Ok("1".to_string()) {
        let canonical = canonical_repo_root(repo_root)
            .join(".fno")
            .join("settings.yaml");
        if !candidates.contains(&canonical) {
            candidates.push(canonical);
        }
    }
    candidates.push(global_settings_path());
    candidates
}

/// `_prefer_toml`: each `settings.yaml` location gains its `config.toml`
/// sibling as the higher-priority candidate.
fn prefer_toml(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.file_name().and_then(|n| n.to_str()) == Some("settings.yaml") {
            let toml = path.with_file_name("config.toml");
            if !out.contains(&toml) {
                out.push(toml);
            }
        }
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// `_candidate_paths`: the ordered read candidates, config.toml-first. No
/// migrate-on-read runs here (the Rust reader is read-only); on a migrated
/// install the list is byte-identical to Python's.
pub fn candidate_paths(repo_root: &Path) -> Vec<PathBuf> {
    prefer_toml(settings_yaml_locations(repo_root))
}

/// `_read_settings_doc` for the Rust chain: TOML by suffix, everything else
/// contributes nothing (the YAML fold named in the module doc).
pub fn parse_config_file(path: &Path) -> Option<Value> {
    if path.extension().and_then(|e| e.to_str()) != Some("toml") {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: toml::Value = toml::from_str(&text).ok()?;
    Some(toml_to_json(parsed))
}

/// TOML parse tree to the JSON-shaped tree every walker here reads.
pub fn toml_to_json(value: toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let mut map = Map::new();
            for (key, value) in table {
                map.insert(key, toml_to_json(value));
            }
            Value::Object(map)
        }
    }
}

/// `_deep_merge`: nested dicts merge key-by-key, everything else replaces
/// wholesale, and a `null` override never nulls out an existing dict.
pub fn deep_merge(base: &mut Value, override_value: &Value) {
    match (base, override_value) {
        (Value::Object(base_map), Value::Object(over_map)) => {
            for (key, value) in over_map {
                match base_map.get_mut(key) {
                    Some(existing) if existing.is_object() && value.is_object() => {
                        deep_merge(existing, value);
                    }
                    Some(existing) if existing.is_object() && value.is_null() => {}
                    _ => {
                        base_map.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (slot, value) => *slot = value.clone(),
    }
}

/// `_unwrap_config_dict`: lift a legacy `config:`-wrapped block to the top
/// level; the wrapped block wins over a stray top-level key of the same name.
pub fn unwrap_config_dict(raw: &Value) -> Value {
    let map = match raw.as_object() {
        Some(map) => map,
        None => return raw.clone(),
    };
    let wrapped = match map.get("config").and_then(|c| c.as_object()) {
        Some(wrapped) => wrapped,
        None => return raw.clone(),
    };
    let mut rest = Map::new();
    for (key, value) in map {
        if key != "config" {
            rest.insert(key.clone(), value.clone());
        }
    }
    let mut out = Value::Object(rest);
    deep_merge(&mut out, &Value::Object(wrapped.clone()));
    out
}

/// `_flatten_leaf_paths`: dotted path and value for every leaf in a tree.
fn flatten_leaf_paths(value: &Value, prefix: &str, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_leaf_paths(entry, &path, out);
            }
        }
        other => out.push((prefix.to_string(), other.clone())),
    }
}

/// `WORKTREE_LOCAL_KEYS`: the dotted, flat paths a `config.local.toml` may
/// override per worktree. Everything else in that file is dropped.
const WORKTREE_LOCAL_KEYS: [&str; 1] = ["project.id"];

/// `_worktree_local_override`: the local file reduced to the allowlisted
/// leaves, nested back into blocks.
pub(super) fn worktree_local_override(local_raw: &Value) -> Option<Value> {
    let mut leaves: Vec<(String, Value)> = Vec::new();
    flatten_leaf_paths(local_raw, "", &mut leaves);
    let mut override_map = Map::new();
    let mut matched = false;
    for (path, value) in leaves {
        if WORKTREE_LOCAL_KEYS.contains(&path.as_str()) {
            matched = true;
            let mut parts = path.split('.');
            let leaf = parts.next_back().unwrap_or("");
            let mut cursor = &mut override_map;
            for parent in parts {
                cursor = cursor
                    .entry(parent.to_string())
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
                    .unwrap();
            }
            cursor.insert(leaf.to_string(), value);
        }
    }
    if matched {
        Some(Value::Object(override_map))
    } else {
        None
    }
}

/// `_alias_am_grant`: fold a scope's legacy `dispatch.auto_merge` into
/// `auto_merge.grant`; a canonical grant present wins. True when it folded.
fn alias_am_grant(scope: &mut Value) -> bool {
    let map = match scope.as_object_mut() {
        Some(map) => map,
        None => return false,
    };
    let dispatch = match map.get("dispatch").and_then(|d| d.as_object()) {
        Some(dispatch) => dispatch.clone(),
        None => return false,
    };
    let legacy = match dispatch.get("auto_merge") {
        Some(legacy) if !legacy.is_null() => legacy.clone(),
        _ => return false,
    };
    let am = map.get("auto_merge").and_then(|a| a.as_object()).cloned();
    let mut am = am.unwrap_or_default();
    if am.contains_key("grant") {
        return false;
    }
    let grant = if legacy == Value::Bool(true) {
        "dispatch"
    } else {
        "none"
    };
    am.insert("grant".to_string(), Value::String(grant.to_string()));
    map.insert("auto_merge".to_string(), Value::Object(am));
    true
}

/// `_alias_legacy_keys` over one parsed layer: the legacy spellings bridge to
/// their canonical modeled paths before the merge, so a high-priority legacy
/// value beats a low-priority canonical one. Warnings stay Python-side; the
/// Rust reader is silent by construction (the loader warns on its own walks).
pub fn alias_legacy_keys(raw: &mut Value) {
    if !raw.is_object() {
        return;
    }
    let had_config = raw.get("config").map(|c| c.is_object()).unwrap_or(false);
    let mut config: Map<String, Value> = raw
        .get("config")
        .and_then(|c| c.as_object())
        .cloned()
        .unwrap_or_default();

    // review.external_reviewers (list or scalar) -> review.external_reviewers.
    let canonical_present = config
        .get("review")
        .and_then(|review| review.as_object())
        .map(|review| review.contains_key("external_reviewers"))
        .unwrap_or(false);
    if !canonical_present {
        let resolved = raw
            .get("external_reviewers")
            .filter(|v| !v.is_null())
            .or_else(|| raw.get("external_reviewer").filter(|v| !v.is_null()));
        if let Some(resolved) = resolved {
            let mut review = config
                .get("review")
                .and_then(|review| review.as_object())
                .cloned()
                .unwrap_or_default();
            review.insert("external_reviewers".to_string(), resolved.clone());
            config.insert("review".to_string(), Value::Object(review));
        }
    }

    // Top-level project.* -> config.project.*, only for a legacy config:-wrapped
    // file; in the flat shape top-level project IS canonical.
    let top_project = raw.get("project").filter(|p| p.is_object()).cloned();
    if had_config {
        if let Some(top_project) = top_project {
            let mut cfg_project = config
                .get("project")
                .and_then(|project| project.as_object())
                .cloned()
                .unwrap_or_default();
            let mut lifted = false;
            for field in ["id", "vision"] {
                let top_value = top_project.get(field).filter(|v| !v.is_null());
                let cfg_absent = cfg_project.get(field).map(|v| v.is_null()).unwrap_or(true);
                if let (Some(value), true) = (top_value, cfg_absent) {
                    cfg_project.insert(field.to_string(), value.clone());
                    lifted = true;
                }
            }
            if lifted {
                config.insert("project".to_string(), Value::Object(cfg_project));
            }
        }
        // Top-level work -> config.work, same legacy-only rule.
        if let Some(top_work) = raw.get("work") {
            if top_work.is_object() && !config.get("work").map(|w| w.is_object()).unwrap_or(false) {
                config.insert("work".to_string(), top_work.clone());
            }
        }
    }

    // dispatch.auto_merge -> auto_merge.grant, per scope: the top level, then
    // the wrapped config block when one is present. The Python arm writes the
    // mutated block back; the unconditional write at the tail does the same.
    alias_am_grant(raw);
    if had_config {
        let mut config_value = Value::Object(config.clone());
        alias_am_grant(&mut config_value);
        config = config_value.as_object().cloned().unwrap_or_default();
    }

    // Top-level providers -> accounts (the pre-rename spelling), at both
    // scopes.
    if !raw.as_object().unwrap().contains_key("accounts") {
        if let Some(providers) = raw.get("providers") {
            let providers = providers.clone();
            raw.as_object_mut()
                .unwrap()
                .insert("accounts".to_string(), providers);
        }
    }
    if !config.contains_key("accounts") {
        if let Some(providers) = config.get("providers") {
            let providers = providers.clone();
            config.insert("accounts".to_string(), providers);
        }
    }

    // Write the mutated wrapped block back once; the Python arms each wrote it
    // back as they fired, and an unfired arm leaves `config` equal to the
    // original block, so one unconditional write lands the same shape.
    if had_config {
        raw.as_object_mut()
            .unwrap()
            .insert("config".to_string(), Value::Object(config));
    }
}

/// `_aliased_layers`: `(resolved path, alias-applied raw parse)` per existing
/// parseable candidate, highest precedence first. YAML candidates contribute
/// nothing (the fold in the module doc). Honors a `FNO_CONFIG` pin.
pub fn aliased_layers(repo_root: &Path) -> Vec<(PathBuf, Value)> {
    let mut layers = Vec::new();
    for candidate in candidate_paths(repo_root) {
        if !candidate.is_file() {
            continue;
        }
        if let Some(mut raw) = parse_config_file(&candidate) {
            // The alias pass runs on the RAW parsed shape, before any unwrap
            // lifts the config block - the loader's exact order.
            alias_legacy_keys(&mut raw);
            layers.push((resolve_path(&candidate), raw));
        }
    }
    layers
}

/// The same walk anchored at `root` WITHOUT the `FNO_CONFIG` pin, the way
/// `load_settings_for_repo` reads a foreign repo's chain.
pub fn aliased_layers_at(root: &Path) -> Vec<(PathBuf, Value)> {
    let mut layers = Vec::new();
    for candidate in chain_at(root) {
        if !candidate.is_file() {
            continue;
        }
        if let Some(mut raw) = parse_config_file(&candidate) {
            alias_legacy_keys(&mut raw);
            layers.push((resolve_path(&candidate), raw));
        }
    }
    layers
}

pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

/// The loader's full merge: layers deep-merged lowest-priority first, then the
/// allowlisted worktree-local override layered on top.
pub fn merged_config(repo_root: &Path) -> Value {
    let layers = aliased_layers(repo_root);
    let mut raw = Value::Object(Map::new());
    for (_path, parsed) in layers.iter().rev() {
        deep_merge(&mut raw, parsed);
    }
    // The override layers beside the highest-priority candidate's directory.
    if let Some(top) = candidate_paths(repo_root).first().cloned() {
        let local = top.parent().map(|dir| dir.join("config.local.toml"));
        if let Some(local) = local {
            if local.is_file() && !is_symlink(&local) {
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

/// `resolve_source`: which file decided a dotted key, replaying the loader's
/// merge. `(decider, overridden)`, `None` when no file sets the key.
pub fn resolve_source(key: &str, repo_root: &Path) -> Option<(PathBuf, Vec<PathBuf>)> {
    let mut layers = aliased_layers(repo_root);
    // The worktree-local override enters as the highest layer through the same
    // allowlist filter the loader applies.
    let candidates = candidate_paths(repo_root);
    if let Some(top) = candidates.first() {
        let local = top.parent().map(|dir| dir.join("config.local.toml"));
        if let Some(local) = local {
            if local.is_file() && !is_symlink(&local) {
                if let Some(parsed) = parse_config_file(&local) {
                    if let Some(override_value) = worktree_local_override(&parsed) {
                        layers.insert(0, (resolve_path(&local), override_value));
                    }
                }
            }
        }
    }

    let variants: Vec<String> = if let Some(stripped) = key.strip_prefix("config.") {
        vec![key.to_string(), stripped.to_string()]
    } else {
        vec![key.to_string(), format!("config.{key}")]
    };

    fn get_dotted<'a>(dotted: &str, data: &'a Value) -> Option<&'a Value> {
        let mut node = data;
        for part in dotted.split('.') {
            node = node.as_object()?.get(part)?;
        }
        Some(node)
    }

    fn value_of(variants: &[String], data: &Value) -> Option<Value> {
        for variant in variants {
            if let Some(got) = get_dotted(variant, data) {
                return Some(got.clone());
            }
        }
        None
    }

    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut setters: Vec<(PathBuf, Value)> = Vec::new();
    let mut decider: Option<PathBuf> = None;
    let mut merged = Value::Object(Map::new());
    let mut prev: Option<Value> = None;
    for (path, parsed) in layers.iter().rev() {
        if seen.contains(path) {
            continue;
        }
        seen.insert(path.clone());
        let own = value_of(&variants, &unwrap_config_dict(parsed));
        if let Some(own) = own.clone() {
            setters.push((path.clone(), own));
        }
        deep_merge(&mut merged, parsed);
        let now = value_of(&variants, &unwrap_config_dict(&merged));
        if let Some(now) = now.clone() {
            let changed = prev.as_ref().map(|p| p != &now).unwrap_or(true);
            if changed {
                decider = Some(path.clone());
            }
            if let Some(own) = own.clone() {
                if own == now {
                    decider = Some(path.clone());
                }
            }
            prev = Some(now);
        }
    }
    if setters.is_empty() {
        return None;
    }
    let decider = decider?;
    let final_value = prev.unwrap_or(Value::Null);
    let overridden = setters
        .iter()
        .filter(|(path, own)| *path != decider && *own != final_value)
        .map(|(path, _)| path.clone())
        .collect();
    Some((decider, overridden))
}

/// Dotted lookup into the merged raw tree; `None` when any hop is missing.
pub fn raw_get<'a>(raw: &'a Value, dotted: &str) -> Option<&'a Value> {
    let mut node = raw;
    for part in dotted.split('.') {
        node = node.as_object()?.get(part)?;
    }
    Some(node)
}

/// Dotted lookup with the loader's prefix tolerance: `config.` is optional.
pub fn raw_get_variant(raw: &Value, key: &str) -> Option<Value> {
    let variants: Vec<String> = match key.strip_prefix("config.") {
        Some(stripped) => vec![key.to_string(), stripped.to_string()],
        None => vec![key.to_string(), format!("config.{key}")],
    };
    for variant in &variants {
        if let Some(got) = raw_get(raw, variant) {
            return Some(got.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_word_skips_flags_and_source_values() {
        assert_eq!(harness_word(&["claude", "--force"]), Some("claude"));
        assert_eq!(harness_word(&["--force", "opencode"]), Some("opencode"));
        assert_eq!(harness_word(&["--source", "codex", "agy"]), Some("agy"));
        assert_eq!(harness_word(&["codex"]), Some("codex"));
        assert_eq!(harness_word(&["--stage-only", "codex"]), Some("codex"));
        assert_eq!(harness_word(&["--force"]), None);
        assert_eq!(harness_word(&[]), None);
    }

    #[test]
    fn classify_claims_read_doors_and_forwards_writer_and_codex() {
        let os = |words: &[&str]| -> Vec<OsString> { words.iter().map(OsString::from).collect() };
        assert!(matches!(
            classify(&os(&["config", "get", "review.max_rounds"])),
            Some(Door::Get(_))
        ));
        assert!(matches!(
            classify(&os(&["config", "history"])),
            Some(Door::History(_))
        ));
        assert!(matches!(
            classify(&os(&["config", "build-dir"])),
            Some(Door::BuildDir)
        ));
        assert!(matches!(
            classify(&os(&["config", "status-sinks", "--json"])),
            Some(Door::StatusSinks(_))
        ));
        assert!(matches!(
            classify(&os(&["config", "active-backlog"])),
            Some(Door::ActiveBacklog)
        ));
        assert!(matches!(
            classify(&os(&["config", "plugin", "install", "claude", "--dev"])),
            Some(Door::PluginInstall(_))
        ));
        // The writer and the doctor stay Python.
        assert_eq!(classify(&os(&["config", "set", "a.b", "1"])), None);
        assert_eq!(classify(&os(&["config", "unset", "a.b"])), None);
        assert_eq!(classify(&os(&["config", "doctor", "--post-merge"])), None);
        // The codex arm stays on the Python converge engine.
        assert_eq!(
            classify(&os(&["config", "plugin", "install", "codex"])),
            None
        );
        // The bare group and unknown leaves forward.
        assert_eq!(classify(&os(&["config"])), None);
        assert_eq!(classify(&os(&["config", "no-such-leaf"])), None);
    }

    #[test]
    fn deep_merge_matches_the_python_semantics() {
        let mut base: Value =
            serde_json::from_str(r#"{"a": {"x": 1, "y": 2}, "list": [1]}"#).unwrap();
        let over: Value =
            serde_json::from_str(r#"{"a": {"y": 3, "z": null}, "list": [2], "null_block": null}"#)
                .unwrap();
        deep_merge(&mut base, &over);
        assert_eq!(base["a"]["x"], serde_json::json!(1));
        assert_eq!(base["a"]["y"], serde_json::json!(3));
        assert_eq!(base["a"]["z"], Value::Null);
        assert_eq!(base["list"], serde_json::json!([2]));
    }
}
