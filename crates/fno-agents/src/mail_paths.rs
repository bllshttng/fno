//! The one place that reads the environment and the config cascade to answer
//! "where is the mail bus". The ladder itself is `mail::log::resolve_log_path`.

use std::path::{Path, PathBuf};

/// The bus log under `fno_dir`: `FNO_BUS_DIR`, then `FNO_INBOX_ROOT/.bus`, then
/// the `config.paths.bus_dir` override (tilde-expanded; `{vault}`/`{project}`
/// templates are not), else `<fno_dir>/bus/messages.jsonl`.
pub(crate) fn bus_log_path(fno_dir: &Path) -> PathBuf {
    let bus_dir_env = std::env::var("FNO_BUS_DIR").ok();
    let inbox_root_env = std::env::var("FNO_INBOX_ROOT").ok();
    let configured = std::env::current_dir().ok().and_then(|cwd| {
        crate::agents_config::config_lookup(&cwd, &["paths", "bus_dir"])
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|s| !s.trim().is_empty())
            .map(|dir| crate::cargo_build_dirs::expand_home(Path::new(&dir)))
    });
    crate::mail::log::resolve_log_path(
        fno_dir,
        bus_dir_env.as_deref(),
        inbox_root_env.as_deref(),
        configured.as_deref(),
    )
}

/// The bus DIRECTORY under `fno_dir`: `bus_log_path`'s parent. Callers pass
/// the state root (`agents_config::state_dir`), not the agents home, which
/// `FNO_STATE_DIR` does not move.
pub(crate) fn bus_dir_of(fno_dir: &Path) -> PathBuf {
    crate::mail::log::dir_of_log(&bus_log_path(fno_dir), fno_dir)
}

/// The live bus log for this process: the ladder over the agents home's parent.
pub(crate) fn live_bus_log() -> PathBuf {
    let home = crate::paths::AgentsHome::from_env();
    bus_log_path(home.root().parent().unwrap_or_else(|| home.root()))
}

/// The bus directory the mail leaves read: the state root when one resolves,
/// else the agents home's root.
pub(crate) fn state_bus_dir(home: &crate::paths::AgentsHome) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::agents_config::state_dir(&cwd).unwrap_or_else(|| home.root().to_path_buf());
    bus_dir_of(&root)
}

/// The same directory from the state root alone; `None` when no state root
/// resolves.
pub(crate) fn state_bus_dir_opt() -> Option<PathBuf> {
    crate::agents_config::state_dir(Path::new(".")).map(|dir| bus_dir_of(&dir))
}
