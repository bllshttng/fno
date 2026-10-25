//! Installed-pack records, activation receipts and conformance attribution.
//!
//! The registry persists which packs are installed (id, version, digest,
//! declared effect ceiling) and which are activated: a receipt naming the
//! exact role-layer paths the activation wrote. That path list is what makes
//! deactivation surgical, leaving a hand-written definition in the same
//! layer alone. It also records which pack digest declared which adapter
//! conformance, so a false declaration stays traceable.
//!
//! The file is a dotfile at the role root, never inside the walked `plugin`
//! layer. Mutations take an flock on `<registry>.lock`, the same lock file
//! the retired Python leg's `filelock` used, and every write is atomic.

use super::is_safe_component;
use super::manifest::{self, Effect, PackManifest};
use crate::roles::models::{
    self, format_instant, lax_bool, object, only_fields, parse_instant, required_str, Instant,
};
use serde_json::{json, Value};
use std::fs::File;
use std::path::{Path, PathBuf};

/// The registry is present but cannot be trusted. It never reads as empty:
/// that would discard every ownership record and let activation overwrite.
#[derive(Debug)]
pub struct Corrupt(pub String);

#[derive(Clone, Debug)]
pub struct Attribution {
    pub adapter_id: String,
    pub adapter_version: String,
    pub pack_digest: String,
    pub remote_idempotency: bool,
    pub reconciliation: bool,
}

#[derive(Clone, Debug)]
pub struct Receipt {
    pub pack_id: String,
    pub pack_digest: String,
    pub resolved_version: String,
    pub written_paths: Vec<String>,
    pub activated_at: Instant,
    pub conformance: Vec<Attribution>,
}

#[derive(Clone, Debug)]
pub struct InstalledPack {
    pub pack_id: String,
    pub resolved_version: String,
    pub pack_digest: String,
    pub manifest_path: String,
    pub declared_effects: Vec<Effect>,
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub packs: Vec<InstalledPack>,
    pub receipts: Vec<Receipt>,
}

fn digest_field(obj: &models::Obj, key: &str, at: &str) -> Result<String, String> {
    match obj.get(key) {
        Some(Value::String(text))
            if text.len() == 64
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Ok(text.clone())
        }
        Some(_) => Err(format!(
            "{at}.{key}: string should match pattern '^[0-9a-f]{{64}}$'"
        )),
        None => Err(format!("{at}.{key}: field required")),
    }
}

fn list<'a>(obj: &'a models::Obj, key: &str, at: &str) -> Result<&'a [Value], String> {
    match obj.get(key) {
        None => Ok(&[][..]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(format!("{at}.{key}: input should be a valid tuple")),
    }
}

fn attribution(value: &Value, at: &str) -> Result<Attribution, String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &[
            "adapter_id",
            "adapter_version",
            "pack_digest",
            "remote_idempotency",
            "reconciliation",
        ],
        at,
    )?;
    Ok(Attribution {
        adapter_id: required_str(obj, "adapter_id", at)?,
        adapter_version: required_str(obj, "adapter_version", at)?,
        pack_digest: digest_field(obj, "pack_digest", at)?,
        remote_idempotency: lax_bool(obj, "remote_idempotency", false, at)?,
        reconciliation: lax_bool(obj, "reconciliation", false, at)?,
    })
}

fn receipt(value: &Value, at: &str) -> Result<Receipt, String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &[
            "pack_id",
            "pack_digest",
            "resolved_version",
            "written_paths",
            "activated_at",
            "conformance",
        ],
        at,
    )?;
    let paths_at = format!("{at}.written_paths");
    let written_paths = match obj.get("written_paths") {
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{paths_at}.{i}: input should be a valid string"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(format!("{paths_at}: input should be a valid tuple")),
        None => return Err(format!("{paths_at}: field required")),
    };
    let activated_at = match obj.get("activated_at") {
        Some(value) => parse_instant(value, &format!("{at}.activated_at"))?,
        None => return Err(format!("{at}.activated_at: field required")),
    };
    Ok(Receipt {
        pack_id: required_str(obj, "pack_id", at)?,
        pack_digest: digest_field(obj, "pack_digest", at)?,
        resolved_version: required_str(obj, "resolved_version", at)?,
        written_paths,
        activated_at,
        conformance: list(obj, "conformance", at)?
            .iter()
            .enumerate()
            .map(|(i, item)| attribution(item, &format!("{at}.conformance.{i}")))
            .collect::<Result<_, _>>()?,
    })
}

fn installed_pack(value: &Value, at: &str) -> Result<InstalledPack, String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &[
            "pack_id",
            "resolved_version",
            "pack_digest",
            "manifest_path",
            "declared_effects",
        ],
        at,
    )?;
    Ok(InstalledPack {
        pack_id: required_str(obj, "pack_id", at)?,
        resolved_version: required_str(obj, "resolved_version", at)?,
        pack_digest: digest_field(obj, "pack_digest", at)?,
        manifest_path: required_str(obj, "manifest_path", at)?,
        declared_effects: list(obj, "declared_effects", at)?
            .iter()
            .enumerate()
            .map(|(i, item)| manifest::effect(item, &format!("{at}.declared_effects.{i}")))
            .collect::<Result<_, _>>()?,
    })
}

impl Registry {
    /// `PackRegistry.model_validate(raw)`, ownership check included.
    pub fn parse(value: &Value) -> Result<Registry, String> {
        let at = "PackRegistry";
        let obj = object(value, at)?;
        only_fields(obj, &["packs", "receipts"], at)?;
        let registry = Registry {
            packs: list(obj, "packs", at)?
                .iter()
                .enumerate()
                .map(|(i, item)| installed_pack(item, &format!("{at}.packs.{i}")))
                .collect::<Result<_, _>>()?,
            receipts: list(obj, "receipts", at)?
                .iter()
                .enumerate()
                .map(|(i, item)| receipt(item, &format!("{at}.receipts.{i}")))
                .collect::<Result<_, _>>()?,
        };
        registry.check_ownership()?;
        Ok(registry)
    }

    /// Duplicate ids, a path owned twice, or a receipt path outside its own
    /// pack's namespace make first-match ownership ambiguous: corrupt.
    fn check_ownership(&self) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        if let Some(pack) = self.packs.iter().find(|p| !ids.insert(&p.pack_id)) {
            return Err(format!(
                "duplicate installed pack id {}",
                crate::client_commands::py_repr_str(&pack.pack_id)
            ));
        }
        let mut ids = std::collections::HashSet::new();
        if let Some(receipt) = self.receipts.iter().find(|r| !ids.insert(&r.pack_id)) {
            return Err(format!(
                "duplicate activation receipt for {}",
                crate::client_commands::py_repr_str(&receipt.pack_id)
            ));
        }
        let mut seen: std::collections::HashMap<&str, &str> = Default::default();
        for receipt in &self.receipts {
            if !is_safe_component(&receipt.pack_id) {
                return Err(format!(
                    "receipt pack_id {:?} is not a safe path component",
                    receipt.pack_id
                ));
            }
            let prefix = format!("plugin/{}/", receipt.pack_id);
            for path in &receipt.written_paths {
                let segments: Vec<&str> = path.split('/').collect();
                if path.starts_with('/') || segments.iter().any(|s| matches!(*s, "." | ".." | "")) {
                    return Err(format!("non-normalized receipt path {path:?}"));
                }
                let Some(role) = path
                    .strip_prefix(&prefix)
                    .and_then(|rest| rest.strip_suffix(".json"))
                else {
                    return Err(format!(
                        "receipt path {path:?} is not under {prefix} or not a .json"
                    ));
                };
                if !is_safe_component(role) {
                    return Err(format!("receipt path {path:?} has an unsafe role segment"));
                }
                if let Some(owner) = seen.insert(path.as_str(), receipt.pack_id.as_str()) {
                    return Err(format!(
                        "path {path:?} owned by receipts {owner:?} and {:?}",
                        receipt.pack_id
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "packs": self.packs.iter().map(|p| json!({
                "pack_id": p.pack_id,
                "resolved_version": p.resolved_version,
                "pack_digest": p.pack_digest,
                "manifest_path": p.manifest_path,
                "declared_effects": p.declared_effects.iter().map(Effect::to_json).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "receipts": self.receipts.iter().map(|r| json!({
                "pack_id": r.pack_id,
                "pack_digest": r.pack_digest,
                "resolved_version": r.resolved_version,
                "written_paths": r.written_paths,
                "activated_at": format_instant(&r.activated_at),
                "conformance": r.conformance.iter().map(|c| json!({
                    "adapter_id": c.adapter_id,
                    "adapter_version": c.adapter_version,
                    "pack_digest": c.pack_digest,
                    "remote_idempotency": c.remote_idempotency,
                    "reconciliation": c.reconciliation,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    pub fn pack_by_id(&self, pack_id: &str) -> Option<&InstalledPack> {
        self.packs.iter().find(|p| p.pack_id == pack_id)
    }

    pub fn receipt_for(&self, pack_id: &str) -> Option<&Receipt> {
        self.receipts.iter().find(|r| r.pack_id == pack_id)
    }

    /// The pack id whose receipt recorded this path.
    pub fn owner_of_path(&self, written_path: &str) -> Option<&str> {
        self.receipts
            .iter()
            .find(|r| r.written_paths.iter().any(|p| p == written_path))
            .map(|r| r.pack_id.as_str())
    }

    /// Record (or refresh) a pack as installed.
    pub fn install(&mut self, manifest: &PackManifest, manifest_path: &Path) {
        self.packs.retain(|p| p.pack_id != manifest.id);
        self.packs.push(InstalledPack {
            pack_id: manifest.id.clone(),
            resolved_version: manifest.version.clone(),
            pack_digest: manifest.digest(),
            manifest_path: manifest_path.to_string_lossy().into_owned(),
            declared_effects: manifest.permissions.clone(),
        });
    }

    pub fn record_activation(&mut self, receipt: Receipt) {
        self.receipts.retain(|r| r.pack_id != receipt.pack_id);
        self.receipts.push(receipt);
    }
}

/// Lock-guarded, atomically written persistence at one path.
pub struct Store {
    pub path: PathBuf,
}

/// The held flock; dropping the file releases it.
pub struct Held(#[allow(dead_code)] File);

impl Store {
    pub fn at_root(root: &Path) -> Store {
        Store {
            path: root.join(".pack-registry.json"),
        }
    }

    fn label(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    pub fn lock(&self) -> std::io::Result<Held> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut lock_path = self.path.clone().into_os_string();
        lock_path.push(".lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(PathBuf::from(lock_path))?;
        file.lock()?;
        Ok(Held(file))
    }

    /// A missing file is a fresh install; a present but unreadable,
    /// unparseable or schema-invalid one is corrupt.
    pub fn load(&self) -> Result<Registry, Corrupt> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
            Err(e) => {
                return Err(Corrupt(format!(
                    "registry at {} is unreadable: {e}",
                    self.label()
                )))
            }
        };
        let text = String::from_utf8(bytes)
            .map_err(|e| Corrupt(format!("registry at {} is unreadable: {e}", self.label())))?;
        let raw: Value = serde_json::from_str(&text).map_err(|e| {
            Corrupt(format!(
                "registry at {} is not valid JSON: {e}",
                self.label()
            ))
        })?;
        Registry::parse(&raw).map_err(|e| {
            Corrupt(format!(
                "registry at {} failed schema validation: {e}",
                self.label()
            ))
        })
    }

    /// pack id -> resolved version, for the dependency family. Fails closed
    /// on a corrupt registry.
    pub fn installed_index(&self) -> Result<std::collections::BTreeMap<String, String>, Corrupt> {
        Ok(self
            .load()?
            .packs
            .into_iter()
            .map(|p| (p.pack_id, p.resolved_version))
            .collect())
    }

    /// Re-check, then write `json.dumps(sort_keys=True, indent=2)` through a
    /// temp file and a rename. The caller holds the lock.
    pub fn save(&self, registry: &Registry) -> Result<(), String> {
        let value = registry.to_json();
        Registry::parse(&value)?;
        let payload =
            crate::roles::cli::python_json(&crate::backlog::decisions_cli::sort_keys(&value));
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let mut tmp = tempfile::Builder::new()
            .prefix(".pack-registry-")
            .suffix(".tmp")
            .tempfile_in(dir)
            .map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut tmp, payload.as_bytes()).map_err(|e| e.to_string())?;
        tmp.persist(&self.path).map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

/// A pack's adapter conformance declarations as attributable records.
pub fn conformance_for(manifest: &PackManifest) -> Vec<Attribution> {
    let digest = manifest.digest();
    manifest
        .adapters
        .iter()
        .map(|a| Attribution {
            adapter_id: a.conformance.adapter_id.clone(),
            adapter_version: a.conformance.adapter_version.clone(),
            pack_digest: digest.clone(),
            remote_idempotency: a.conformance.remote_idempotency,
            reconciliation: a.conformance.reconciliation,
        })
        .collect()
}
