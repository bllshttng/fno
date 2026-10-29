//! The post-merge readiness oracle: whether the `/fno:ship pr merged` ritual
//! can run for a repo, and why. One pure, read-only verdict for `fno config
//! doctor --post-merge`, the bare doctor's summary line, `fno config setup
//! post-merge`, the /target preflight check and the pr-watch tick.
//!
//! Only the repo-local file counts: post_merge is a per-repo opt-in, so a
//! global `parking_lot_path` must not make every repo look ready. A missing
//! file is defaults; an unparseable one or a bad post_merge value is
//! `error` with the cause, never a false verdict.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub status: &'static str,
    pub enabled: bool,
    pub activity: bool,
    pub parking_lot_path: Option<String>,
    pub project_id: Option<String>,
    pub cause: Option<String>,
    pub note: Option<String>,
}

impl Verdict {
    pub fn is_ready(&self) -> bool {
        self.status == "ready"
    }

    /// The verdict as `json.dumps(asdict(verdict))` printed it.
    pub fn to_json(&self) -> String {
        let value = json!({
            "status": self.status,
            "enabled": self.enabled,
            "activity": self.activity,
            "parking_lot_path": self.parking_lot_path,
            "project_id": self.project_id,
            "cause": self.cause,
            "note": self.note,
        });
        crate::config_schema::py_dumps(&value, None, false)
    }

    pub fn to_value(&self) -> Value {
        serde_json::from_str(&self.to_json()).unwrap_or(Value::Null)
    }

    pub fn summary_line(&self) -> String {
        match self.status {
            "ready" => {
                let line = format!(
                    "[doctor] post-merge: ready (parking_lot_path={})",
                    self.parking_lot_path.as_deref().unwrap_or("None")
                );
                match &self.note {
                    Some(note) => format!("{line}; note: {note}"),
                    None => line,
                }
            }
            "unconfigured" => "[doctor] post-merge: unconfigured - config.post_merge.parking_lot_path is unset; the /fno:ship pr merged prose+triage will be skipped. Set it with: fno config setup post-merge".to_string(),
            "opted_out" => "[doctor] post-merge: opted_out (config.post_merge.enabled=false)".to_string(),
            "dormant" => "[doctor] post-merge: dormant (no fno activity in this repo)".to_string(),
            "error" => format!(
                "[doctor] post-merge: error - {}",
                self.cause.as_deref().unwrap_or("None")
            ),
            other => format!("[doctor] post-merge: {other}"),
        }
    }
}

/// The repo the oracle reports on: the git toplevel of the cwd, else the cwd.
pub fn repo_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output()
    {
        let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !line.is_empty() {
            return PathBuf::from(line);
        }
    }
    cwd
}

/// A flat config file, `config.toml` first, a read-only legacy
/// `settings.yaml` second. A malformed file is an error carrying its cause.
fn read_flat(toml_path: &Path, yaml_path: &Path) -> Result<Value, String> {
    if toml_path.is_file() {
        let text = std::fs::read_to_string(toml_path).map_err(|e| format!("OSError: {e}"))?;
        let parsed: toml::Value = toml::from_str(&text).map_err(|e| {
            let first = e.to_string();
            format!("TOMLDecodeError: {}", first.lines().next().unwrap_or(""))
        })?;
        return Ok(crate::config_cli::unwrap_config_dict(
            &crate::config_cli::toml_to_json(parsed),
        ));
    }
    if yaml_path.is_file() {
        let text = std::fs::read_to_string(yaml_path).map_err(|e| format!("OSError: {e}"))?;
        let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).map_err(|e| {
            let first = e.to_string();
            format!("YAMLError: {}", first.lines().next().unwrap_or(""))
        })?;
        let parsed = crate::config_cli::yaml_to_json(parsed);
        return match parsed {
            Value::Null => Ok(Value::Object(Map::new())),
            Value::Object(_) => Ok(crate::config_cli::unwrap_config_dict(&parsed)),
            _ => Err(format!(
                "ValueError: config is not a mapping: {}",
                yaml_path.to_string_lossy()
            )),
        };
    }
    Ok(Value::Object(Map::new()))
}

/// The repo's post_merge block plus its project id, the worktree-local
/// `project.id` override layered on top.
fn load_repo_post_merge(
    repo_root: &Path,
) -> Result<(bool, Option<String>, Option<String>), String> {
    let fno_dir = repo_root.join(".fno");
    let mut raw = read_flat(&fno_dir.join("config.toml"), &fno_dir.join("settings.yaml"))?;

    let local_toml = fno_dir.join("config.local.toml");
    let local_path = if local_toml.is_file() {
        local_toml
    } else {
        fno_dir.join("settings.local.yaml")
    };
    if local_path.is_file() && !crate::config_cli::is_symlink(&local_path) {
        // A corrupt local override is skipped, unlike the repo file: it only
        // supplies project.id, which the repo config can answer without.
        if let Some(parsed) = crate::config_cli::parse_config_file(&local_path) {
            if parsed.is_object() {
                let flat = crate::config_cli::unwrap_config_dict(&parsed);
                if let Some(over) = crate::config_cli::worktree_local_override(&flat) {
                    crate::config_cli::deep_merge(&mut raw, &over);
                }
            }
        }
    }

    let empty = raw.as_object().map(|m| m.is_empty()).unwrap_or(true);
    if empty {
        return Ok((true, None, None));
    }
    let pm = raw.get("post_merge").filter(|v| v.is_object());
    let enabled = match pm.and_then(|pm| pm.get("enabled")) {
        None | Some(Value::Null) => true,
        Some(value) => lax_bool(value).ok_or_else(|| {
            format!(
                "ValidationError: post_merge.enabled = {} is not a valid boolean",
                super::py_repr(value)
            )
        })?,
    };
    let parking = match pm.and_then(|pm| pm.get("parking_lot_path")) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()).filter(|s| !s.is_empty()),
        Some(value) => {
            return Err(format!(
                "ValidationError: post_merge.parking_lot_path = {} is not a valid string",
                super::py_repr(value)
            ))
        }
    };
    let project_id = raw
        .get("project")
        .and_then(|p| p.get("id"))
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    Ok((enabled, parking, project_id))
}

/// pydantic's lax bool: real booleans, 0/1, and the yes/no word set.
fn lax_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => match n.as_i64() {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
            "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// True when this repo ships or plans through fno: an in-flight target
/// session, or a graph node mapping the repo by project id or cwd. Any
/// unreadable state reads as dormant.
fn repo_has_fno_activity(repo_root: &Path, project_id: Option<&str>) -> bool {
    if repo_root.join(".fno").join("target-state.md").is_file() {
        return true;
    }
    let backend = std::env::var("FNO_TRACKER_BACKEND").unwrap_or_default();
    if !backend.is_empty() && backend != "graph" {
        return false;
    }
    let settings = super::Settings::load();
    let anchor = super::graph_json(&settings);
    let store = anchor.with_file_name("graph.db");
    if !store.exists() {
        return false;
    }
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&store, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return false;
    };
    let Ok(mut stmt) = conn.prepare("SELECT project, cwd, extras FROM nodes") else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    }) else {
        return false;
    };
    let root = super::resolve(repo_root).to_string_lossy().into_owned();
    let under =
        |cwd: &str| !cwd.is_empty() && (cwd == root || cwd.starts_with(&format!("{root}/")));
    for (project, cwd, extras) in rows.flatten() {
        if let (Some(want), Some(have)) = (project_id, project.as_deref()) {
            if want == have {
                return true;
            }
        }
        let resolved: Option<String> = extras
            .and_then(|e| serde_json::from_str::<Value>(&e).ok())
            .and_then(|e| {
                e.get("_resolved_cwd")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        if resolved.as_deref().is_some_and(&under) || cwd.as_deref().is_some_and(&under) {
            return true;
        }
    }
    false
}

/// The verdict for `repo_root`. Never writes. Order: settings load, enabled,
/// activity, parking_lot_path.
pub fn readiness(repo_root: &Path) -> Verdict {
    let (enabled, parking, project_id) = match load_repo_post_merge(repo_root) {
        Ok(loaded) => loaded,
        Err(cause) => {
            return Verdict {
                status: "error",
                enabled: true,
                activity: false,
                parking_lot_path: None,
                project_id: None,
                cause: Some(cause),
                note: None,
            }
        }
    };
    let verdict = |status: &'static str,
                   activity: bool,
                   parking: Option<String>,
                   note: Option<String>| Verdict {
        status,
        enabled: status != "opted_out",
        activity,
        parking_lot_path: parking,
        project_id: project_id.clone(),
        cause: None,
        note,
    };
    if !enabled {
        return verdict("opted_out", false, parking, None);
    }
    if !repo_has_fno_activity(repo_root, project_id.as_deref()) {
        return verdict("dormant", false, parking, None);
    }
    if parking.is_none() {
        return verdict("unconfigured", true, None, None);
    }
    let note = if project_id.is_some() {
        None
    } else {
        Some("project.id unset - ritual auto-detects; set for clean provenance".to_string())
    };
    verdict("ready", true, parking, note)
}
