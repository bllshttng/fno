//! Function packs: versioned, verifiable capability as data, not code.
//!
//! A pack declares roles, skills, agents, workflows, adapters, evaluators,
//! assets, a maximum-expected effect ceiling, and benchmark scenarios.
//! Verification reports each condition on two independent axes (checked,
//! and a four-state result) before any activation. Activation projects the
//! packaged roles into the `plugin` layer the role registry already walks.
//! It grants no capability and no effect: it writes role definitions only.
//!
//! This module carries no branch on a pack id or a function name. A pack is
//! content under `plugins/`, discovered by path and validated by schema;
//! `scripts/ci/check-plugins-function-agnostic.sh` holds that line.
//!
//! The `fno config plugins` commands nest under the existing `spawn-overlay`
//! door as `fno-agents spawn-overlay plugins <command>`, so they add no
//! fno-agents root command.

pub mod activate;
pub mod cli;
pub mod manifest;
pub mod registry;
pub mod verify;
pub mod version;

/// An id becomes a path segment under `<root>/plugin/<pack>/<role>.json`, so
/// it must be one safe component.
pub(crate) fn is_safe_component(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && value != "."
        && value != ".."
        && !value.contains('\0')
}

/// `Path.resolve()` in its lenient form: symlinks resolve as far as the path
/// exists, and the missing tail joins lexically, `..` popping a component.
pub(crate) fn resolve_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    if let Ok(found) = std::fs::canonicalize(&absolute) {
        return found;
    }
    let mut components = absolute.components();
    let Some(last) = components.next_back() else {
        return absolute;
    };
    let parent = components.as_path();
    match last {
        Component::Normal(name) => resolve_path(parent).join(name),
        Component::ParentDir => {
            let base = resolve_path(parent);
            base.parent().map(PathBuf::from).unwrap_or(base)
        }
        Component::CurDir => resolve_path(parent),
        _ => absolute,
    }
}

/// `root/sub` resolves under `anchor`.
pub(crate) fn contained_under(root: &std::path::Path, sub: &str, anchor: &std::path::Path) -> bool {
    resolve_path(&root.join(sub)).starts_with(resolve_path(anchor))
}

/// `os.urandom(n).hex()`.
pub(crate) fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (nanos >> ((i % 16) * 8)) as u8 ^ (std::process::id() as u8);
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
