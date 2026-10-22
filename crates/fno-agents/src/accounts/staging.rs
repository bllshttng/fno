//! Credential staging: the per-account dir layout under the managed store
//! (`staging.py`). Each record gets `<root>/<id>/`; an oauth_dir record's
//! harness dir inside it is a symlink to its `credentials_source`.

use crate::accounts::records::Record;
use std::path::{Path, PathBuf};

/// `staging._default_providers_root`: the managed store, fail-open to
/// `~/.fno/providers` when no state dir resolves.
pub fn providers_root(cwd: &Path) -> PathBuf {
    crate::accounts::slot::store_root(cwd).unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".fno/providers")
    })
}

/// `staging._symlink_path`: claude reads `<id>/.claude`; every other harness
/// takes a HOME override, so its dir is `<id>/home/.<harness>`.
pub fn symlink_path(record: &Record, account_dir: &Path) -> PathBuf {
    if record.harness == "claude" {
        account_dir.join(".claude")
    } else {
        account_dir
            .join("home")
            .join(format!(".{}", record.harness))
    }
}

/// `staging.verify_staged`: an api_key record needs its marker dir; an
/// oauth_dir record needs its symlink with a live target.
pub fn verify_staged(record: &Record, root: &Path) -> bool {
    let account_dir = root.join(&record.id);
    if record.auth == "api_key" {
        return account_dir.is_dir();
    }
    let link = symlink_path(record, &account_dir);
    link.symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
        && link.exists()
}
