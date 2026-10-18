//! Where does a clicked file path open?
//!
//! `mux.open_with` picks the target: `inmux` (the default), `obsidian` or
//! `system`. [`target`] is pure and takes the path and the config, so tests
//! assert the target and only [`launch`] spawns an opener. `inmux` sends a
//! file the viewer can show to the client viewer (shaped by `mux.viewer`) and
//! every other file to the platform opener.

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

/// The `mux.viewer` setting. An absent or unrecognised value reads as the
/// default, `Panel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerShape {
    #[default]
    Panel,
    Modal,
}

impl ViewerShape {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|r| r.trim().to_ascii_lowercase()).as_deref() {
            Some("modal") => ViewerShape::Modal,
            _ => ViewerShape::Panel,
        }
    }

    pub fn read(cwd: &Path) -> Self {
        Self::parse(config_str(cwd, "mux", "viewer").as_deref())
    }
}

/// What the router decided to launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An `obsidian://open` URI fno built for a file under the vault root.
    Obsidian { uri: String },
    /// The platform opener, given the plain path.
    System(PathBuf),
    /// The client's own viewer, which the caller loads on the UI side.
    Viewer {
        path: PathBuf,
        line: Option<u32>,
        shape: ViewerShape,
    },
}

/// Extensions the viewer cannot show yet: other image formats, PDF, archives,
/// audio, video and office documents go to the platform opener under `inmux`.
/// PNG, JPEG, GIF and WebP show in the viewer.
const OUTSIDE_EXTS: &[&str] = &[
    "bmp", "tif", "tiff", "heic", "ico", "svg", "pdf", "zip", "gz", "tgz", "tar", "bz2", "xz",
    "7z", "dmg", "mp3", "wav", "flac", "m4a", "mp4", "mov", "mkv", "avi", "webm", "doc", "docx",
    "xls", "xlsx", "ppt", "pptx", "key", "pages", "numbers",
];

/// Whether `inmux` can show `path`: not a directory, and not one of the
/// [`OUTSIDE_EXTS`]. A file with no telling extension is judged by content in
/// the viewer, which says "no preview" for a binary.
fn viewable(path: &Path) -> bool {
    let outside = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| OUTSIDE_EXTS.contains(&e.to_ascii_lowercase().as_str()));
    !outside && !path.is_dir()
}

/// Pick the target for `path`. `obsidian` needs `obsidian.enabled`, a vault,
/// and a path under that vault root; anything short of that falls back to
/// `system`. `inmux` shows a [`viewable`] file in the viewer.
pub fn target(
    path: &Path,
    line: Option<u32>,
    open_with: OpenWith,
    obsidian: &ObsidianCfg,
    shape: ViewerShape,
) -> Target {
    if open_with == OpenWith::Inmux && viewable(path) {
        return Target::Viewer {
            path: path.to_path_buf(),
            line,
            shape,
        };
    }
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

/// Resolve the target from config. Blocking (config reads, one stat): call it
/// off the UI loop.
pub fn resolve(path: &Path, line: Option<u32>) -> Target {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    target(
        path,
        line,
        OpenWith::read(&cwd),
        &ObsidianCfg::read(&cwd),
        ViewerShape::read(&cwd),
    )
}

/// Run an outside target. Blocking: call it off the UI loop. `Err` carries
/// one human line for the status notice. A viewer target launches nothing
/// here: the client shows it.
pub fn launch(target: &Target) -> Result<(), String> {
    match target {
        Target::Obsidian { uri } => crate::link::open_fno_uri(uri),
        Target::System(path) => crate::link::open_fno_path(path),
        Target::Viewer { .. } => Ok(()),
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
    const OUTSIDE: &str = "/tmp/shot.heic";

    #[test]
    fn parse_reads_both_settings_and_defaults_the_rest() {
        assert_eq!(OpenWith::parse(Some("obsidian")), OpenWith::Obsidian);
        assert_eq!(OpenWith::parse(Some(" System ")), OpenWith::System);
        assert_eq!(OpenWith::parse(Some("inmux")), OpenWith::Inmux);
        assert_eq!(OpenWith::parse(Some("vim")), OpenWith::Inmux);
        assert_eq!(OpenWith::parse(None), OpenWith::Inmux);
        assert_eq!(ViewerShape::parse(Some(" Modal ")), ViewerShape::Modal);
        assert_eq!(ViewerShape::parse(Some("float")), ViewerShape::Panel);
        assert_eq!(ViewerShape::parse(None), ViewerShape::Panel);
    }

    fn route(path: &str, mode: OpenWith, c: &ObsidianCfg) -> Target {
        target(Path::new(path), Some(7), mode, c, ViewerShape::Modal)
    }

    #[test]
    fn only_inmux_shows_the_viewer_and_only_for_a_file_it_can_show() {
        let c = cfg(true, Some("/opt/myvault"));
        for shown in [IN_VAULT, "/tmp/shot.PNG", "/tmp/photo.jpeg"] {
            assert_eq!(
                route(shown, OpenWith::Inmux, &c),
                Target::Viewer {
                    path: PathBuf::from(shown),
                    line: Some(7),
                    shape: ViewerShape::Modal,
                },
                "{shown}"
            );
        }
        for outside in [OUTSIDE, "/tmp/Brief.PDF", "/tmp"] {
            assert_eq!(
                route(outside, OpenWith::Inmux, &c),
                Target::System(PathBuf::from(outside)),
                "{outside}"
            );
        }
        assert_eq!(
            route(IN_VAULT, OpenWith::System, &c),
            Target::System(PathBuf::from(IN_VAULT))
        );
    }

    #[test]
    fn obsidian_builds_a_uri_for_a_vault_file() {
        let t = route(
            IN_VAULT,
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
        let outside = (OUTSIDE, cfg(true, Some("/opt/myvault")));
        let off = (IN_VAULT, cfg(false, Some("/opt/myvault")));
        let no_vault = (IN_VAULT, cfg(true, None));
        let empty_vault = (IN_VAULT, cfg(true, Some("")));
        for (path, c) in [outside, off, no_vault, empty_vault] {
            assert_eq!(
                route(path, OpenWith::Obsidian, &c),
                Target::System(PathBuf::from(path))
            );
        }
    }
}
