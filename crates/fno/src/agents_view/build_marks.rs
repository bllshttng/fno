//! The cargo build mark: the sideline row's compiling/waiting glyph rides
//! the `db/build-marks` sidecars the daemon arm (fno-agents build_marks.rs)
//! writes, stamped read-time onto rows by harness_session_id, exactly the
//! machine-armed hold mark's shape. Fail-untiring like `overlay_hold_marks`:
//! an unreadable or expired sidecar degrades to no mark, never a wrong one.
//! The mark is additive, orthogonal to the badge like `dnd`.

use super::{rfc3339_like_to_secs, RegistryAgent};
use crate::proto::{BuildMark, BuildStage};
use std::path::Path;

/// Stamp `build` onto rows the db/build-marks sidecars name. Fail-quiet:
/// an unreadable or expired sidecar degrades to no mark.
pub(crate) fn overlay_build_marks(rows: &mut [RegistryAgent], now_secs: u64) {
    overlay_build_marks_at(rows, &build_marks_dir(), now_secs)
}

fn build_marks_dir() -> std::path::PathBuf {
    // The same root resolution the daemon arm's marks_dir() uses (FNO_HOME
    // > $HOME/.fno), so the overlay reads the root the arm wrote. A global
    // config paths.state_dir root is not mirrored here; that install
    // degrades to no mark, never a wrong one (hold_marks.rs' acceptance).
    let root = match std::env::var_os("FNO_HOME") {
        Some(home) => std::path::PathBuf::from(home),
        None => std::env::var_os("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".fno"))
            .unwrap_or_else(|| std::path::PathBuf::from(".fno")),
    };
    crate::state_layout::place_dir(&root, "build-marks").unwrap_or_else(|| root.join("build-marks"))
}

pub(crate) fn overlay_build_marks_at(rows: &mut [RegistryAgent], dir: &Path, now_secs: u64) {
    for row in rows.iter_mut() {
        let Some(sid) = row.harness_session_id.as_deref() else {
            continue;
        };
        // mail_hold.rs identity_key: ses_ ids verbatim, else lowercased.
        let key = if sid.starts_with("ses_") {
            sid.to_string()
        } else {
            sid.to_lowercase()
        };
        let Ok(text) = std::fs::read_to_string(dir.join(format!("{key}.json"))) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        // The daemon refreshes the stamp each pass; a stamp older than the
        // TTL means a dead daemon pinned a frozen hourglass - age it out.
        let live = v
            .get("at")
            .and_then(|a| a.as_str())
            .and_then(rfc3339_like_to_secs)
            .is_some_and(|at| {
                now_secs.saturating_sub(at) * 1000
                    <= v.get("ttl_ms").and_then(|t| t.as_i64()).unwrap_or(30_000) as u64
            });
        if !live {
            continue;
        }
        let stage = match v.get("state").and_then(|s| s.as_str()) {
            Some("compiling") => BuildStage::Compiling,
            Some("waiting") => BuildStage::Waiting,
            _ => continue,
        };
        let position = v.get("position").and_then(|p| p.as_u64());
        row.build = Some(match stage {
            BuildStage::Compiling => BuildMark::compiling(),
            BuildStage::Waiting => BuildMark::waiting(position),
        });
    }
}
