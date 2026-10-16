//! The launchd service facts the service steps and detection share: the
//! two labels in one place, the platform gate, and bounded `launchctl
//! list` reads. Nothing here writes; the install commands the steps run
//! own that.

use std::time::Duration;

/// The pr-watcher agent's launchd label, shared with detection so a
/// renamed label cannot desync the two readers.
pub(crate) const PR_WATCHER_LABEL: &str = "sh.fno.pr-watcher";

/// The groom agent's launchd label.
pub(crate) const GROOM_LABEL: &str = "sh.fno.groom";

/// Installer subprocess cap: an install may copy plists or compile, so it
/// gets far more than a detection probe before the poll loop kills it.
pub(crate) const INSTALL_CAP: Duration = Duration::from_secs(30);

/// The service rows are macOS launchd; anywhere else they report
/// needs_human with their command and load nothing.
pub(crate) fn macos() -> bool {
    cfg!(target_os = "macos")
}

/// `launchctl list` once, each line's label read off its last whitespace
/// column. `None` when the read fails or times out: the caller reports
/// rather than guessing.
pub(crate) fn loaded_labels() -> Option<Vec<String>> {
    const LOAD_CAP: Duration = Duration::from_secs(5);
    let out = crate::setup_detect::bounded_for(&["launchctl", "list"], LOAD_CAP).ok()?;
    Some(
        out.lines()
            .filter_map(|l| l.split_whitespace().last().map(String::from))
            .collect(),
    )
}

/// Whether one label is loaded. An unreadable `launchctl list` reads as
/// not loaded, so the step offers the install instead of claiming a
/// service runs when it cannot see.
pub(crate) fn is_loaded(label: &str) -> bool {
    loaded_labels()
        .map(|labels| labels.iter().any(|l| l == label))
        .unwrap_or(false)
}
