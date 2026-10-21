//! The per-id sidecar file reader, the Rust twin of `fno.tracker.sidecar.load`
//! external mode.
//!
//! [`save`] is the only writer of the per-id file; the Python
//! `_save_to_file` calls it through the tracker door. It refuses any key
//! outside `SIDECAR_KEYS`, so the partition is enforced where the file is
//! written; the reader passes every key through except `id`. The graph
//! backend also projects only `SIDECAR_KEYS` from its row.

use crate::claims::encode_key;
use crate::tracker::TrackerError;
use serde_json::Value;
use std::path::Path;

/// The Python `Sidecar.model_fields` minus `id`: the sidecar-side names, used
/// by the graph backend's `sidecar()` projection. A new Sidecar field must be
/// added here for graph mode to serve it.
pub(crate) const SIDECAR_KEYS: &[&str] = &[
    "cwd",
    "plan_path",
    "pr_number",
    "pr_url",
    "additional_prs",
    "cost_usd",
    "cost_sessions",
    "claimed_at",
    "batch",
    "contained_in",
    "sessions",
    "source_session_id",
    "source_harness",
    "source_cwd",
    "source_node_id",
    "source_plan_path",
    "source_inbox_msg",
    "spawned_by_session",
    "spawned_by_harness",
    "spawned_by_cwd",
];

/// `~/.fno/sidecar` (or the configured state root's `sidecar` dir).
pub(crate) fn root(cwd: &Path) -> std::path::PathBuf {
    crate::agents_config::state_dir(cwd)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("sidecar")
}

/// Read one per-id sidecar file. A missing file is an empty map; a parse or
/// read failure is `Backend` naming the id. Every key passes through except
/// `id` - no second field list here.
pub fn load(root: &Path, id: &str) -> Result<serde_json::Map<String, Value>, TrackerError> {
    read_file(&root.join(format!("{}.json", encode_key(id))), id)
}

fn read_file(path: &Path, id: &str) -> Result<serde_json::Map<String, Value>, TrackerError> {
    if !path.exists() {
        return Ok(serde_json::Map::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| TrackerError::Backend(format!("sidecar unreadable for {id}: {e}")))?;
    let v: Value = serde_json::from_str(&raw)
        .map_err(|e| TrackerError::Backend(format!("sidecar unreadable for {id}: {e}")))?;
    let mut out = serde_json::Map::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if k != "id" {
                out.insert(k.clone(), val.clone());
            }
        }
    }
    Ok(out)
}

/// Merge `fields` into the per-id sidecar file and return its path. A key
/// outside `SIDECAR_KEYS` refuses before any write: the tracker owns it.
/// Temp file plus rename in the same dir, so a reader never sees half a file.
pub fn save(
    root: &Path,
    id: &str,
    fields: &serde_json::Map<String, Value>,
) -> Result<std::path::PathBuf, TrackerError> {
    save_at(&root.join(format!("{}.json", encode_key(id))), id, fields)
}

/// [`save`] at an explicit file path: the Python client passes the path its
/// own `sidecar_path` resolves, so both legs name one file.
pub fn save_at(
    path: &Path,
    id: &str,
    fields: &serde_json::Map<String, Value>,
) -> Result<std::path::PathBuf, TrackerError> {
    if let Some(bad) = fields
        .keys()
        .find(|k| k.as_str() != "id" && !SIDECAR_KEYS.contains(&k.as_str()))
    {
        return Err(TrackerError::Refused(format!(
            "{bad:?} is not a sidecar key; the tracker owns it. Sidecar keys: {}",
            SIDECAR_KEYS.join(", ")
        )));
    }
    let mut merged = read_file(path, id)?;
    for (k, v) in fields {
        if k != "id" {
            merged.insert(k.clone(), v.clone());
        }
    }
    let mut body = serde_json::Map::new();
    body.insert("id".into(), Value::String(id.to_string()));
    body.extend(merged);
    let fail = |e: &dyn std::fmt::Display| {
        TrackerError::Backend(format!("sidecar write failed for {id}: {e}"))
    };
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| fail(&e))?;
    let mut text = serde_json::to_string_pretty(&Value::Object(body)).map_err(|e| fail(&e))?;
    text.push('\n');
    let tmp = tempfile::Builder::new()
        .prefix(".sidecar-tmp-")
        .tempfile_in(dir)
        .map_err(|e| fail(&e))?;
    std::fs::write(tmp.path(), text).map_err(|e| fail(&e))?;
    tmp.persist(path).map_err(|e| fail(&e.error))?;
    Ok(path.to_path_buf())
}
