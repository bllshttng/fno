//! parity-stage: characterization
//! parity-oracle: fno.review_level.resolve_review_level
//!
//! Golden parity for the `review resolve-level` Rust port, frozen against
//! the live Python leaf before its deletion. The case table and fixture
//! builders below are byte-identical to the capture driver that froze
//! `tests/golden/review_level/`; keep them in lockstep when a case is
//! added. Case outputs embed the per-case base tempdir, normalized to
//! `<TMP>` exactly as the capture driver normalized it.

mod common;

use common::{assert_golden as assert_golden_common, Golden};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Every prefix the capture driver scrubbed before running the leaf. The
/// scrub runs per case, so the env-provider stamp case's FNO_ROUTE_PROVIDER
/// is gone again by the next case, exactly as under the subprocess driver.
const SCRUB_PREFIXES: [&str; 7] = [
    "FNO_",
    "CLAUDE_",
    "CODEX_",
    "ANTHROPIC_",
    "OPENAI_",
    "ZAI_",
    "GLM_",
];

/// Whole-environment snapshot restored on Drop, panic-safe: one case's pins
/// can never outlive the test into a sibling test in the same process.
struct EnvGuard {
    saved: Vec<(OsString, OsString)>,
}

impl EnvGuard {
    fn take_snapshot() -> Self {
        EnvGuard {
            saved: std::env::vars_os().collect(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let saved: HashSet<&OsString> = self.saved.iter().map(|(k, _)| k).collect();
        let live: Vec<OsString> = std::env::vars_os().map(|(k, _)| k).collect();
        for name in live {
            if !saved.contains(&name) {
                std::env::remove_var(&name);
            }
        }
        for (name, value) in &self.saved {
            std::env::set_var(name, value);
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git spawns");
    assert!(
        status.success(),
        "git {args:?} failed in {}",
        repo.display()
    );
}

fn make_repo(base: &Path) -> PathBuf {
    let repo = base.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@example.test"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "seed\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    repo
}

fn fixture_small(base: &Path) -> PathBuf {
    let repo = make_repo(base);
    git(&repo, &["branch", "origin/main"]);
    std::fs::write(repo.join("b.txt"), "1\n2\n").unwrap();
    std::fs::write(repo.join("c.txt"), "3\n4\n5\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "small"]);
    repo
}

fn fixture_large(base: &Path) -> PathBuf {
    let repo = make_repo(base);
    git(&repo, &["branch", "origin/main"]);
    for i in 0..70 {
        std::fs::write(repo.join(format!("f{i:02}.txt")), format!("{i}\n")).unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "large"]);
    repo
}

fn fixture_master_only(base: &Path) -> PathBuf {
    let repo = make_repo(base);
    git(&repo, &["branch", "origin/master"]);
    std::fs::write(repo.join("m.txt"), "1\n2\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "on master base"]);
    repo
}

fn fixture_unrelated(base: &Path) -> PathBuf {
    let repo = make_repo(base);
    git(&repo, &["checkout", "-q", "--orphan", "other"]);
    std::fs::write(repo.join("other.txt"), "unrelated\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "other root"]);
    git(&repo, &["branch", "origin/main"]);
    git(&repo, &["checkout", "-q", "main"]);
    repo
}

fn fixture_binary(base: &Path) -> PathBuf {
    let repo = make_repo(base);
    git(&repo, &["branch", "origin/main"]);
    let mut bytes = b"\x00PNG\r\n\x1a\n".to_vec();
    bytes.extend(0u8..=255);
    std::fs::write(repo.join("bin.dat"), &bytes).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "binary"]);
    repo
}

fn plain(base: &Path) -> PathBuf {
    let dir = base.join("plain");
    std::fs::create_dir(&dir).unwrap();
    dir
}

/// Which cwd a case resolves from. `Base` is the bare case dir itself (the
/// capture driver's default for explicit-level cases); `Plain` is an empty
/// non-git dir under it.
enum Fixture {
    Base,
    Plain,
    Small,
    Large,
    MasterOnly,
    Unrelated,
    Binary,
}

struct Case {
    label: &'static str,
    level: Option<&'static str>,
    provider: Option<&'static str>,
    env_provider: bool,
    fixture: Fixture,
    usage_error: bool,
}

/// The capture driver's defaults: an explicit level, no provider, cwd is
/// the bare case dir.
fn case(label: &'static str, level: Option<&'static str>) -> Case {
    Case {
        label,
        level,
        provider: None,
        env_provider: false,
        fixture: Fixture::Base,
        usage_error: false,
    }
}

#[test]
fn review_level_golden_parity() {
    let _guard = env_lock();
    let _env = EnvGuard::take_snapshot();
    let root = std::env::temp_dir().join(format!("review-level-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // The capture refused a root inside a repo (git discovers a repo by
    // walking up from cwd, which poisons the diff-sized cases); the parity
    // root inherits that invariant.
    let probe = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&root)
        .output()
        .expect("git spawns");
    assert!(
        !probe.status.success(),
        "parity root is inside a repo: {}",
        String::from_utf8_lossy(&probe.stdout)
    );

    let cases = vec![
        case("explicit-low", Some("low")),
        case("explicit-medium", Some("medium")),
        case("explicit-high", Some("high")),
        case("explicit-xhigh", Some("xhigh")),
        case("explicit-max", Some("max")),
        Case {
            provider: Some("zai"),
            ..case("explicit-max-provider-zai", Some("max"))
        },
        Case {
            provider: Some("openai"),
            ..case("explicit-high-provider-openai", Some("high"))
        },
        case("bogus-level", Some("bogus")),
        Case {
            provider: Some("gemini"),
            ..case("bogus-provider", Some("high"))
        },
        Case {
            env_provider: true,
            fixture: Fixture::Plain,
            ..case("env-provider-stamp", None)
        },
        case("padded-max-token", Some("  MAX  ")),
        Case {
            fixture: Fixture::Plain,
            ..case("outside-git", None)
        },
        Case {
            fixture: Fixture::Small,
            ..case("diff-small", None)
        },
        Case {
            fixture: Fixture::Large,
            ..case("diff-large", None)
        },
        Case {
            fixture: Fixture::MasterOnly,
            ..case("origin-master-only", None)
        },
        Case {
            fixture: Fixture::Unrelated,
            ..case("unrelated-histories", None)
        },
        Case {
            fixture: Fixture::Binary,
            ..case("binary-numstat-row", None)
        },
        Case {
            usage_error: true,
            ..case("unknown-flag", None)
        },
    ];

    for test_case in cases {
        let base = root.join(test_case.label);
        std::fs::create_dir(&base).unwrap();
        if test_case.usage_error {
            // The typer door rejects the argv before any resolution; the
            // Rust parser must refuse the same shape.
            assert!(
                fno_agents::review_level::parse_args(&["--bogus-flag".to_string()]).is_err(),
                "[{}] parser must refuse an unknown flag",
                test_case.label
            );
            assert_golden_common(
                "review_level",
                test_case.label,
                &Golden {
                    exit: Some(2),
                    streams: vec![String::new()],
                },
                None,
            );
            continue;
        }
        // Pin the case env exactly as the capture driver did: scrub the
        // seven prefixes, HOME and CODEX_HOME point at fresh empty dirs
        // under the case base, then the case's own stamp lands.
        let scrubbed: Vec<OsString> = std::env::vars_os()
            .map(|(k, _)| k)
            .filter(|k| {
                let text = k.to_string_lossy();
                SCRUB_PREFIXES.iter().any(|p| text.starts_with(p))
            })
            .collect();
        for name in scrubbed {
            std::env::remove_var(&name);
        }
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::create_dir_all(base.join("codex")).unwrap();
        std::env::set_var("HOME", base.join("home"));
        std::env::set_var("CODEX_HOME", base.join("codex"));
        if test_case.env_provider {
            std::env::set_var("FNO_ROUTE_PROVIDER", "zai");
        }
        let cwd = match test_case.fixture {
            Fixture::Base => base.clone(),
            Fixture::Plain => plain(&base),
            Fixture::Small => fixture_small(&base),
            Fixture::Large => fixture_large(&base),
            Fixture::MasterOnly => fixture_master_only(&base),
            Fixture::Unrelated => fixture_unrelated(&base),
            Fixture::Binary => fixture_binary(&base),
        };
        let out = fno_agents::review_level::resolve_level_stdout(
            test_case.level,
            test_case.provider,
            &cwd,
        );
        let normalized = out.replace(base.to_string_lossy().as_ref(), "<TMP>");
        assert_golden_common(
            "review_level",
            test_case.label,
            &Golden {
                exit: Some(0),
                streams: vec![normalized],
            },
            None,
        );
    }
}
