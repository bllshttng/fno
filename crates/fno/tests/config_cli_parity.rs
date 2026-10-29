//! parity-stage: characterization
//! parity-oracle: fno.config_cli
//!
//! The config-cli family port, frozen against goldens captured from the
//! base-commit leg through `config_cli_driver.py` (the committed oracle).
//! Each case rebuilds its fixture through the driver's `--fixture` mode (the
//! case table there is the single source), runs the native door on the same
//! fixture through the freshly built `fno` binary, and compares the
//! normalized (stdout, stderr, exit) triple against the committed golden.
//! Folds: the fixture root path (in resolved, raw, and macOS `/private`
//! spellings) - the one divergence the whoami port also folded, since the
//! host owns it. The fixtures are all config.toml: the case table predates
//! the reader's yaml leg, and a yaml golden still needs one capture run of
//! the committed oracle. The cold-settings.yaml path is covered end-to-end
//! by the worktree-manager smoke tests, whose `wt_config` reader goes
//! through `fno config get` against a fresh HOME.
//!
//! With `FNO_CAPTURE_GOLDEN=1` the test re-freezes every golden from the
//! oracle (`--case` mode, which loads the base-commit leg from git) instead
//! of comparing, so the capture stays reproducible while the base commit
//! exists.

use std::path::{Path, PathBuf};
use std::process::Command;

const DRIVER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/config_cli_driver.py");
const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/config_cli");

/// One parity case: the golden stem, the argv the door answers, the case's
/// env stamps (`<FIXTURE>` folds to the case root), and the driver fixture
/// that backs it.
struct Case {
    name: &'static str,
    argv: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
    fixture: &'static str,
}

const STATE_PIN: &[(&str, &str)] = &[("FNO_STATE_DIR", "<FIXTURE>/state")];
const STATE_AGENTS_EVENTS: &[(&str, &str)] = &[
    ("FNO_STATE_DIR", "<FIXTURE>/state"),
    ("FNO_AGENTS_HOME", "<FIXTURE>/fake_home/.fno/agents"),
    ("FNO_EVENTS_PATH", "<FIXTURE>/state/events.jsonl"),
];
const FAKE_BIN: &[(&str, &str)] = &[("FNO_AGENTS_BIN", "<FIXTURE>/fake_bin/fno-agents")];
const NO_BIN: &[(&str, &str)] = &[("FNO_AGENTS_BIN", "/nonexistent/fno-agents")];
const ZAI_STAMP: &[(&str, &str)] = &[("FNO_ROUTE_PROVIDER", "zai")];

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "get_scalar",
            argv: &["config", "get", "review.max_rounds"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_default",
            argv: &["config", "get", "blueprint.max_prs_per_epic"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_descends",
            argv: &["config", "get", "loops.mystery.level"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_block",
            argv: &["config", "get", "review"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_unknown",
            argv: &["config", "get", "no.such.key"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_prefix",
            argv: &["config", "get", "config.review.max_rounds"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_alias",
            argv: &["config", "get", "providers.active"],
            env: &[],
            fixture: "providers_alias",
        },
        Case {
            name: "get_json",
            argv: &["config", "get", "review.max_rounds", "--json"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_layering",
            argv: &["config", "get", "review.max_rounds"],
            env: &[],
            fixture: "layering",
        },
        Case {
            name: "get_local_override",
            argv: &["config", "get", "project.id"],
            env: &[],
            fixture: "layering",
        },
        Case {
            name: "get_missing",
            argv: &["config", "get"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_extra",
            argv: &["config", "get", "a", "b"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_unknown_opt",
            argv: &["config", "get", "--nope"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "get_help",
            argv: &["config", "get", "--help"],
            env: &[],
            fixture: "scalar",
        },
        Case {
            name: "history_empty",
            argv: &["config", "history"],
            env: STATE_AGENTS_EVENTS,
            fixture: "empty_project",
        },
        Case {
            name: "history_rows",
            argv: &["config", "history"],
            env: STATE_AGENTS_EVENTS,
            fixture: "history",
        },
        Case {
            name: "history_json",
            argv: &["config", "history", "--json"],
            env: STATE_AGENTS_EVENTS,
            fixture: "history",
        },
        Case {
            name: "history_filter",
            argv: &["config", "history", "review.max_rounds"],
            env: STATE_AGENTS_EVENTS,
            fixture: "history",
        },
        Case {
            name: "history_help",
            argv: &["config", "history", "--help"],
            env: &[],
            fixture: "empty_project",
        },
        Case {
            name: "build_dir",
            argv: &["config", "build-dir"],
            env: STATE_PIN,
            fixture: "empty_project",
        },
        Case {
            name: "builddir_help",
            argv: &["config", "build-dir", "--help"],
            env: &[],
            fixture: "empty_project",
        },
        Case {
            name: "budget_nostamp",
            argv: &["config", "assert-subagent-budget", "--width", "4"],
            env: &[],
            fixture: "budget",
        },
        Case {
            name: "budget_within",
            argv: &["config", "assert-subagent-budget", "--width", "4"],
            env: ZAI_STAMP,
            fixture: "budget",
        },
        Case {
            name: "budget_exceeded",
            argv: &["config", "assert-subagent-budget", "--width", "9"],
            env: ZAI_STAMP,
            fixture: "budget",
        },
        Case {
            name: "budget_no_entry",
            argv: &["config", "assert-subagent-budget", "--width", "3"],
            env: &[("FNO_ROUTE_PROVIDER", "gemini")],
            fixture: "budget",
        },
        Case {
            name: "budget_badwidth",
            argv: &["config", "assert-subagent-budget", "--width", "abc"],
            env: ZAI_STAMP,
            fixture: "budget",
        },
        Case {
            name: "budget_help",
            argv: &["config", "assert-subagent-budget", "--help"],
            env: &[],
            fixture: "budget",
        },
        Case {
            name: "sinks_empty",
            argv: &["config", "status-sinks"],
            env: &[],
            fixture: "empty_project",
        },
        Case {
            name: "sinks_json",
            argv: &["config", "status-sinks", "--json"],
            env: &[],
            fixture: "work_map",
        },
        Case {
            name: "sinks_help",
            argv: &["config", "status-sinks", "--help"],
            env: &[],
            fixture: "work_map",
        },
        Case {
            name: "backlog_empty_object",
            argv: &["config", "active-backlog"],
            env: FAKE_BIN,
            fixture: "fake_agents_empty",
        },
        Case {
            name: "backlog_rows",
            argv: &["config", "active-backlog"],
            env: FAKE_BIN,
            fixture: "fake_agents_rows",
        },
        Case {
            name: "backlog_list_shape",
            argv: &["config", "active-backlog"],
            env: FAKE_BIN,
            fixture: "fake_agents_list",
        },
        Case {
            name: "backlog_help",
            argv: &["config", "active-backlog", "--help"],
            env: &[],
            fixture: "empty_project",
        },
        Case {
            name: "plugin_usage",
            argv: &["config", "plugin", "install"],
            env: NO_BIN,
            fixture: "empty_project",
        },
        Case {
            name: "plugin_help",
            argv: &["config", "plugin", "install", "--help"],
            env: &[],
            fixture: "empty_project",
        },
        Case {
            name: "plugin_claude_forward",
            argv: &["config", "plugin", "install", "claude"],
            env: FAKE_BIN,
            fixture: "fake_agents_list",
        },
        Case {
            name: "plugin_codex",
            argv: &["config", "plugin", "install", "codex"],
            env: FAKE_BIN,
            fixture: "fake_agents_list",
        },
    ]
}

/// The base env the driver's `scrubbed_env` floors to, then the case stamps,
/// then the `<FIXTURE>` fold.
fn case_env(root: &Path, stamps: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "HOME".to_string(),
            root.join("fake_home").to_string_lossy().to_string(),
        ),
        (
            "FNO_GLOBAL_SETTINGS_PATH".to_string(),
            root.join("fake_home/.fno/settings.yaml")
                .to_string_lossy()
                .to_string(),
        ),
        ("FNO_NO_CANONICAL_CONFIG".to_string(), "1".to_string()),
        (
            "FNO_STATE_DIR".to_string(),
            root.join("state").to_string_lossy().to_string(),
        ),
        (
            "FNO_AGENTS_HOME".to_string(),
            root.join("fake_home/.fno/agents")
                .to_string_lossy()
                .to_string(),
        ),
    ];
    // The spawned binary may write shims or scratch under the temp root; a
    // scrubbed TMPDIR would move its tempdir to /tmp and misjudge fixture
    // paths the OS temp root owns.
    for key in ["TMPDIR", "TEMPDIR"] {
        if let Ok(value) = std::env::var(key) {
            env.push((key.to_string(), value));
        }
    }
    for (key, value) in stamps {
        env.push((
            key.to_string(),
            value.replace("<FIXTURE>", &root.to_string_lossy()),
        ));
    }
    env
}

/// Fold what the host owns: the fixture root in resolved, raw, and macOS
/// `/private` spellings.
fn normalize(text: &str, root: &Path) -> String {
    let resolved = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .to_string();
    let raw = root.to_string_lossy().to_string();
    let mut folded = text.to_string();
    for token in [format!("/private{resolved}"), resolved, raw] {
        if !token.is_empty() {
            folded = folded.replace(&token, "<FIXTURE>");
        }
    }
    folded
}

fn unique_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fno-config-cli-parity-{}-{}",
        std::process::id(),
        name
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Build the fixture through the driver, the way the capture ran it.
fn build_fixture(case: &Case, root: &Path) {
    let out = Command::new("python3")
        .arg(DRIVER)
        .args(["--fixture", case.fixture])
        .arg(root)
        .output()
        .expect("python3 must exist to rebuild parity fixtures");
    assert!(
        out.status.success(),
        "fixture build failed for {}: {}",
        case.name,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One normalized (stdout, stderr, exit) triple from the freshly built `fno`
/// binary on the case fixture.
fn run_native(case: &Case, root: &Path) -> (String, String, i32) {
    build_fixture(case, root);
    let project = root.join("project");
    let mut command = Command::new(env!("CARGO_BIN_EXE_fno"));
    command
        .args(case.argv)
        .current_dir(&project)
        .env("PATH", std::env::var("PATH").unwrap_or_default());
    for (key, value) in case_env(root, case.env) {
        command.env(key, value);
    }
    // The oracle capture ran under the pytest-less CliRunner, which strips
    // nothing; the binary runs in the same cwd with the same env.
    let out = command.output().expect("the fno binary must run");
    (
        normalize(&String::from_utf8_lossy(&out.stdout), root),
        normalize(&String::from_utf8_lossy(&out.stderr), root),
        out.status.code().unwrap_or(-1),
    )
}

/// Re-freeze every golden from the base-commit oracle through the driver.
fn capture_goldens(case: &Case, root: &Path) {
    let out = Command::new("python3")
        .arg(DRIVER)
        .args(["--case", case.name])
        .arg(root)
        .output()
        .expect("python3 must exist to capture goldens");
    let stdout = normalize(&String::from_utf8_lossy(&out.stdout), root);
    let stderr = normalize(&String::from_utf8_lossy(&out.stderr), root);
    let exit = out.status.code().unwrap_or(-1);
    std::fs::write(format!("{GOLDEN_DIR}/{}.out", case.name), stdout).unwrap();
    std::fs::write(format!("{GOLDEN_DIR}/{}.err", case.name), stderr).unwrap();
    std::fs::write(
        format!("{GOLDEN_DIR}/{}.exit", case.name),
        format!("{exit}\n"),
    )
    .unwrap();
}

fn compare(case: &Case, root: &Path) {
    let (out, err, exit) = run_native(case, root);
    let golden_out = std::fs::read_to_string(format!("{GOLDEN_DIR}/{}.out", case.name))
        .unwrap_or_else(|_| panic!("golden {}.out missing", case.name));
    let golden_err = std::fs::read_to_string(format!("{GOLDEN_DIR}/{}.err", case.name))
        .unwrap_or_else(|_| panic!("golden {}.err missing", case.name));
    let golden_exit: i32 = std::fs::read_to_string(format!("{GOLDEN_DIR}/{}.exit", case.name))
        .expect("golden .exit missing")
        .trim()
        .parse()
        .expect("golden exit parses");
    assert_eq!(exit, golden_exit, "{}: exit code drifted", case.name);
    assert_eq!(out, golden_out, "{}: stdout drifted", case.name);
    assert_eq!(err, golden_err, "{}: stderr drifted", case.name);
    let _ = root;
}

#[test]
fn config_cli_family_matches_the_captured_leg() {
    let capture = std::env::var("FNO_CAPTURE_GOLDEN").as_deref() == Ok("1");
    std::fs::create_dir_all(GOLDEN_DIR).unwrap();
    for case in cases() {
        let root = unique_root(case.name);
        std::fs::create_dir_all(&root).unwrap();
        if capture {
            capture_goldens(&case, &root);
        } else {
            compare(&case, &root);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
