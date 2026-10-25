//! The per-id sidecar file reader, plus the one sidecar vocabulary and the
//! graph-row projection both backends share.
//!
//! [`save`] is the only writer of the per-id file, reached through the
//! tracker door's `sidecar-set`. It refuses any key outside `SIDECAR_KEYS`,
//! so the partition is enforced where the file is written; the reader passes
//! every key through except `id`. The graph backend projects its rows
//! through the same key list (`project_row`, with the load semantics the
//! Python `Sidecar` model owned: null lists coerce to `[]`, a present key
//! with the wrong type degrades the projection).

use crate::claims::encode_key;
use crate::tracker::TrackerError;
use serde_json::{json, Map, Value};
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

/// The list fields whose explicit `null` graph value degrades to `[]`, never a
/// projection failure. The Python Sidecar coerced them in a field validator:
/// a row written with `"additional_prs": null` stays readable, because every
/// other reader tolerates the empty shape and one null dropped whole nodes
/// out of scans (misrouted PR mail, skipped merge closes).
pub(crate) const NULL_COERCED_LISTS: &[&str] = &["additional_prs", "cost_sessions", "sessions"];

/// Project one graph row's sidecar fields with the Python `Sidecar` model's
/// semantics. `None` is an invalid projection (any present sidecar key with
/// the wrong type - the pydantic ValidationError case): single reads degrade
/// to the empty sidecar, whole-store scans skip the row. Null scalars drop;
/// the three list fields coerce null to `[]`.
pub(crate) fn project_row(row: &Value) -> Option<Map<String, Value>> {
    let obj = row.as_object()?;
    let mut out = Map::new();
    for key in SIDECAR_KEYS {
        let Some(v) = obj.get(*key) else {
            continue;
        };
        if v.is_null() {
            if NULL_COERCED_LISTS.contains(key) {
                out.insert((*key).to_string(), json!([]));
            }
            continue;
        }
        let valid = match *key {
            "pr_number" => matches!(v, Value::Number(n) if n.is_i64() || n.is_u64()),
            "cost_usd" => v.is_number(),
            "additional_prs" | "cost_sessions" | "sessions" => v.is_array(),
            _ => v.is_string(),
        };
        if !valid {
            return None;
        }
        out.insert((*key).to_string(), v.clone());
    }
    Some(out)
}

/// Finish a projection into the served shape: `id` always present, null
/// values dropped, the list fields defaulted to `[]`. A `None` projection
/// (the invalid case) lands here as the empty shape - `id` plus the list
/// defaults. This is the envelope the `sidecar-get` and `sidecar-all` ops
/// answer with.
pub(crate) fn served_shape(id: &str, fields: Option<Map<String, Value>>) -> Map<String, Value> {
    let mut out = fields.unwrap_or_else(|| Map::new());
    out.insert("id".into(), json!(id));
    for key in NULL_COERCED_LISTS {
        out.entry((*key).to_string()).or_insert_with(|| json!([]));
    }
    out
}

/// Decode one sidecar filename stem back to its id: the inverse of
/// [`crate::claims::encode_key`] (`%XX`, uppercase hex). A stem with a
/// malformed escape decodes byte-for-byte - the scan that reads the stems
/// never has an invalid file name to skip on.
pub(crate) fn decode_key(stem: &str) -> String {
    let bytes = stem.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Every per-id sidecar file in `root`, decoded back to ids, corrupt files
/// skipped (a file that is not a sidecar is not a row).
pub(crate) fn load_all_files(root: &Path) -> Vec<(String, Map<String, Value>)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for path in names {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(obj) = v.as_object() {
            out.push((decode_key(stem), obj.clone()));
        }
    }
    out
}

/// The write-side partition gate: a key outside `SIDECAR_KEYS` (other than
/// the `id` join key) refuses before any write - the tracker owns it. Both
/// write arms (the per-id file and the graph row merge) run it.
pub(crate) fn gate_fields(fields: &Map<String, Value>) -> Result<(), TrackerError> {
    if let Some(bad) = fields
        .keys()
        .find(|k| k.as_str() != "id" && !SIDECAR_KEYS.contains(&k.as_str()))
    {
        return Err(TrackerError::Refused(format!(
            "{bad:?} is not a sidecar key; the tracker owns it. Sidecar keys: {}",
            SIDECAR_KEYS.join(", ")
        )));
    }
    Ok(())
}

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
    gate_fields(fields)?;
    let path = root.join(format!("{}.json", encode_key(id)));
    let mut merged = read_file(&path, id)?;
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
    Ok(path)
}
