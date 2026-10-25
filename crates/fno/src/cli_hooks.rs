//! `fno config setup cli-hooks` and `cli-hooks-codex`: wire footnote's hooks
//! into the user-level CLI config the plugin manifest cannot reach.
//!
//! - Gemini: `~/.gemini/settings.json` -> `hooks.SessionStart` (JSON).
//! - Codex: `~/.codex/config.toml` -> `[[hooks.SessionStart]]` (TOML).
//! - Claude: `~/.claude/settings.json` -> `hooks.WorktreeRemove` (JSON).
//!
//! Claude's SessionStart hook arrives through the plugin manifest. Its
//! WorktreeRemove hook cannot, because `claude rm` runs with no agent session
//! and so never loads plugin hooks. The settings file is the only place that
//! reaches it.
//!
//! Every writer is idempotent, backs the file up to `<name>.fno-bak` first,
//! writes through a temp file and a rename, and merges into what is there.
//! Codex treats a user-config hook as untrusted until the user approves it,
//! so a Codex write ends with that instruction.
//!
//! `hooks/session-start.sh` runs `cli-hooks --no-codex --no-gemini --claude
//! --repair-only` when the plugin root moves, so that path must stay quiet.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Footnote's SessionStart entry script, relative to the plugin root.
const SESSION_START_SCRIPT: &str = "hooks/session-start.sh";
/// The script Claude's WorktreeRemove hook runs, relative to the plugin root.
const WORKTREE_REMOVE_SCRIPT: &str = "hooks/worktree-remove.sh";
/// A file only the plugin ships: its presence proves a directory is the root.
const PLUGIN_MARKER: &str = "hooks/helpers/init-target-state.sh";
const GEMINI_HOOK_NAME: &str = "fno-session-start";

const CLI_HOOKS_USAGE: &str = "usage: fno config setup cli-hooks [--codex/--no-codex] \
[--gemini/--no-gemini] [--claude/--no-claude] [--codex-config PATH] \
[--gemini-settings PATH] [--claude-settings PATH] [--repair-only]";
const CODEX_USAGE: &str = "usage: fno config setup cli-hooks-codex [--codex-config PATH] \
[--codex-hooks-json PATH] [--migrate-legacy-hooks-json]";

/// One installer's outcome. `note` is advice or a refusal; `error` marks
/// the refusals, which fail the command.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InstallResult {
    pub path: PathBuf,
    pub changed: bool,
    pub already_present: bool,
    pub backup: Option<PathBuf>,
    pub needs_trust: bool,
    pub note: Option<String>,
    pub legacy_backup: Option<PathBuf>,
    pub error: Option<String>,
}

impl InstallResult {
    fn at(path: &Path) -> InstallResult {
        InstallResult {
            path: path.to_path_buf(),
            ..InstallResult::default()
        }
    }

    fn refused(path: &Path, message: &str) -> InstallResult {
        InstallResult {
            note: Some(message.to_string()),
            error: Some(message.to_string()),
            ..InstallResult::at(path)
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin root
// ---------------------------------------------------------------------------

fn expand_tilde(p: &Path) -> PathBuf {
    let Some(text) = p.to_str() else {
        return p.to_path_buf();
    };
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
        return p.to_path_buf();
    };
    if text == "~" {
        return PathBuf::from(home);
    }
    match text.strip_prefix("~/") {
        Some(rest) => PathBuf::from(home).join(rest),
        None => p.to_path_buf(),
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(|v| expand_tilde(Path::new(&v)))
}

fn git_line(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// Map a linked-worktree plugin root onto its canonical checkout. A worktree
/// path persisted into user settings dies when the worktree is archived. A
/// non-git root (a release tarball) and any git failure return it unchanged.
fn canonical_plugin_root(root: &Path) -> PathBuf {
    let common = git_line(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    if let Some(common) = common.map(PathBuf::from) {
        if common.is_absolute() && common.file_name().is_some_and(|n| n == ".git") {
            if let Some(canon) = common.parent() {
                if canon.join(".claude-plugin/plugin.json").is_file() {
                    return canon.to_path_buf();
                }
            }
        }
    }
    root.to_path_buf()
}

/// The `~/.fno/install/plugin-root` pointer the session-start hook writes,
/// when it still names a plugin.
fn persisted_plugin_root() -> Option<PathBuf> {
    let pointer = match env_path("FNO_HOME") {
        Some(home) => home.join("install/plugin-root"),
        None => env_path("HOME")?.join(".fno/install/plugin-root"),
    };
    let text = std::fs::read_to_string(pointer).ok()?;
    let root = canonical_plugin_root(&expand_tilde(Path::new(text.trim())));
    root.join(PLUGIN_MARKER).is_file().then_some(root)
}

fn repo_root() -> PathBuf {
    if let Some(pin) = env_path("FNO_REPO_ROOT") {
        return pin;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    git_line(&cwd, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or(cwd)
}

/// A script that ships with the plugin. The env hints are authoritative:
/// when one is set the script is expected there, with no fall-through.
pub fn plugin_script(rel: &str) -> PathBuf {
    for key in ["CLAUDE_PLUGIN_ROOT", "CODEX_PLUGIN_ROOT", "FNO_REPO_ROOT"] {
        if let Some(root) = env_path(key) {
            return root.join(rel);
        }
    }
    if let Some(root) = persisted_plugin_root() {
        if root.join(rel).exists() {
            return root.join(rel);
        }
    }
    repo_root().join(rel)
}

/// [`plugin_script`] on the canonical checkout, for a path that gets written
/// into a file outside the session.
pub fn plugin_script_durable(rel: &str) -> PathBuf {
    let resolved = plugin_script(rel);
    let mut root = resolved.as_path();
    for _ in Path::new(rel).components() {
        root = root.parent().unwrap_or(root);
    }
    canonical_plugin_root(root).join(rel)
}

// ---------------------------------------------------------------------------
// File helpers
// ---------------------------------------------------------------------------

fn backup(path: &Path) -> std::io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".fno-bak");
    let bak = path.with_file_name(name);
    std::fs::copy(path, &bak)?;
    Ok(Some(bak))
}

/// Temp file plus rename, keeping the old file's mode: a fresh temp file is
/// 0600, which would tighten a 0644 user config.
fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mode = std::fs::metadata(path).ok().map(|m| m.permissions());
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(text.as_bytes())?;
    if let Some(mode) = mode {
        std::fs::set_permissions(tmp.path(), mode)?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Back up, then write `data` as indented JSON.
fn write_json(path: &Path, data: &Value) -> std::io::Result<Option<PathBuf>> {
    let bak = backup(path)?;
    let text = serde_json::to_string_pretty(data).map_err(std::io::Error::other)?;
    atomic_write(path, &(text + "\n"))?;
    Ok(bak)
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// The settings object to merge into, or the reason not to. An absent file
/// merges into an empty object. A file that exists but is not a readable
/// JSON object must never be overwritten as if it were empty.
fn load_settings(path: &Path) -> Result<Map<String, Value>, String> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("settings.json could not be read ({e}); left unchanged"))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(other) => Err(format!(
            "settings.json root is {}, not an object; left unchanged",
            kind(&other)
        )),
        Err(e) => Err(format!("settings.json is malformed ({e}); left unchanged")),
    }
}

/// The child array at `obj[key]`, created when absent. `None` when the key
/// holds something else, so the caller refuses rather than overwrite it.
fn child_array<'a>(obj: &'a mut Map<String, Value>, key: &str) -> Option<&'a mut Vec<Value>> {
    obj.entry(key).or_insert_with(|| json!([])).as_array_mut()
}

/// Python's `shlex.quote`: unchanged when every character is safe, else one
/// single-quoted word.
fn sh_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// `FNO_PLATFORM` tells the shared entry script which CLI is calling: Codex
/// and Gemini set no plugin-root variable for a user-config hook.
fn wrapped_command(command: &str, cli: &str) -> String {
    format!("env FNO_PLATFORM={cli} {command}")
}

// ---------------------------------------------------------------------------
// Hook readers (the shared TOML/JSON hook shape)
// ---------------------------------------------------------------------------

fn hooks_table<'a>(
    data: &'a Value,
    source: &str,
) -> Result<Option<&'a Map<String, Value>>, String> {
    let Value::Object(root) = data else {
        return Err(format!("{source} root is not an object"));
    };
    match root.get("hooks") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(hooks)) => Ok(Some(hooks)),
        Some(_) => Err(format!("{source} `hooks` is not an object")),
    }
}

/// The commands in one event's groups. `Null` reads as empty.
fn event_commands(
    groups: &Value,
    source: &str,
    event: &str,
    out: &mut Vec<String>,
) -> Result<(), String> {
    let groups = match groups {
        Value::Null => return Ok(()),
        Value::Array(g) => g,
        _ => return Err(format!("{source} `hooks.{event}` is not an array")),
    };
    for group in groups {
        let Value::Object(group) = group else {
            return Err(format!("{source} {event} group is not an object"));
        };
        let entries = match group.get("hooks") {
            None | Some(Value::Null) => continue,
            Some(Value::Array(e)) => e,
            Some(_) => return Err(format!("{source} {event} `hooks` is not an array")),
        };
        for hook in entries {
            let Value::Object(hook) = hook else {
                return Err(format!("{source} {event} hook is not an object"));
            };
            match hook.get("command") {
                None | Some(Value::Null) => {}
                Some(Value::String(c)) if c.is_empty() => {}
                Some(Value::String(c)) => out.push(c.clone()),
                Some(_) => return Err(format!("{source} {event} command is not a string")),
            }
        }
    }
    Ok(())
}

/// Every hook command, all events, trust state excluded.
fn all_commands(data: &Value, source: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    if let Some(hooks) = hooks_table(data, source)? {
        for (event, groups) in hooks {
            if event != "state" {
                event_commands(groups, source, event, &mut out)?;
            }
        }
    }
    Ok(out)
}

fn session_start_commands(data: &Value, source: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    if let Some(groups) = hooks_table(data, source)?.and_then(|h| h.get("SessionStart")) {
        event_commands(groups, source, "SessionStart", &mut out)?;
    }
    Ok(out)
}

/// Only footnote's own Codex SessionStart commands.
fn is_footnote_codex_command(command: &str) -> bool {
    command.contains(SESSION_START_SCRIPT)
        && (command.contains("FNO_PLATFORM=codex")
            || command.contains("/footnote/hooks/session-start.sh"))
}

fn parse_toml(text: &str) -> Result<Value, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    serde_json::to_value(table).map_err(|e| e.to_string())
}

/// The two Codex hook layers, read without writing either.
#[derive(Debug, Default)]
struct CodexLayers {
    config_path: PathBuf,
    hooks_json_path: PathBuf,
    state: &'static str,
    json_foreign: Vec<String>,
    errors: Vec<String>,
}

fn inspect_codex(config_path: &Path, hooks_json_path: &Path) -> CodexLayers {
    let mut errors = Vec::new();
    let mut toml_all = Vec::new();
    let mut json_all = Vec::new();
    let mut json_footnote = Vec::new();
    if config_path.exists() {
        let read = std::fs::read_to_string(config_path)
            .map_err(|e| e.to_string())
            .and_then(|t| parse_toml(&t))
            .and_then(|v| {
                session_start_commands(&v, "config.toml")?;
                all_commands(&v, "config.toml")
            });
        match read {
            Ok(all) => toml_all = all,
            Err(e) => errors.push(format!("{}: {e}", config_path.display())),
        }
    }
    if hooks_json_path.exists() {
        let read = std::fs::read_to_string(hooks_json_path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
            .and_then(|v| match v {
                Value::Object(_) => Ok(v),
                _ => Err("expected a JSON object".to_string()),
            })
            .and_then(|v| {
                let session = session_start_commands(&v, "hooks.json")?;
                Ok((session, all_commands(&v, "hooks.json")?))
            });
        match read {
            Ok((session, all)) => {
                json_footnote = session
                    .into_iter()
                    .filter(|c| is_footnote_codex_command(c))
                    .collect();
                json_all = all;
            }
            Err(e) => errors.push(format!("{}: {e}", hooks_json_path.display())),
        }
    }
    let state = match (errors.is_empty(), toml_all.is_empty(), json_all.is_empty()) {
        (false, _, _) => "malformed",
        (true, false, false) => "both",
        (true, false, true) => "toml-only",
        (true, true, false) => "json-only",
        (true, true, true) => "neither",
    };
    let json_foreign = json_all
        .into_iter()
        .filter(|c| !json_footnote.contains(c))
        .collect();
    CodexLayers {
        config_path: config_path.to_path_buf(),
        hooks_json_path: hooks_json_path.to_path_buf(),
        state,
        json_foreign,
        errors,
    }
}

fn codex_note(layers: &CodexLayers, migrated: bool, foreign_remains: bool) -> Option<String> {
    let (config, legacy) = (
        layers.config_path.display(),
        layers.hooks_json_path.display(),
    );
    if !layers.errors.is_empty() {
        return Some(format!(
            "malformed Codex hook config; left malformed input unchanged: {}",
            layers.errors.join("; ")
        ));
    }
    if foreign_remains {
        return Some(format!(
            "legacy hooks remain in {legacy}; foreign hooks were preserved and need manual \
             consolidation into {config} (TOML is preferred)"
        ));
    }
    if layers.state == "both" {
        let action = if migrated {
            "footnote-owned legacy JSON hooks were migrated; "
        } else {
            ""
        };
        if !layers.json_foreign.is_empty() {
            return Some(format!(
                "{action}both Codex hook layers contain hooks: {config} and {legacy}; TOML is \
                 preferred. Legacy JSON contains foreign hooks and needs manual consolidation; \
                 it was left unchanged"
            ));
        }
        return Some(format!(
            "{action}both Codex hook layers contain SessionStart hooks: {config} and {legacy}; \
             TOML is preferred. Re-run with --migrate-legacy-hooks-json to remove only \
             footnote-owned legacy JSON hooks"
        ));
    }
    migrated.then(|| format!("migrated footnote-owned legacy SessionStart hooks from {legacy}"))
}

// ---------------------------------------------------------------------------
// Installers
// ---------------------------------------------------------------------------

/// Merge footnote's SessionStart group into a Gemini settings.json.
pub fn install_gemini(command: &str, settings: &Path) -> InstallResult {
    let mut data = match load_settings(settings) {
        Ok(d) => d,
        Err(note) => return InstallResult::refused(settings, &note),
    };
    let Some(hooks) = data
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return InstallResult::refused(settings, "`hooks` is not an object; left unchanged");
    };
    let Some(session_start) = child_array(hooks, "SessionStart") else {
        return InstallResult::refused(
            settings,
            "`hooks.SessionStart` is not an array; left unchanged",
        );
    };
    let present = session_start
        .iter()
        .filter_map(|g| g.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_object)
        .any(|h| {
            h.get("name").and_then(Value::as_str) == Some(GEMINI_HOOK_NAME)
                || h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.ends_with(SESSION_START_SCRIPT))
        });
    if present {
        return InstallResult {
            already_present: true,
            ..InstallResult::at(settings)
        };
    }
    // No matcher: Gemini matchers are exact strings, so `startup` would miss
    // the resume and clear sources.
    session_start.push(json!({"hooks": [{
        "name": GEMINI_HOOK_NAME,
        "type": "command",
        "command": wrapped_command(command, "gemini"),
        "timeout": 10000,
    }]}));
    match write_json(settings, &Value::Object(data)) {
        Ok(backup) => InstallResult {
            changed: true,
            backup,
            ..InstallResult::at(settings)
        },
        Err(e) => InstallResult::refused(settings, &format!("write failed ({e}); left unchanged")),
    }
}

/// The worktree-remove script a WorktreeRemove command runs, by token, so a
/// quoted path or a trailing argument still matches.
fn worktree_script(command: &str) -> Option<String> {
    shlex::split(command)?
        .into_iter()
        .find(|t| t.ends_with(WORKTREE_REMOVE_SCRIPT))
}

/// Merge footnote's WorktreeRemove hook into a Claude settings.json.
///
/// `repair_only` re-points an entry whose script is gone (a plugin upgrade
/// moves the versioned install dir) and never adds one. A live entry that is
/// not the resolved path is re-pointed only by an explicit install: the
/// automatic repair would otherwise flip global settings between two live
/// roots on every session.
pub fn install_claude_worktree_remove(
    command: &str,
    settings: &Path,
    repair_only: bool,
) -> InstallResult {
    let mut data = match load_settings(settings) {
        Ok(d) => d,
        Err(note) => return InstallResult::refused(settings, &note),
    };
    let Some(hooks) = data
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return InstallResult::refused(settings, "`hooks` is not an object; left unchanged");
    };
    let Some(groups) = child_array(hooks, "WorktreeRemove") else {
        return InstallResult::refused(
            settings,
            "`hooks.WorktreeRemove` is not an array; left unchanged",
        );
    };
    let new_command = format!("bash {}", sh_quote(command));
    let wired = groups.iter_mut().find_map(|group| {
        group
            .get_mut("hooks")?
            .as_array_mut()?
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .find_map(|h| {
                let script =
                    worktree_script(h.get("command").and_then(Value::as_str).unwrap_or(""))?;
                Some((h, script))
            })
    });
    if let Some((hook, script)) = wired {
        let live = Path::new(&script).is_file();
        if live && (repair_only || Path::new(&script) == Path::new(command)) {
            return InstallResult {
                already_present: true,
                ..InstallResult::at(settings)
            };
        }
        hook.insert("command".into(), Value::String(new_command));
        return match write_json(settings, &Value::Object(data)) {
            Ok(backup) => InstallResult {
                changed: true,
                backup,
                note: Some(format!("re-pointed from {script}")),
                ..InstallResult::at(settings)
            },
            Err(e) => {
                InstallResult::refused(settings, &format!("write failed ({e}); left unchanged"))
            }
        };
    }
    if repair_only {
        return InstallResult {
            note: Some("not wired; repair-only, nothing to repair".into()),
            ..InstallResult::at(settings)
        };
    }
    groups.push(json!({"hooks": [{"type": "command", "command": new_command}]}));
    match write_json(settings, &Value::Object(data)) {
        Ok(backup) => InstallResult {
            changed: true,
            backup,
            ..InstallResult::at(settings)
        },
        Err(e) => InstallResult::refused(settings, &format!("write failed ({e}); left unchanged")),
    }
}

fn codex_hook_present(text: &str) -> bool {
    match parse_toml(text).and_then(|v| session_start_commands(&v, "config.toml")) {
        Ok(commands) => commands.iter().any(|c| is_footnote_codex_command(c)),
        Err(_) => text.contains(SESSION_START_SCRIPT) || text.contains("FNO_PLATFORM=codex"),
    }
}

/// Remove footnote-owned SessionStart hooks from the legacy hooks.json,
/// after a backup. Returns the backup, the removed count, and whether any
/// foreign hook remains. The caller has already read the file as valid.
fn migrate_legacy_codex(path: &Path) -> std::io::Result<(Option<PathBuf>, usize, bool)> {
    let text = std::fs::read_to_string(path)?;
    let data: Value = serde_json::from_str(&text).map_err(std::io::Error::other)?;
    let foreign = |v: &Value| {
        all_commands(v, "hooks.json")
            .map(|c| !c.is_empty())
            .unwrap_or(false)
    };
    let mut migrated = data.clone();
    let mut removed = 0;
    if let Some(hooks) = migrated.get_mut("hooks").and_then(Value::as_object_mut) {
        let mut kept_groups = Vec::new();
        for mut group in hooks
            .get("SessionStart")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let entries = group
                .get("hooks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let before = entries.len();
            let kept: Vec<Value> = entries
                .into_iter()
                .filter(|h| {
                    !h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(is_footnote_codex_command)
                })
                .collect();
            removed += before - kept.len();
            if !kept.is_empty() {
                group["hooks"] = Value::Array(kept);
                kept_groups.push(group);
            }
        }
        if removed > 0 {
            if kept_groups.is_empty() {
                hooks.remove("SessionStart");
            } else {
                hooks.insert("SessionStart".into(), Value::Array(kept_groups));
            }
        }
    }
    if removed == 0 {
        return Ok((None, 0, foreign(&data)));
    }
    let bak = backup(path)?;
    let only_ours = migrated.as_object().is_some_and(|root| {
        root.get("hooks")
            .and_then(Value::as_object)
            .is_none_or(Map::is_empty)
            && root.keys().all(|k| k == "description" || k == "hooks")
    });
    if only_ours {
        std::fs::remove_file(path)?;
    } else {
        let text = serde_json::to_string_pretty(&migrated).map_err(std::io::Error::other)?;
        atomic_write(path, &(text + "\n"))?;
    }
    Ok((bak, removed, foreign(&migrated)))
}

/// Append footnote's SessionStart block to a Codex config.toml. It appends
/// text rather than reserializing, so the user's comments survive.
pub fn install_codex(
    command: &str,
    config: &Path,
    hooks_json: &Path,
    migrate_legacy: bool,
) -> InstallResult {
    let before = inspect_codex(config, hooks_json);
    if !before.errors.is_empty() {
        let note = codex_note(&before, false, false).unwrap_or_default();
        return InstallResult::refused(config, &note);
    }
    let existing = std::fs::read_to_string(config).unwrap_or_default();
    let already_present = codex_hook_present(&existing);
    let mut result = InstallResult {
        already_present,
        needs_trust: true,
        ..InstallResult::at(config)
    };
    if !already_present {
        // A TOML basic string escapes the way a JSON string does.
        let quoted = Value::String(wrapped_command(command, "codex")).to_string();
        let block = format!(
            "{} - footnote SessionStart context injection.\n\
             # Codex treats this as an UNMANAGED hook: approve/trust it in Codex before it\n\
             # runs. Remove this block to uninstall.\n\
             [[hooks.SessionStart]]\n\n\
             [[hooks.SessionStart.hooks]]\n\
             type = \"command\"\n\
             command = {quoted}\n",
            crate::uninstall::CODEX_BLOCK_MARK
        );
        let sep = if existing.is_empty() || existing.ends_with("\n\n") {
            ""
        } else if existing.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let written = backup(config).and_then(|bak| {
            atomic_write(config, &format!("{existing}{sep}{block}"))?;
            Ok(bak)
        });
        match written {
            Ok(bak) => {
                result.backup = bak;
                result.changed = true;
            }
            Err(e) => {
                return InstallResult::refused(
                    config,
                    &format!("write failed ({e}); left unchanged"),
                )
            }
        }
    }
    let (mut migrated, mut foreign_remains) = (false, false);
    if migrate_legacy && hooks_json.exists() {
        match migrate_legacy_codex(hooks_json) {
            Ok((bak, removed, foreign)) => {
                result.legacy_backup = bak;
                migrated = removed > 0;
                foreign_remains = foreign;
                result.changed |= migrated;
            }
            Err(e) => {
                let message = format!("legacy hooks.json migration failed ({e})");
                result.note = Some(message.clone());
                result.error = Some(message);
                return result;
            }
        }
    }
    let after = inspect_codex(config, hooks_json);
    result.note = codex_note(&after, migrated, foreign_remains);
    result
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub struct Opts {
    pub codex: bool,
    pub gemini: bool,
    pub claude: bool,
    pub gemini_settings: Option<PathBuf>,
    pub codex_config: Option<PathBuf>,
    pub codex_hooks_json: Option<PathBuf>,
    pub claude_settings: Option<PathBuf>,
    pub migrate_legacy_hooks_json: bool,
    pub repair_only: bool,
}

/// `Ok(None)` is a help request. `codex_only` selects the
/// `cli-hooks-codex` flag set, which wires Codex alone.
pub fn parse_opts(tail: &[OsString], codex_only: bool) -> Result<Option<Opts>, String> {
    let mut o = Opts {
        codex: true,
        gemini: !codex_only,
        claude: !codex_only,
        gemini_settings: None,
        codex_config: None,
        codex_hooks_json: None,
        claude_settings: None,
        migrate_legacy_hooks_json: false,
        repair_only: false,
    };
    let mut i = 0;
    while i < tail.len() {
        let arg = tail[i].to_str().ok_or("arguments must be UTF-8")?;
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg, None),
        };
        // `--codex=false` must refuse, not read as `--codex`.
        let takes_value = matches!(
            flag,
            "--codex-config" | "--codex-hooks-json" | "--gemini-settings" | "--claude-settings"
        );
        if inline.is_some() && !takes_value {
            return Err(format!("{flag} takes no value"));
        }
        let mut value = || -> Result<PathBuf, String> {
            if let Some(v) = inline.clone() {
                return Ok(PathBuf::from(v));
            }
            i += 1;
            tail.get(i)
                .map(PathBuf::from)
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match (flag, codex_only) {
            ("-h" | "--help", _) => return Ok(None),
            ("--codex-config", _) => o.codex_config = Some(value()?),
            ("--codex-hooks-json", true) => o.codex_hooks_json = Some(value()?),
            ("--migrate-legacy-hooks-json", true) => o.migrate_legacy_hooks_json = true,
            ("--codex", false) => o.codex = true,
            ("--no-codex", false) => o.codex = false,
            ("--gemini", false) => o.gemini = true,
            ("--no-gemini", false) => o.gemini = false,
            ("--claude", false) => o.claude = true,
            ("--no-claude", false) => o.claude = false,
            ("--gemini-settings", false) => o.gemini_settings = Some(value()?),
            ("--claude-settings", false) => o.claude_settings = Some(value()?),
            ("--repair-only", false) => o.repair_only = true,
            _ => return Err(format!("unknown option: {arg}")),
        }
        i += 1;
    }
    Ok(Some(o))
}

fn home() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| PathBuf::from("."))
}

fn bak_suffix(bak: &Option<PathBuf>) -> String {
    bak.as_ref()
        .and_then(|b| b.file_name())
        .map(|n| format!("; backed up {}", n.to_string_lossy()))
        .unwrap_or_default()
}

/// Run every requested installer and print one line each. Exit 1 when any
/// installer refused.
pub fn run_opts(o: &Opts) -> i32 {
    let entry = plugin_script(SESSION_START_SCRIPT);
    if !entry.is_file() {
        eprintln!(
            "error: could not locate the installed footnote hooks dir ({}). Run from a \
             footnote-enabled session, or set CLAUDE_PLUGIN_ROOT.",
            entry.display()
        );
        return 1;
    }
    let command = entry.display().to_string();
    let (mut any_change, mut needs_trust, mut failed) = (false, false, false);

    if o.gemini && !o.repair_only {
        let path = o
            .gemini_settings
            .as_deref()
            .map(expand_tilde)
            .unwrap_or_else(|| home().join(".gemini/settings.json"));
        let res = install_gemini(&command, &path);
        any_change |= res.changed;
        let shown = path.display();
        if let Some(note) = &res.note {
            eprintln!("gemini: error: {note} ({shown})");
            failed = true;
        } else if res.already_present {
            println!("gemini: already wired ({shown})");
        } else {
            println!(
                "gemini: wired SessionStart -> {command} ({shown}{})",
                bak_suffix(&res.backup)
            );
        }
    }

    if o.codex && !o.repair_only {
        let config = o
            .codex_config
            .as_deref()
            .map(expand_tilde)
            .unwrap_or_else(|| {
                env_path("CODEX_HOME")
                    .unwrap_or_else(|| home().join(".codex"))
                    .join("config.toml")
            });
        let legacy = o
            .codex_hooks_json
            .as_deref()
            .map(expand_tilde)
            .unwrap_or_else(|| config.with_file_name("hooks.json"));
        let res = install_codex(&command, &config, &legacy, o.migrate_legacy_hooks_json);
        let shown = config.display();
        if let Some(error) = &res.error {
            eprintln!("codex: error: {error} ({shown})");
            failed = true;
        } else {
            any_change |= res.changed;
            needs_trust |= res.needs_trust;
            if let Some(note) = &res.note {
                println!("codex: {note}{} ({shown})", bak_suffix(&res.legacy_backup));
            } else if res.already_present {
                println!("codex: already wired ({shown})");
            } else {
                println!(
                    "codex: wired SessionStart -> {command} ({shown}{})",
                    bak_suffix(&res.backup)
                );
            }
        }
    }

    if o.claude {
        let script = plugin_script_durable(WORKTREE_REMOVE_SCRIPT);
        if !script.is_file() {
            eprintln!(
                "claude: error: could not locate worktree-remove.sh ({})",
                script.display()
            );
            failed = true;
        } else {
            let path = o
                .claude_settings
                .as_deref()
                .map(expand_tilde)
                .unwrap_or_else(|| home().join(".claude/settings.json"));
            let res =
                install_claude_worktree_remove(&script.display().to_string(), &path, o.repair_only);
            any_change |= res.changed;
            let shown = path.display();
            if o.repair_only && !res.changed && res.error.is_none() {
                // Nothing wired, or already healthy: the session hook stays quiet.
            } else if let Some(error) = &res.error {
                eprintln!("claude: error: {error} ({shown})");
                failed = true;
            } else if res.already_present {
                println!("claude: already wired ({shown})");
            } else {
                let fixed = res
                    .note
                    .as_deref()
                    .map(|n| format!("; {n}"))
                    .unwrap_or_default();
                println!(
                    "claude: wired WorktreeRemove -> {} ({shown}{}{fixed})",
                    script.display(),
                    bak_suffix(&res.backup)
                );
            }
        }
    }

    if needs_trust {
        println!(
            "\ncodex: the hook is UNTRUSTED until you approve it. Start Codex and approve the \
             footnote SessionStart hook, then confirm it fires."
        );
    }
    if failed {
        return 1;
    }
    if !any_change {
        println!("\nNothing to do (hooks already wired).");
    }
    0
}

/// `cli-hooks` (`codex_only` false) or `cli-hooks-codex` (true).
pub fn run(tail: &[OsString], codex_only: bool) -> i32 {
    let usage = if codex_only {
        CODEX_USAGE
    } else {
        CLI_HOOKS_USAGE
    };
    match parse_opts(tail, codex_only) {
        Ok(Some(o)) => run_opts(&o),
        Ok(None) => {
            println!(
                "{usage}\n\nWire footnote's user-level CLI hooks: the Codex and Gemini SessionStart \
                 context hook, and Claude's WorktreeRemove hook. Each write is idempotent, backs \
                 the file up first, and keeps your other hooks."
            );
            0
        }
        Err(e) => {
            eprintln!("{e}\n{usage}");
            2
        }
    }
}

#[cfg(test)]
#[path = "cli_hooks_tests.rs"]
mod tests;
