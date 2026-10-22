//! Layered role-definition discovery, ported from `fno.roles.registry`.
//!
//! The root holds one directory per layer (`built-in`, `plugin`,
//! `company`, `project`, `plan`). Every `.json` file under a layer
//! directory is one source, and `--source layer=path` adds a file or a
//! directory outside the walk. Each source reads as valid, invalid or
//! unreadable, and an invalid one stays visible: it keeps its role when
//! the document still names one. Records sort by layer precedence, then
//! by source id.
//!
//! An OS failure keeps the wording Python printed for it
//! (`PermissionError: [Errno 13] Permission denied: '<path>'`), because
//! the `error` field is what an operator reads to fix the file.

use super::models::{self, RoleLayer, RoleRef};
use crate::client_commands::py_repr_str;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One discovery result, including invalid and unreadable sources.
#[derive(Clone, Debug)]
pub struct Discovered {
    pub layer: RoleLayer,
    pub source_id: String,
    pub status: &'static str,
    pub role: Option<RoleRef>,
    pub raw_definition: Option<Value>,
    /// The `RoleDefinitionSource` document the record carries: the raw
    /// document when it validated, or the invalid stand-in built from its
    /// role and revision.
    pub definition: Option<Value>,
    pub error: Option<String>,
}

/// The role root: `FNO_ROLES_ROOT`, else `<repo root>/.fno/roles`, where
/// the repo root is the project identity and never the bare cwd.
pub fn default_role_root() -> PathBuf {
    if let Some(configured) = std::env::var_os("FNO_ROLES_ROOT").filter(|v| !v.is_empty()) {
        return crate::plans_path::expanduser(&configured.to_string_lossy());
    }
    crate::backlog::style_check::repo_root()
        .join(".fno")
        .join("roles")
}

fn source_id(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// The exception class name, a colon and the message, as Python printed
/// the OSError it raised on the same path.
fn os_error_text(err: &io::Error, path: &Path) -> String {
    let Some(code) = err.raw_os_error() else {
        return format!("OSError: {err}");
    };
    let class = match code {
        2 => "FileNotFoundError",
        1 | 13 => "PermissionError",
        20 => "NotADirectoryError",
        21 => "IsADirectoryError",
        _ => "OSError",
    };
    let display = err.to_string();
    let suffix = format!(" (os error {code})");
    let message = display.strip_suffix(&suffix).unwrap_or(&display);
    format!(
        "{class}: [Errno {code}] {message}: {}",
        py_repr_str(&path.to_string_lossy())
    )
}

fn unreadable(layer: RoleLayer, path: &Path, root: &Path, err: &io::Error) -> Discovered {
    Discovered {
        layer,
        source_id: source_id(path, root),
        status: "unreadable",
        role: None,
        raw_definition: None,
        definition: None,
        error: Some(os_error_text(err, path)),
    }
}

fn bare_invalid(
    layer: RoleLayer,
    source_id: String,
    raw: Option<Value>,
    error: String,
) -> Discovered {
    Discovered {
        layer,
        source_id,
        status: "invalid",
        role: None,
        raw_definition: raw,
        definition: None,
        error: Some(error),
    }
}

fn role_value(role: &RoleRef) -> Value {
    json!({"id": role.id, "function_id": role.function_id})
}

/// A schema-invalid document keeps its role, and its stand-in definition
/// when it also names a revision.
fn invalid_document(layer: RoleLayer, source_id: String, raw: Value, error: String) -> Discovered {
    let role = raw
        .as_object()
        .and_then(|obj| obj.get("role"))
        .and_then(|value| models::role_ref(value, "RoleRef").ok());
    let revision = raw
        .get("snapshot_revision")
        .and_then(Value::as_str)
        .filter(|r| !r.trim().is_empty());
    let definition = match (&role, revision) {
        (Some(role), Some(revision)) => Some(json!({
            "layer": layer.as_str(),
            "source_id": source_id,
            "snapshot_revision": revision,
            "role": role_value(role),
            "status": "invalid",
            "error": error,
        })),
        _ => None,
    };
    Discovered {
        layer,
        source_id,
        status: "invalid",
        role,
        raw_definition: Some(raw),
        definition,
        error: Some(error),
    }
}

fn read_definition(path: &Path, layer: RoleLayer, root: &Path) -> Discovered {
    let id = source_id(path, root);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => return unreadable(layer, path, root, &err),
    };
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => return bare_invalid(layer, id, None, format!("UnicodeDecodeError: {err}")),
    };
    let raw: Value = match serde_json::from_str(&text) {
        Ok(raw) => raw,
        Err(err) => return bare_invalid(layer, id, None, format!("JSONDecodeError: {err}")),
    };
    let checked = models::definition_source(&raw).and_then(|definition| {
        if definition.layer != layer {
            return Err(format!(
                "document layer {} does not match discovery layer {}",
                py_repr_str(definition.layer.as_str()),
                py_repr_str(layer.as_str())
            ));
        }
        if definition.source_id != id {
            return Err(format!(
                "document source_id {} does not match source truth {}",
                py_repr_str(&definition.source_id),
                py_repr_str(&id)
            ));
        }
        Ok(definition)
    });
    match checked {
        Ok(definition) => Discovered {
            layer,
            source_id: id,
            status: definition.status,
            role: Some(definition.role),
            raw_definition: Some(raw.clone()),
            definition: Some(raw),
            error: definition.error,
        },
        Err(error) => invalid_document(layer, id, raw, error),
    }
}

/// `os.walk(top, followlinks=False)` with sorted names: every `.json`
/// entry that is not a directory, and an unreadable row per directory
/// that cannot be listed.
fn walk_json(
    dir: &Path,
    layer: RoleLayer,
    root: &Path,
    paths: &mut Vec<PathBuf>,
    errors: &mut Vec<Discovered>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            errors.push(unreadable(layer, dir, root, &err));
            return;
        }
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            || (entry.file_type().map(|t| t.is_symlink()).unwrap_or(false)
                && entry.path().is_dir());
        if is_dir {
            // A symlinked directory is listed as a directory and never
            // entered, the followlinks=False contract.
            if !entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                dirs.push(name);
            }
        } else if name.ends_with(".json") {
            files.push(name);
        }
    }
    files.sort();
    dirs.sort();
    paths.extend(files.into_iter().map(|name| dir.join(name)));
    for name in dirs {
        walk_json(&dir.join(name), layer, root, paths, errors);
    }
}

/// `--source` refused before any read: the spec is not `layer=path`, or
/// names no known layer.
#[derive(Debug)]
pub struct DiscoveryInputError(pub String);

fn explicit_paths(
    specs: &[String],
    root: &Path,
    errors: &mut Vec<Discovered>,
) -> Result<Vec<(RoleLayer, PathBuf)>, DiscoveryInputError> {
    let mut parsed = Vec::new();
    for spec in specs {
        let (layer_text, path_text) = match spec.split_once('=') {
            Some((layer, path)) if !path.is_empty() => (layer, path),
            _ => return Err(DiscoveryInputError("--source must use layer=path".into())),
        };
        let layer = RoleLayer::parse(layer_text).ok_or_else(|| {
            let choices: Vec<&str> = RoleLayer::ALL.iter().map(|l| l.as_str()).collect();
            DiscoveryInputError(format!(
                "unknown role layer {}; expected one of {}",
                py_repr_str(layer_text),
                choices.join(", ")
            ))
        })?;
        let path = crate::plans_path::expanduser(path_text);
        if path.is_dir() {
            let mut found = Vec::new();
            walk_json(&path, layer, root, &mut found, errors);
            parsed.extend(found.into_iter().map(|p| (layer, p)));
        } else {
            parsed.push((layer, path));
        }
    }
    Ok(parsed)
}

/// Discover every definition under `root` (default [`default_role_root`])
/// plus the explicit `--source` specs, sorted by layer then source id.
pub fn discover(
    root: Option<&Path>,
    specs: &[String],
) -> Result<Vec<Discovered>, DiscoveryInputError> {
    let root = match root {
        Some(root) => crate::plans_path::expanduser(&root.to_string_lossy()),
        None => default_role_root(),
    };
    let mut candidates: Vec<(RoleLayer, PathBuf)> = Vec::new();
    let mut errors = Vec::new();
    for layer in RoleLayer::ALL {
        let layer_root = root.join(layer.as_str());
        match fs::metadata(&layer_root) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => errors.push(unreadable(layer, &layer_root, &root, &err)),
            Ok(meta) if !meta.is_dir() => errors.push(bare_invalid(
                layer,
                source_id(&layer_root, &root),
                None,
                "role layer exists but is not a directory".into(),
            )),
            Ok(_) => {
                let mut found = Vec::new();
                walk_json(&layer_root, layer, &root, &mut found, &mut errors);
                candidates.extend(found.into_iter().map(|p| (layer, p)));
            }
        }
    }
    candidates.extend(explicit_paths(specs, &root, &mut errors)?);
    // One read per (layer, resolved path): the first sighting keeps its
    // place and the last spelling wins, as a Python dict assignment does.
    let mut order: Vec<(RoleLayer, PathBuf)> = Vec::new();
    let mut spelled: HashMap<(RoleLayer, PathBuf), PathBuf> = HashMap::new();
    for (layer, path) in candidates {
        let key = (
            layer,
            fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
        );
        if spelled.insert(key.clone(), path).is_none() {
            order.push(key);
        }
    }
    let mut records: Vec<Discovered> = order
        .iter()
        .map(|key| read_definition(&spelled[key], key.0, &root))
        .collect();
    records.extend(errors);
    records.sort_by(|a, b| (a.layer, &a.source_id).cmp(&(b.layer, &b.source_id)));
    Ok(records)
}

/// The record as the Python door reads it back.
fn record_value(record: &Discovered) -> Value {
    let mut row = Map::new();
    row.insert("layer".into(), json!(record.layer.as_str()));
    row.insert("source".into(), json!(record.source_id));
    row.insert("status".into(), json!(record.status));
    row.insert(
        "role".into(),
        record.role.as_ref().map_or(Value::Null, role_value),
    );
    row.insert(
        "raw_definition".into(),
        record.raw_definition.clone().unwrap_or(Value::Null),
    );
    row.insert(
        "definition".into(),
        record.definition.clone().unwrap_or(Value::Null),
    );
    row.insert("error".into(), json!(record.error));
    Value::Object(row)
}

/// `spawn-overlay` kind `roles-discover`: `{root?, sources?}` in, the
/// sorted records out, or `{input_error}` for a malformed `--source`.
pub fn discover_kind(payload: &Value) -> Result<Value, String> {
    let root = payload
        .get("root")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let specs: Vec<String> = payload
        .get("sources")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok(match discover(root.as_deref(), &specs) {
        Ok(records) => json!({"rows": records.iter().map(record_value).collect::<Vec<_>>()}),
        Err(DiscoveryInputError(message)) => json!({"input_error": message}),
    })
}
