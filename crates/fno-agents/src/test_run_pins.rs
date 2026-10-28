//! State-root pins the cargo run door hands a test binary.
//!
//! A test binary in `crates/*/tests/` links this crate without `cfg(test)`,
//! so the [`crate::paths`] fences never fire in it, and a bare `cargo test`
//! resolves the live `~/.fno`. One integration test turned the live
//! `~/.fno/agents/registry.json` into a directory that way.
//! [`crate::live_store_fence`] now refuses the SQLite open, but every other
//! write (claims, mail, JSON state) still reached the live tree. So the run
//! door gives each test binary a fresh temp state tree, and refuses a pin that
//! points into the live one. The wrapper exports the pins before it execs the
//! binary, so the binary's own children inherit them too.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The verdict for one test binary.
#[derive(Debug, PartialEq)]
pub(crate) enum Pins {
    /// Export these, each relative to a fresh sandbox, before the binary
    /// runs. Empty when every root is already pinned outside the live tree.
    Set(Vec<(&'static str, &'static str)>),
    /// A root points into the live tree. The string is the refusal line.
    Refused(String),
}

/// Each root, and where it lands under the sandbox. The layout mirrors the
/// live one: the claims root is a home, the state dir its `.fno`, and the
/// agents home `.fno/agents`, whose parent the spaces root derives from.
const ROOTS: [(&str, &str); 3] = [
    ("FNO_CLAIMS_ROOT", ""),
    ("FNO_STATE_DIR", ".fno"),
    ("FNO_AGENTS_HOME", ".fno/agents"),
];

/// Decide the pins. `declared` is a set `FNO_TEST_HERMETIC`: `1` came through
/// `fno doctor test`, which already sandboxed `HOME`, and `0` is an ambient
/// run on purpose. Either way the door adds nothing. A set-but-empty root is
/// unset, as everywhere else in this crate.
pub(crate) fn decide(
    declared: bool,
    home: Option<&Path>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Pins {
    if declared {
        return Pins::Set(Vec::new());
    }
    let mut pins = Vec::new();
    for (key, rel) in ROOTS {
        match env(key).filter(|v| !v.is_empty()) {
            Some(value) => {
                if let Some(home) = home.filter(|h| is_live(h, Path::new(&value))) {
                    return Pins::Refused(format!(
                        "[cargo test] {key}={} points into the live fno home {}. A test \
                         binary must not touch live state: unset {key} and the run door \
                         pins a temp tree, or set FNO_TEST_HERMETIC=0 to run against live \
                         state on purpose.",
                        Path::new(&value).display(),
                        home.display()
                    ));
                }
            }
            None => pins.push((key, rel)),
        }
    }
    Pins::Set(pins)
}

/// Whether `root` is the home itself or anything under its `.fno`.
fn is_live(home: &Path, root: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (home, root) = (canon(home), canon(root));
    root == home || root.starts_with(home.join(".fno"))
}

/// A fresh sandbox under the temp dir, with its `.fno` made. The OS temp
/// sweep removes it later; a test binary cannot clean up after an exec.
pub(crate) fn make_sandbox() -> std::io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("fno-cargo-test-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(dir.join(".fno"))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_os_str().to_owned()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// A bare `cargo test` sets no root: every root lands in the sandbox, in
    /// the live layout, and none of them under the home.
    #[test]
    fn an_unpinned_test_binary_gets_every_root_in_the_sandbox() {
        let home = tempfile::TempDir::new().unwrap();
        let pins = decide(false, Some(home.path()), env_of(&[]));
        assert_eq!(
            pins,
            Pins::Set(vec![
                ("FNO_CLAIMS_ROOT", ""),
                ("FNO_STATE_DIR", ".fno"),
                ("FNO_AGENTS_HOME", ".fno/agents"),
            ])
        );
    }

    /// A root pinned into the live tree is refused, named, and the escape
    /// is in the line. A root pinned elsewhere is kept, not overwritten.
    #[test]
    fn a_root_pinned_into_the_live_home_is_refused_and_one_elsewhere_is_kept() {
        let home = tempfile::TempDir::new().unwrap();
        let live = home.path().join(".fno").join("agents");
        std::fs::create_dir_all(&live).unwrap();
        let elsewhere = tempfile::TempDir::new().unwrap();

        let refused = decide(
            false,
            Some(home.path()),
            env_of(&[("FNO_AGENTS_HOME", &live)]),
        );
        let Pins::Refused(line) = refused else {
            panic!("a live agents home must be refused, got {refused:?}");
        };
        assert!(line.contains("FNO_AGENTS_HOME"), "{line}");
        assert!(line.contains("FNO_TEST_HERMETIC=0"), "{line}");

        let claims_at_home = decide(
            false,
            Some(home.path()),
            env_of(&[("FNO_CLAIMS_ROOT", home.path())]),
        );
        assert!(
            matches!(claims_at_home, Pins::Refused(_)),
            "{claims_at_home:?}"
        );

        let kept = decide(
            false,
            Some(home.path()),
            env_of(&[("FNO_STATE_DIR", elsewhere.path())]),
        );
        let Pins::Set(pins) = kept else {
            panic!("a root outside the live tree is kept");
        };
        assert!(pins.iter().all(|(k, _)| *k != "FNO_STATE_DIR"), "{pins:?}");
    }

    /// A declared run (`fno doctor test`, or ambient on purpose) gets no
    /// pins and no refusal, even with a live root set.
    #[test]
    fn a_declared_run_gets_nothing() {
        let home = tempfile::TempDir::new().unwrap();
        let live = home.path().join(".fno");
        let pins = decide(true, Some(home.path()), env_of(&[("FNO_STATE_DIR", &live)]));
        assert_eq!(pins, Pins::Set(Vec::new()));
    }
}
