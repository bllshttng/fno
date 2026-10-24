//! Activation projects packaged roles into the plugin role layer, granting
//! nothing.
//!
//! Each packaged role lands as a full `RoleDefinitionSource` document at
//! `<role root>/plugin/<pack id>/<role>.json`, the layer discovery already
//! walks, so the role resolves through the untouched resolver. Activation
//! never writes `config.approvals.authorized_principals`, never mints a
//! capability fact and never touches the effect journal.
//!
//! Pack and role ids become path segments, so each is checked as one safe
//! component and every target is proven to stay under the plugin root before
//! any write. Verification runs against the locked registry snapshot,
//! ownership is checked first, definitions stage to temp files and swap in
//! only once every stage succeeded, and the registry commit rides the same
//! lock. Any later failure rolls the whole layer back. Deactivation removes
//! only paths its receipt recorded, and keeps a residual receipt for any path
//! it could not remove, so that path keeps an owner.

use super::manifest::PackManifest;
use super::registry::{conformance_for, Corrupt, Receipt, Store};
use super::verify::{load_manifest, manifest_file, verify_manifest};
use super::{contained_under, is_safe_component, random_hex, resolve_path};
use crate::client_commands::py_repr_str;
use crate::roles::models::{DefinitionSource, RoleLayer};
use std::path::{Path, PathBuf};

/// An artifact a role requires that the project has not configured. The
/// pack's own asset by that id is named as a worked example. This is a line
/// of self-teaching text, never a gate: resolution is what blocks.
pub struct Unconfigured {
    pub identifier: String,
    pub example_source: Option<String>,
}

/// Required artifact identifiers, in first-seen order, that `configured`
/// lacks. Names no pack id and no function name.
pub fn unconfigured_required_artifacts(
    manifest: &PackManifest,
    configured: &[String],
) -> Vec<Unconfigured> {
    let mut required: Vec<&str> = Vec::new();
    for role in &manifest.roles {
        for selector in &role.context_selectors {
            if let Some(identifier) = &selector.identifier {
                if selector.kind == "artifact" && !required.contains(&identifier.as_str()) {
                    required.push(identifier);
                }
            }
        }
    }
    required
        .into_iter()
        .filter(|identifier| !configured.iter().any(|c| c.as_str() == *identifier))
        .map(|identifier| Unconfigured {
            identifier: identifier.to_string(),
            example_source: manifest
                .assets
                .iter()
                .rev()
                .find(|a| a.id == identifier)
                .map(|a| a.source.clone()),
        })
        .collect()
}

/// A refusal: the reason word and its detail.
pub struct Refusal {
    pub reason: &'static str,
    pub detail: String,
}

/// Activation fails by refusal, or by an unexpected error mid-commit.
pub enum Failure {
    Refused(Refusal),
    Error(String),
}

fn refuse(reason: &'static str, detail: impl Into<String>) -> Failure {
    Failure::Refused(Refusal {
        reason,
        detail: detail.into(),
    })
}

fn corrupt(error: Corrupt) -> Failure {
    refuse("registry_corrupt", error.0)
}

pub struct Activation {
    pub receipt: Receipt,
    pub already_active: bool,
    pub unconfigured: Vec<Unconfigured>,
}

struct Planned {
    source_id: String,
    target: PathBuf,
    document: String,
}

/// Every target moved aside (with its backup) or newly created, kept so a
/// later failure can restore the layer.
#[derive(Default)]
struct Swap {
    backups: Vec<(PathBuf, PathBuf)>,
    fresh: Vec<PathBuf>,
    stale: Vec<(PathBuf, PathBuf)>,
}

fn restore(pairs: &[(PathBuf, PathBuf)]) {
    for (original, backup) in pairs {
        if original.exists() {
            let _ = std::fs::remove_file(original);
        }
        if backup.exists() {
            let _ = std::fs::rename(backup, original);
        }
    }
}

impl Swap {
    fn rollback(&self) {
        restore(&self.backups);
        for target in &self.fresh {
            let _ = std::fs::remove_file(target);
        }
        restore(&self.stale);
    }

    fn discard_backups(&self) {
        for (_, backup) in self.backups.iter().chain(&self.stale) {
            let _ = std::fs::remove_file(backup);
        }
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut text = path.as_os_str().to_os_string();
    text.push(suffix);
    PathBuf::from(text)
}

/// Stage every definition to a temp file beside its target, then swap them
/// all into place. A backup name carries a per-activation token, so one left
/// by a crashed run is never clobbered.
fn stage_and_swap(planned: &[Planned], token: &str) -> Result<Swap, Failure> {
    let mut staged: Vec<(PathBuf, tempfile::TempPath)> = Vec::new();
    for plan in planned {
        let stage = (|| -> std::io::Result<tempfile::TempPath> {
            let dir = plan.target.parent().unwrap_or(Path::new("."));
            std::fs::create_dir_all(dir)?;
            let stem = plan
                .target
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut file = tempfile::Builder::new()
                .prefix(&format!(".{stem}-stage-"))
                .suffix(".tmp")
                .tempfile_in(dir)?;
            std::io::Write::write_all(&mut file, plan.document.as_bytes())?;
            Ok(file.into_temp_path())
        })();
        match stage {
            Ok(path) => staged.push((plan.target.clone(), path)),
            Err(e) => {
                return Err(refuse(
                    "unwritable_layer",
                    format!("cannot stage a definition file: {e}"),
                ))
            }
        }
    }
    let mut swap = Swap::default();
    for (target, tmp) in staged {
        let result = (|| -> std::io::Result<()> {
            let had_prior = target.exists();
            if had_prior {
                let backup = with_suffix(&target, &format!(".bak-{token}"));
                std::fs::rename(&target, &backup)?;
                swap.backups.push((target.clone(), backup));
            }
            tmp.persist(&target).map_err(|e| e.error)?;
            if !had_prior {
                swap.fresh.push(target.clone());
            }
            Ok(())
        })();
        if let Err(e) = result {
            // The remaining staged temp files drop (and unlink) here.
            swap.rollback();
            return Err(refuse(
                "unwritable_layer",
                format!("cannot commit a definition file: {e}"),
            ));
        }
    }
    Ok(swap)
}

/// Verify, then project the pack's roles into the plugin role layer.
pub fn activate(
    path: &Path,
    store: &Store,
    root: &Path,
    configured: &[String],
) -> Result<Activation, Failure> {
    if root.is_symlink() {
        return Err(refuse(
            "invalid_identity",
            "role root is a symlink; refusing to project through it",
        ));
    }
    let base = if path.is_file() {
        path.parent().unwrap_or(Path::new("")).to_path_buf()
    } else {
        path.to_path_buf()
    };
    let manifest = load_manifest(path).map_err(|failure| {
        refuse(
            "verification_failed",
            format!("{}: {}", failure.result, failure.detail.unwrap_or_default()),
        )
    })?;
    let digest = manifest.digest();
    let component = |value: &str, kind: &str| -> Result<(), Failure> {
        if is_safe_component(value) {
            Ok(())
        } else {
            Err(refuse(
                "invalid_identity",
                format!(
                    "invalid {kind} {}: must be a single path component",
                    py_repr_str(value)
                ),
            ))
        }
    };
    component(&manifest.id, "pack id")?;
    for role in &manifest.roles {
        component(&role.role.id, "role id")?;
    }
    let revision = format!("pack:{}", &digest[..16]);
    let plugin_root = root.join("plugin");
    if plugin_root.is_symlink() || (plugin_root.exists() && !plugin_root.is_dir()) {
        return Err(refuse(
            "invalid_identity",
            "plugin role root is a symlink or not a directory",
        ));
    }
    // Prove every target stays under the plugin root before any write.
    let mut planned = Vec::new();
    for role in &manifest.roles {
        let source_id = format!("plugin/{}/{}.json", manifest.id, role.role.id);
        if !contained_under(root, &source_id, &plugin_root) {
            return Err(refuse(
                "invalid_identity",
                format!("{source_id} resolves outside the plugin role root"),
            ));
        }
        let definition = DefinitionSource {
            layer: RoleLayer::Plugin,
            source_id: source_id.clone(),
            snapshot_revision: revision.clone(),
            role: role.role.clone(),
            manifest: Some(role.clone()),
            status: "valid",
            error: None,
        };
        planned.push(Planned {
            target: root.join(&source_id),
            source_id,
            document: definition.to_json().to_string(),
        });
    }

    let _held = store
        .lock()
        .map_err(|e| Failure::Error(format!("cannot lock {}: {e}", store.path.display())))?;
    let mut registry = store.load().map_err(corrupt)?;
    let existing = registry.receipt_for(&manifest.id).cloned();
    let already_active = existing.as_ref().is_some_and(|r| r.pack_digest == digest);
    // Verify against the locked snapshot, so a concurrent update cannot move
    // a dependency between verify and commit.
    let installed: std::collections::BTreeMap<String, String> = registry
        .packs
        .iter()
        .map(|p| (p.pack_id.clone(), p.resolved_version.clone()))
        .collect();
    let report = verify_manifest(&manifest, &installed, &base);
    if !report.ok() {
        let failing: Vec<String> = report
            .conditions
            .iter()
            .filter(|c| !c.ok())
            .map(|c| format!("{}={}", c.name, c.result))
            .collect();
        return Err(refuse("verification_failed", failing.join("; ")));
    }
    for plan in &planned {
        let target = &plan.target;
        if target.is_symlink() || (target.exists() && !target.is_file()) {
            return Err(refuse(
                "path_occupied",
                format!("{} is a symlink or non-regular file", plan.source_id),
            ));
        }
        match registry.owner_of_path(&plan.source_id) {
            Some(owner) if owner != manifest.id => {
                return Err(refuse(
                    "different_pack_owns_path",
                    format!("{} is owned by pack {owner}", plan.source_id),
                ))
            }
            None if target.exists() || target.is_symlink() => {
                return Err(refuse(
                    "path_occupied",
                    format!("{} exists but no receipt owns it", plan.source_id),
                ))
            }
            _ => {}
        }
    }
    let new_set: Vec<&str> = planned.iter().map(|p| p.source_id.as_str()).collect();
    let prior_paths: Vec<String> = existing
        .as_ref()
        .map(|r| r.written_paths.clone())
        .unwrap_or_default();
    let stale_paths: Vec<&String> = prior_paths
        .iter()
        .filter(|p| !new_set.contains(&p.as_str()))
        .collect();
    // A prior receipt path escaping the plugin root means a tampered registry.
    for prior in &stale_paths {
        if !contained_under(root, prior, &plugin_root) {
            return Err(refuse(
                "registry_corrupt",
                format!(
                    "prior receipt path {} escapes the plugin root",
                    py_repr_str(prior)
                ),
            ));
        }
    }

    let mut swap = stage_and_swap(&planned, &random_hex(8))?;
    let commit = (|| -> Result<Receipt, Failure> {
        // Rename stale paths aside, never unlink, so a failed save restores
        // the whole layer.
        for prior in &stale_paths {
            let prior_path = root.join(prior);
            if prior_path.is_symlink() {
                return Err(refuse(
                    "unwritable_layer",
                    format!("stale path {prior} is a symlink"),
                ));
            }
            if prior_path.is_file() {
                let backup = with_suffix(&prior_path, &format!(".stale-{}.bak", random_hex(4)));
                std::fs::rename(&prior_path, &backup).map_err(|e| Failure::Error(e.to_string()))?;
                swap.stale.push((prior_path, backup));
            } else if prior_path.exists() {
                return Err(refuse(
                    "unwritable_layer",
                    format!("stale path {prior} is not a regular file"),
                ));
            }
        }
        let receipt = Receipt {
            pack_id: manifest.id.clone(),
            pack_digest: digest.clone(),
            resolved_version: manifest.version.clone(),
            written_paths: new_set.iter().map(|s| s.to_string()).collect(),
            activated_at: chrono::Utc::now().fixed_offset(),
            conformance: conformance_for(&manifest),
        };
        registry.install(&manifest, &resolve_path(&manifest_file(path)));
        registry.record_activation(receipt.clone());
        store.save(&registry).map_err(Failure::Error)?;
        Ok(receipt)
    })();
    match commit {
        Err(failure) => {
            swap.rollback();
            Err(failure)
        }
        Ok(receipt) => {
            swap.discard_backups();
            Ok(Activation {
                receipt,
                already_active,
                unconfigured: unconfigured_required_artifacts(&manifest, configured),
            })
        }
    }
}

pub struct Deactivation {
    pub removed: Vec<String>,
    pub left_alone: Vec<String>,
}

/// Remove only the definition paths this pack's receipt recorded, under one
/// registry lock. A path outside the plugin root, owned by another pack, or
/// a symlink or non-regular file is left alone and keeps a residual receipt.
pub fn deactivate(pack_id: &str, store: &Store, root: &Path) -> Result<Deactivation, String> {
    let nothing = || Deactivation {
        removed: Vec::new(),
        left_alone: Vec::new(),
    };
    if root.is_symlink() {
        return Ok(nothing());
    }
    let plugin_root = root.join("plugin");
    let _held = store
        .lock()
        .map_err(|e| format!("deactivation failed: cannot lock {}: {e}", store.path.display()))?;
    let mut registry = store
        .load()
        .map_err(|e| format!("registry corrupt; cannot deactivate safely: {}", e.0))?;
    let Some(receipt) = registry.receipt_for(pack_id).cloned() else {
        return Ok(nothing());
    };
    let token = random_hex(4);
    let mut outcome = nothing();
    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
    for source_id in &receipt.written_paths {
        if !contained_under(root, source_id, &plugin_root) {
            outcome.left_alone.push(source_id.clone());
            continue;
        }
        if registry
            .owner_of_path(source_id)
            .is_some_and(|owner| owner != pack_id)
        {
            outcome.left_alone.push(source_id.clone());
            continue;
        }
        let path = root.join(source_id);
        if path.is_symlink() || (path.exists() && !path.is_file()) {
            outcome.left_alone.push(source_id.clone());
            continue;
        }
        if !path.exists() {
            // Already absent: cleared from the receipt, neither removed nor
            // left behind.
            continue;
        }
        let backup = with_suffix(&path, &format!(".deact-{token}.bak"));
        match std::fs::rename(&path, &backup) {
            Ok(()) => {
                backups.push((path, backup));
                outcome.removed.push(source_id.clone());
            }
            Err(_) => outcome.left_alone.push(source_id.clone()),
        }
    }
    if outcome.left_alone.is_empty() {
        registry.receipts.retain(|r| r.pack_id != pack_id);
    } else {
        for r in registry
            .receipts
            .iter_mut()
            .filter(|r| r.pack_id == pack_id)
        {
            r.written_paths = outcome.left_alone.clone();
        }
    }
    if let Err(e) = store.save(&registry) {
        restore(&backups);
        return Err(format!("deactivation failed: {e}"));
    }
    for (_, backup) in &backups {
        let _ = std::fs::remove_file(backup);
    }
    Ok(outcome)
}
