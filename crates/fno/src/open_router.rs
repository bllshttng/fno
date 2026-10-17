//! Where does a clicked file path open?
//!
//! `mux.open_with` picks the target: `inmux` (the default), `obsidian` or
//! `system`. [`target`] is pure and takes the path and the config, so tests
//! assert the target and only [`open_file`] spawns an opener. The in-mux
//! viewer has not shipped, so `inmux` degrades to `system`: the default
//! changes nothing on the day this router lands.

use std::path::{Path, PathBuf};

use crate::digest_overlay::{config_str, ObsidianCfg};

/// The `mux.open_with` setting. An absent or unrecognised value reads as the
/// default, `Inmux`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenWith {
    #[default]
    Inmux,
    Obsidian,
    System,
}

impl OpenWith {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|r| r.trim().to_ascii_lowercase()).as_deref() {
            Some("obsidian") => OpenWith::Obsidian,
            Some("system") => OpenWith::System,
            _ => OpenWith::Inmux,
        }
    }

    pub fn read(cwd: &Path) -> Self {
        Self::parse(config_str(cwd, "mux", "open_with").as_deref())
    }
}

/// What the router decided to launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An `obsidian://open` URI fno built for a file under the vault root.
    Obsidian { uri: String },
    /// The platform opener, given the plain path.
    System(PathBuf),
}

/// Pick the target for `path`. `obsidian` needs `obsidian.enabled`, a vault,
/// and a path under that vault root; anything short of that falls back to
/// `system`.
pub fn target(path: &Path, open_with: OpenWith, obsidian: &ObsidianCfg) -> Target {
    if open_with == OpenWith::Obsidian && obsidian.enabled {
        let uri = obsidian
            .vault
            .as_deref()
            .filter(|v| !v.is_empty())
            .and_then(|vault| crate::link::vault_file_uri(path, vault));
        if let Some(uri) = uri {
            return Target::Obsidian { uri };
        }
    }
    Target::System(path.to_path_buf())
}

/// Resolve the target from config and open it. Blocking: call it off the UI
/// loop. `Err` carries one human line for the status notice.
pub fn open_file(path: &Path) -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let open_with = OpenWith::read(&cwd);
    match target(path, open_with, &ObsidianCfg::read(&cwd)) {
        Target::Obsidian { uri } => crate::link::open_fno_uri(&uri),
        Target::System(path) => crate::link::open_fno_path(&path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, vault: Option<&str>) -> ObsidianCfg {
        ObsidianCfg {
            enabled,
            vault: vault.map(String::from),
        }
    }

    const IN_VAULT: &str = "/opt/myvault/plans/a b.md";
    const OUTSIDE: &str = "/tmp/shot.png";

    #[test]
    fn parse_reads_the_three_values_and_defaults_the_rest() {
        assert_eq!(OpenWith::parse(Some("obsidian")), OpenWith::Obsidian);
        assert_eq!(OpenWith::parse(Some(" System ")), OpenWith::System);
        assert_eq!(OpenWith::parse(Some("inmux")), OpenWith::Inmux);
        assert_eq!(OpenWith::parse(Some("vim")), OpenWith::Inmux);
        assert_eq!(OpenWith::parse(None), OpenWith::Inmux);
    }

    #[test]
    fn inmux_and_system_open_the_plain_path_even_inside_the_vault() {
        let c = cfg(true, Some("/opt/myvault"));
        for mode in [OpenWith::Inmux, OpenWith::System] {
            assert_eq!(
                target(Path::new(IN_VAULT), mode, &c),
                Target::System(PathBuf::from(IN_VAULT))
            );
        }
    }

    #[test]
    fn obsidian_builds_a_uri_for_a_vault_file() {
        let t = target(
            Path::new(IN_VAULT),
            OpenWith::Obsidian,
            &cfg(true, Some("/opt/myvault")),
        );
        assert_eq!(
            t,
            Target::Obsidian {
                uri: "obsidian://open?vault=myvault&file=plans/a%20b.md".to_string()
            }
        );
    }

    #[test]
    fn obsidian_falls_back_to_system_when_it_cannot_serve_the_file() {
        let outside = (Path::new(OUTSIDE), cfg(true, Some("/opt/myvault")));
        let off = (Path::new(IN_VAULT), cfg(false, Some("/opt/myvault")));
        let no_vault = (Path::new(IN_VAULT), cfg(true, None));
        let empty_vault = (Path::new(IN_VAULT), cfg(true, Some("")));
        for (path, c) in [outside, off, no_vault, empty_vault] {
            assert_eq!(
                target(path, OpenWith::Obsidian, &c),
                Target::System(path.to_path_buf())
            );
        }
    }
}
