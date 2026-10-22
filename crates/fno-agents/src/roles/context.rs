//! The context catalog: one honest `ContextReference` per entry of the
//! generic `[context.artifacts]` config map, ported from the Python
//! `fno.roles.context`. A readable file carries its real sha256 and byte
//! size; an unreadable one is `readable=false` with a reason naming the
//! path and the error class Python raised. The catalog mints no capability
//! and grants nothing: the resolver consults it.

use super::models::{self, ContextReference};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// One configured artifact, as `ArtifactConfig` validated it.
pub struct Artifact {
    pub identifier: String,
    pub path: String,
    pub sensitivity: String,
}

/// Python's `_deep_merge`: nested tables merge key by key, anything else
/// replaces, and a key already present keeps its place.
fn deep_merge(base: &mut Map<String, Value>, over: Map<String, Value>) {
    for (key, value) in over {
        match value {
            Value::Object(value) if matches!(base.get(&key), Some(Value::Object(_))) => {
                if let Some(Value::Object(existing)) = base.get_mut(&key) {
                    deep_merge(existing, value);
                }
            }
            value => {
                base.insert(key, value);
            }
        }
    }
}

/// `load_settings().context.artifacts`, read from `root`: the candidate
/// config files merged lowest tier first, a legacy `config` block lifted,
/// and each entry checked as `ArtifactConfig` checked it. A file that does
/// not parse is skipped, as the Python loader skips it. Entries keep the
/// file order, since serde_json here keeps insertion order.
pub fn configured_artifacts(root: &Path) -> Result<Vec<Artifact>, String> {
    let mut merged = Map::new();
    for path in crate::agents_config::config_candidates(root).iter().rev() {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Ok(Value::Object(layer)) = toml::from_str::<Value>(&text) {
            deep_merge(&mut merged, layer);
        }
    }
    if let Some(Value::Object(block)) = merged.remove("config") {
        deep_merge(&mut merged, block);
    }
    let artifacts = match merged.get("context") {
        None => return Ok(Vec::new()),
        Some(context) => match models::object(context, "context")?.get("artifacts") {
            None => return Ok(Vec::new()),
            Some(artifacts) => models::object(artifacts, "context.artifacts")?,
        },
    };
    artifacts
        .iter()
        .map(|(identifier, spec)| {
            let at = format!("context.artifacts.{identifier}");
            let spec = models::object(spec, &at)?;
            let path = match spec.get("path") {
                Some(Value::String(path)) => path.clone(),
                Some(_) => return Err(format!("{at}.path: input should be a valid string")),
                None => return Err(format!("{at}.path: field required")),
            };
            Ok(Artifact {
                identifier: identifier.clone(),
                path,
                sensitivity: models::choice(
                    spec,
                    "sensitivity",
                    &models::SENSITIVITY,
                    Some("internal"),
                    &at,
                )?,
            })
        })
        .collect()
}

/// A stable revision over the configured identifiers, paths and
/// sensitivities, never the file contents: it changes only when the
/// configuration does. Pass it to `resolve --snapshot`.
pub fn catalog_revision(artifacts: &[Artifact]) -> String {
    let mut sorted: Vec<&Artifact> = artifacts.iter().collect();
    sorted.sort_by(|a, b| a.identifier.cmp(&b.identifier));
    let payload = sorted
        .iter()
        .map(|a| format!("{}:{}:{}", a.identifier, a.path, a.sensitivity))
        .collect::<Vec<_>>()
        .join("|");
    let digest: String = Sha256::digest(payload.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("context:{}", &digest[..16])
}

/// The root a relative artifact path is anchored to: Python's
/// `resolve_repo_root().resolve()`, never the process cwd.
pub fn repo_root() -> PathBuf {
    let root = crate::law_match::evidence_repo_root();
    std::fs::canonicalize(&root).unwrap_or(root)
}

/// `Path(raw).expanduser()` joined to `root` when relative. As pathlib
/// does, empty and `.` parts drop and `..` stays.
fn resolved_path(raw: &str, root: &Path) -> PathBuf {
    let expanded = match (raw.strip_prefix('~'), std::env::var("HOME")) {
        (Some(rest), Ok(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.trim_end_matches('/'))
        }
        _ => raw.to_string(),
    };
    let mut out = if expanded.starts_with('/') {
        PathBuf::from("/")
    } else {
        root.to_path_buf()
    };
    for part in expanded.split('/').filter(|p| !p.is_empty() && *p != ".") {
        out.push(part);
    }
    out
}

/// The Python exception class `read_bytes` raised for this error.
fn error_class(error: &std::io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EACCES | libc::EPERM) => "PermissionError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        _ => "OSError",
    }
}

/// One reference per artifact, in configured order, each stamped with
/// `snapshot_revision`: the resolver refuses a reference whose revision
/// differs from the one it resolves against.
pub fn build_catalog(
    artifacts: &[Artifact],
    snapshot_revision: &str,
    root: &Path,
) -> Vec<ContextReference> {
    artifacts
        .iter()
        .map(|artifact| {
            let resolved = resolved_path(&artifact.path, root);
            let provenance = resolved.to_string_lossy().into_owned();
            let unread = ContextReference {
                kind: "artifact".into(),
                identifier: artifact.identifier.clone(),
                provenance: provenance.clone(),
                work_order_scope: None,
                content_digest: None,
                content_revision: None,
                snapshot_revision: snapshot_revision.into(),
                fresh_until: None,
                sensitivity: artifact.sensitivity.clone(),
                byte_size: 0,
                readable: false,
                unavailable_reason: None,
            };
            match std::fs::read(&resolved) {
                Ok(data) => {
                    let digest: String = Sha256::digest(&data)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    ContextReference {
                        content_digest: Some(digest.clone()),
                        content_revision: Some(digest),
                        byte_size: data.len() as u64,
                        readable: true,
                        ..unread
                    }
                }
                // Never fabricate the bytes or a digest of a file not read.
                Err(error) => ContextReference {
                    unavailable_reason: Some(format!("{provenance}: {}", error_class(&error))),
                    ..unread
                },
            }
        })
        .collect()
}
