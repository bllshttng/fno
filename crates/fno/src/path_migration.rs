//! The first-run path migration the front door runs before it hands a
//! command to the Python shell.
//!
//! Once per state root, guarded by the `.path-migration-done` sentinel: it
//! writes a `settings.yaml` that spells every schema-managed path key at its
//! default (`state_dir`, `plans_dir`, `paths.*`, `obsidian.*`, `project.*`),
//! deep-merged under any existing file so keys outside the schema survive. An
//! `internal/` symlink in the cwd that resolves into an Obsidian vault turns
//! `obsidian.enabled` on. A prior hand-edited file is moved to `backups/`
//! first. The settings file and the sentinel are written under one lock: both
//! encode one invariant.
//!
//! Best effort: a failure prints one line and never blocks the command.
//! `FNO_SKIP_MIGRATION=1` and `PYTEST_CURRENT_TEST` skip it.

use serde_json::{Map, Value};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SENTINEL: &str = ".path-migration-done";
const SETTINGS: &str = "settings.yaml";
const MANAGED_BLOCKS: [&str; 3] = ["paths", "obsidian", "project"];
const MANAGED_SCALARS: [&str; 2] = ["state_dir", "plans_dir"];

/// The state root as `fno.paths.state_dir()` resolves it: `$FNO_STATE_DIR`,
/// else the configured `state_dir`, else `~/.fno/`.
fn state_root() -> Option<PathBuf> {
    std::env::var_os("HOME")?;
    let settings = crate::config_doctor::Settings::load();
    Some(crate::config_doctor::state_dir(&settings))
}

/// Whether any config source exists that could re-point `state_dir` away
/// from the default root: the `FNO_CONFIG` pin, the `FNO_REPO_ROOT` pin, a
/// repo-level settings file in `cwd` or any ancestor, a global one under
/// `home`, or a checkout above `cwd` (a `.git` marker of either shape - the
/// repo root's own `.fno/` rides the chain even when the nested directory
/// carries none, and a worktree spells `.git` as a file). The fast path may
/// only shortcut when this is false - a sentinel at the default root proves
/// nothing about a root a config file names.
fn config_may_repoint(
    fno_config: bool,
    fno_repo_root: bool,
    cwd: &Path,
    home: Option<&std::ffi::OsStr>,
) -> bool {
    fno_config
        || fno_repo_root
        || cwd.ancestors().any(|dir| {
            [".fno/config.toml", ".fno/settings.yaml"]
                .iter()
                .any(|rel| dir.join(rel).is_file())
                || dir.join(".git").exists()
        })
        || home.is_some_and(|home| {
            ["config.toml", "settings.yaml"]
                .iter()
                .any(|name| PathBuf::from(home).join(".fno").join(name).is_file())
        })
}

/// The front-door hook: run the migration once, never fail the command.
pub fn run_if_needed() {
    if std::env::var("FNO_SKIP_MIGRATION").as_deref() == Ok("1")
        || std::env::var_os("PYTEST_CURRENT_TEST").is_some()
    {
        return;
    }
    // The common case answers from one stat, before the config read spawns
    // git. The stat may only shortcut when no config source exists that could
    // re-point state_dir away from this default root.
    let config_may_repoint = config_may_repoint(
        std::env::var_os("FNO_CONFIG").is_some(),
        std::env::var_os("FNO_REPO_ROOT").is_some(),
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        std::env::var_os("HOME").as_deref(),
    );
    if !config_may_repoint {
        let default_root = std::env::var_os("FNO_STATE_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fno")));
        if default_root.is_some_and(|root| root.join(SENTINEL).exists()) {
            return;
        }
    }
    let Some(root) = state_root() else {
        return;
    };
    if root.join(SENTINEL).exists() {
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match migrate(&root, &cwd) {
        Ok(Some(summary)) => eprintln!("{summary}"),
        Ok(None) => {}
        Err(e) => eprintln!("[setup] migration failed: {e}"),
    }
}

/// The vault an `internal/` symlink in `cwd` points into: the link target's
/// parent when it holds `.obsidian/`, else the target itself when it does.
pub fn detect_vault(cwd: &Path) -> Option<PathBuf> {
    let internal = cwd.join("internal");
    if !std::fs::symlink_metadata(&internal)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    let target = std::fs::canonicalize(&internal).ok()?;
    if let Some(parent) = target.parent() {
        if parent.join(".obsidian").is_dir() {
            return Some(parent.to_path_buf());
        }
    }
    target.join(".obsidian").is_dir().then_some(target)
}

/// The migration's own keys, at their schema defaults, from the generated
/// schema table the config reader also walks.
pub fn managed_overlay(vault: Option<&Path>) -> Value {
    let doc: Value = serde_json::from_str(include_str!("../config_schema.json"))
        .expect("config_schema.json is a committed, generated artifact");
    let mut config = Map::new();
    for block in MANAGED_BLOCKS {
        config.insert(block.to_string(), Value::Object(Map::new()));
    }
    for leaf in doc["leaves"].as_array().into_iter().flatten() {
        let Some(path) = leaf["path"].as_str() else {
            continue;
        };
        let default = leaf.get("default").cloned().unwrap_or(Value::Null);
        match path.split_once('.') {
            None if MANAGED_SCALARS.contains(&path) => {
                config.insert(path.to_string(), default);
            }
            Some((block, key)) if MANAGED_BLOCKS.contains(&block) && !key.contains('.') => {
                if let Some(Value::Object(map)) = config.get_mut(block) {
                    map.insert(key.to_string(), default);
                }
            }
            _ => {}
        }
    }
    if let (Some(vault), Some(Value::Object(obsidian))) = (vault, config.get_mut("obsidian")) {
        obsidian.insert("enabled".into(), Value::Bool(true));
        obsidian.insert("vault".into(), Value::String(vault.display().to_string()));
    }
    let mut out = Map::new();
    out.insert("schema_version".into(), Value::from(1));
    out.insert("config".into(), Value::Object(config));
    Value::Object(out)
}

/// `overlay` merged onto `base`: nested objects recurse, anything else wins.
pub fn deep_merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                let both_objects =
                    value.is_object() && base.get(&key).is_some_and(Value::is_object);
                if both_objects {
                    if let Some(slot) = base.get_mut(&key) {
                        deep_merge(slot, value);
                    }
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

/// Every mapping's keys sorted, the way `yaml.safe_dump(sort_keys=True)` writes.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut pairs: Vec<(String, Value)> = map.into_iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(pairs.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// The YAML the migration writes: `overlay` on top of the readable mapping
/// in `existing`, keys sorted. An unreadable or non-mapping file is ignored.
pub fn render(overlay: Value, existing: Option<&Path>) -> Result<String, String> {
    let mut doc = existing
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text).ok())
        .map(crate::config_cli::yaml_to_json)
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()));
    deep_merge(&mut doc, overlay);
    serde_yaml_ng::to_string(&sorted(doc)).map_err(|e| e.to_string())
}

/// An exclusive `flock` on `path`, retried for up to 30 seconds.
fn lock(path: &Path) -> Result<std::fs::File, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        // SAFETY: flock on an fd this function owns; it touches no memory.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(file);
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for lock; another process may be running.".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Run the migration in `root`. `Ok(None)` when the sentinel already exists
/// (another process won the race); `Ok(Some(line))` names what it wrote.
pub fn migrate(root: &Path, cwd: &Path) -> Result<Option<String>, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let settings = root.join(SETTINGS);
    let sentinel = root.join(SENTINEL);
    let _guard = lock(&root.join(format!("{SETTINGS}.lock")))?;
    if sentinel.exists() {
        return Ok(None);
    }
    // Temp files a crashed run left behind; cleared inside the lock so no
    // other process's in-flight temp is deleted.
    let stale_prefix = format!(".{SETTINGS}.");
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&stale_prefix) && name.ends_with(".tmp") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let _ = std::fs::remove_file(root.join(format!("{SETTINGS}.tmp")));
    let mut notes = Vec::new();
    let mut existing = None;
    if settings.exists() {
        let backups = root.join("backups");
        std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let backup = backups.join(format!("{SETTINGS}.bak.{stamp}"));
        std::fs::rename(&settings, &backup).map_err(|e| e.to_string())?;
        notes.push(format!(
            "[setup] backed up prior settings.yaml to {}",
            backup.display()
        ));
        existing = Some(backup);
    }
    let vault = detect_vault(cwd);
    let text = render(managed_overlay(vault.as_deref()), existing.as_deref())?;
    let mut tmp = tempfile::Builder::new()
        .prefix(&stale_prefix)
        .suffix(".tmp")
        .tempfile_in(root)
        .map_err(|e| e.to_string())?;
    tmp.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    tmp.persist(&settings).map_err(|e| e.error.to_string())?;
    std::fs::File::create(&sentinel).map_err(|e| e.to_string())?;
    let found = match &vault {
        Some(v) => format!("vault at {}", v.display()),
        None => "fresh install".to_string(),
    };
    notes.push(format!(
        "[setup] path migration complete ({found}); settings: {}",
        settings.display()
    ));
    Ok(Some(notes.join("\n")))
}
