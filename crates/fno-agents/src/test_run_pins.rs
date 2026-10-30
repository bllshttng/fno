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
/// agents home `.fno/agents`. The spaces root is pinned too, so a test run
/// that sets its own `FNO_STATE_DIR` cannot move spaces off the sandbox.
const ROOTS: [(&str, &str); 4] = [
    ("FNO_CLAIMS_ROOT", ""),
    ("FNO_STATE_DIR", ".fno"),
    ("FNO_AGENTS_HOME", ".fno/agents"),
    ("FNO_SPACES_DIR", ".fno/spaces"),
];

/// Decide the pins from `FNO_TEST_HERMETIC`. `0` is an ambient run on
/// purpose, so the door adds nothing. `1` is a claim, not a proof: it skips
/// the pins only when `HOME` really sits under the temp dir, the sandbox
/// `fno doctor test` builds. A bare `FNO_TEST_HERMETIC=1 cargo test` keeps the
/// real `HOME` and is pinned like any other run. A set-but-empty root is
/// unset, as everywhere else in this crate.
pub(crate) fn decide(
    hermetic: Option<&str>,
    home: Option<&Path>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Pins {
    let sandboxed = || home.is_some_and(crate::paths::under_temp_dir);
    match hermetic {
        Some("0") => return Pins::Set(Vec::new()),
        Some("1") if sandboxed() => return Pins::Set(Vec::new()),
        _ => {}
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
        let pins = decide(None, Some(home.path()), env_of(&[]));
        assert_eq!(
            pins,
            Pins::Set(vec![
                ("FNO_CLAIMS_ROOT", ""),
                ("FNO_STATE_DIR", ".fno"),
                ("FNO_AGENTS_HOME", ".fno/agents"),
                ("FNO_SPACES_DIR", ".fno/spaces"),
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
            None,
            Some(home.path()),
            env_of(&[("FNO_AGENTS_HOME", &live)]),
        );
        let Pins::Refused(line) = refused else {
            panic!("a live agents home must be refused, got {refused:?}");
        };
        assert!(line.contains("FNO_AGENTS_HOME"), "{line}");
        assert!(line.contains("FNO_TEST_HERMETIC=0"), "{line}");

        let claims_at_home = decide(
            None,
            Some(home.path()),
            env_of(&[("FNO_CLAIMS_ROOT", home.path())]),
        );
        assert!(
            matches!(claims_at_home, Pins::Refused(_)),
            "{claims_at_home:?}"
        );

        let kept = decide(
            None,
            Some(home.path()),
            env_of(&[("FNO_STATE_DIR", elsewhere.path())]),
        );
        let Pins::Set(pins) = kept else {
            panic!("a root outside the live tree is kept");
        };
        assert!(pins.iter().all(|(k, _)| *k != "FNO_STATE_DIR"), "{pins:?}");
    }

    /// `0` gets no pins and no refusal, even with a live root set. `1` gets
    /// none only inside a real temp-dir `HOME`: a bare `FNO_TEST_HERMETIC=1`
    /// run on the operator's `HOME` is pinned like any other.
    #[test]
    fn only_an_ambient_or_proven_sandbox_run_skips_the_pins() {
        let home = tempfile::TempDir::new().unwrap();
        let live = home.path().join(".fno");
        let ambient = decide(
            Some("0"),
            Some(home.path()),
            env_of(&[("FNO_STATE_DIR", &live)]),
        );
        assert_eq!(ambient, Pins::Set(Vec::new()));

        let sandboxed = decide(Some("1"), Some(home.path()), env_of(&[]));
        assert_eq!(sandboxed, Pins::Set(Vec::new()));

        let real_home = Path::new("/nonexistent-operator-home");
        let Pins::Set(pins) = decide(Some("1"), Some(real_home), env_of(&[])) else {
            panic!("an unproven sandbox claim with no live root set must pin, not refuse");
        };
        assert_eq!(pins.len(), ROOTS.len(), "{pins:?}");
    }
}
