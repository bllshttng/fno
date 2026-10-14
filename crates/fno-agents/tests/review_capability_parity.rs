//! parity-stage: characterization
//! parity-oracle: fno.review_capability (through the review-level capability door shapes)
//!
//! Golden parity for the `review-level capability` port, frozen against the
//! live Python leg before wave 2 deletes it. Each case drives the REAL binary
//! with the same env pins, fixtures, and payload the capture driver
//! (`review_capability_golden_driver.py`) runs the Python leg under; the
//! goldens under `tests/golden/review_capability/` hold the leg's door bytes
//! with fixture paths normalized to `<TMP>`.
//!
//! Capture: `FNO_CAPTURE_GOLDEN=1` re-runs the driver per case (`--case`),
//! asserts Rust==Python, and re-freezes via the shared helper. Normal mode
//! (CI): the frozen golden is the contract and Python never runs. The case
//! table below is the lockstep twin of the driver's; add a case to BOTH or
//! neither. The github-apps probe cases run a stub `gh` on PATH so both legs
//! answer False / None / True without a network.

mod common;

use common::{assert_golden, capture_mode, python_executable, Golden};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The payload descriptor table: built-ins (`resolvable_reviewers(None)`)
/// union the crafted rows, built-ins winning, `dataclasses.asdict` shape with
/// `invocations` a dict or null. One string for every reviewers case; the
/// Python leg resolves against the same union.
const DESCRIPTORS: &str = r#"{
  "sigma": {"kind": "local-attestation", "requires": "retired", "invocation": "/fno:review", "asserts": "review-evidence", "invocations": null},
  "code-review": {"kind": "local-attestation", "requires": "none", "invocation": "/fno:review", "asserts": "review-evidence", "invocations": null},
  "declare": {"kind": "local-attestation", "requires": "none", "invocation": "/fno:review declare", "asserts": "self-cert", "invocations": null},
  "lane-review": {"kind": "local-attestation", "requires": "none", "invocation": "/fno:review", "asserts": "review-evidence", "invocations": {"claude": "/fno:review <level>", "codex": "$fno:review <level>"}},
  "human-approval": {"kind": "human", "requires": "operator", "invocation": "/ask-operator", "asserts": "review-evidence", "invocations": null},
  "skill-reviewer": {"kind": "harness-skill", "requires": "skill", "invocation": "/my-review-skill run", "asserts": "invocation", "invocations": null},
  "plugin-reviewer": {"kind": "harness-skill", "requires": "skill", "invocation": "fno:review-helper run", "asserts": "invocation", "invocations": null},
  "panel": {"kind": "external-cli", "requires": "subagent-dispatch", "invocation": "/fno:review", "asserts": "review-evidence", "invocations": null}
}"#;

/// What the door runs for one case.
enum Action {
    /// `capability reviewers`, payload fields pre-serialized.
    Reviewers {
        reviewers: &'static str,
        peers: &'static str,
        peer_identity: Option<&'static str>,
    },
    /// `capability github-apps`, payload fields pre-serialized.
    GithubApps {
        apps: &'static str,
        nudge: &'static str,
    },
    /// `capability invocation` flags.
    Invocation { flags: &'static [&'static str] },
    /// `capability skill-presence` flags.
    SkillPresence { flags: &'static [&'static str] },
}

#[derive(Default)]
enum Skills {
    #[default]
    None,
    /// `~/.claude/skills/my-review-skill/SKILL.md` exists.
    Found,
    /// `~/.claude/skills` exists, empty.
    Absent,
}

struct Case {
    label: &'static str,
    action: Action,
    markers: &'static [(&'static str, &'static str)],
    settings_yaml: Option<&'static str>,
    global_yaml: Option<&'static str>,
    skills: Skills,
    /// The stub `gh`'s search total; `"none"` fails the probe, None = no stub.
    gh_stub: Option<&'static str>,
}

impl Case {
    fn rev(label: &'static str, reviewers: &'static str) -> Case {
        Case {
            label,
            action: Action::Reviewers {
                reviewers,
                peers: "[]",
                peer_identity: None,
            },
            markers: &[],
            settings_yaml: None,
            global_yaml: None,
            skills: Skills::None,
            gh_stub: None,
        }
    }

    fn peers(
        label: &'static str,
        peers: &'static str,
        peer_identity: Option<&'static str>,
    ) -> Case {
        Case {
            action: Action::Reviewers {
                reviewers: "[]",
                peers,
                peer_identity,
            },
            ..Case::rev(label, "[]")
        }
    }

    fn marked(mut self, markers: &'static [(&'static str, &'static str)]) -> Case {
        self.markers = markers;
        self
    }

    fn skills(mut self, skills: Skills) -> Case {
        self.skills = skills;
        self
    }
}

const CLAUDE_MARK: &[(&str, &str)] = &[("CLAUDE_CODE_SESSION_ID", "s-claude-1")];
const CODEX_MARK: &[(&str, &str)] = &[("CODEX_THREAD_ID", "s-codex-1")];
const GEMINI_MARK: &[(&str, &str)] = &[("GEMINI_SESSION_ID", "s-gemini-1")];
const UNATTENDED: &[(&str, &str)] = &[("TARGET_UNATTENDED", "1")];

fn app(
    label: &'static str,
    apps: &'static str,
    nudge: &'static str,
    gh_stub: Option<&'static str>,
) -> Case {
    Case {
        label,
        action: Action::GithubApps { apps, nudge },
        markers: &[],
        settings_yaml: None,
        global_yaml: None,
        skills: Skills::None,
        gh_stub,
    }
}

fn inv(label: &'static str, flags: &'static [&'static str]) -> Case {
    Case {
        label,
        action: Action::Invocation { flags },
        markers: &[],
        settings_yaml: None,
        global_yaml: None,
        skills: Skills::None,
        gh_stub: None,
    }
}

fn skp(label: &'static str, flags: &'static [&'static str], skills: Skills) -> Case {
    Case {
        label,
        action: Action::SkillPresence { flags },
        markers: &[],
        settings_yaml: None,
        global_yaml: None,
        skills,
        gh_stub: None,
    }
}

fn cfg(
    label: &'static str,
    settings_yaml: &'static str,
    global_yaml: Option<&'static str>,
) -> Case {
    Case {
        label,
        action: Action::Reviewers {
            reviewers: "[]",
            peers: "[]",
            peer_identity: None,
        },
        markers: &[],
        settings_yaml: Some(settings_yaml),
        global_yaml,
        skills: Skills::None,
        gh_stub: None,
    }
}

fn cases() -> Vec<Case> {
    vec![
        // requires == "none": the scalar invocation, attended and unattended.
        Case::rev(
            "reviewers none scalar claude attended",
            r#"["code-review"]"#,
        )
        .marked(CLAUDE_MARK),
        Case::rev(
            "reviewers none scalar codex unattended",
            r#"["code-review", "declare"]"#,
        )
        .marked(&[("CODEX_THREAD_ID", "s-codex-1"), ("TARGET_UNATTENDED", "1")]),
        // invocations map: hit, known-harness miss, unknown harness.
        Case::rev("reviewers invocations claude hit", r#"["lane-review"]"#).marked(CLAUDE_MARK),
        Case::rev("reviewers invocations gemini miss", r#"["lane-review"]"#).marked(GEMINI_MARK),
        Case::rev("reviewers invocations unknown", r#"["lane-review"]"#),
        // operator: the attended/unattended fork.
        Case::rev("reviewers operator attended", r#"["human-approval"]"#).marked(CLAUDE_MARK),
        Case::rev("reviewers operator unattended", r#"["human-approval"]"#).marked(&[
            ("CLAUDE_CODE_SESSION_ID", "s-claude-1"),
            ("TARGET_UNATTENDED", "1"),
        ]),
        // skill: found, absent, plugin-qualified (all on claude).
        Case::rev("reviewers skill found", r#"["skill-reviewer"]"#)
            .marked(CLAUDE_MARK)
            .skills(Skills::Found),
        Case::rev("reviewers skill absent", r#"["skill-reviewer"]"#)
            .marked(CLAUDE_MARK)
            .skills(Skills::Absent),
        Case::rev("reviewers skill plugin qualified", r#"["plugin-reviewer"]"#).marked(CLAUDE_MARK),
        // subagent-dispatch across the four harness answers.
        Case::rev("reviewers subagent claude", r#"["panel"]"#).marked(CLAUDE_MARK),
        Case::rev("reviewers subagent codex", r#"["panel"]"#).marked(CODEX_MARK),
        Case::rev("reviewers subagent gemini", r#"["panel"]"#).marked(GEMINI_MARK),
        Case::rev("reviewers subagent unknown", r#"["panel"]"#),
        // retired, unknown name, and a refusal mixing remedy classes.
        Case::rev("reviewers retired sigma", r#"["sigma"]"#).marked(CLAUDE_MARK),
        Case::rev("reviewers unknown name", r#"["phantom"]"#).marked(CLAUDE_MARK),
        Case::rev(
            "reviewers mixed refusal",
            r#"["sigma", "panel", "phantom"]"#,
        )
        .marked(&[
            ("GEMINI_SESSION_ID", "s-gemini-1"),
            ("TARGET_UNATTENDED", "1"),
        ]),
        // local peers: cross-model, same family, unknown author, claude
        // route, and the identity-set shape that returns no verdicts.
        Case::peers("peers cross model", r#"["codex"]"#, None).marked(CLAUDE_MARK),
        Case::peers("peers same family", r#"["claude"]"#, None).marked(CLAUDE_MARK),
        Case::peers("peers unknown author", r#"["codex"]"#, None),
        Case::peers(
            "peers claude route",
            r#"[{"provider": "claude", "model": "zai,glm-5.3"}]"#,
            None,
        )
        .marked(CLAUDE_MARK),
        Case::peers("peers identity set", r#"["codex"]"#, Some("glm-peer")).marked(CLAUDE_MARK),
        // config.unattended.enabled: per-usable-value walk.
        cfg(
            "config unattended true",
            "unattended:\n  enabled: true\n",
            None,
        ),
        cfg(
            "config unattended yes",
            "unattended:\n  enabled: \"yes\"\n",
            None,
        ),
        cfg("config unattended int", "unattended:\n  enabled: 1\n", None),
        cfg(
            "config empty then global",
            "unattended:\n  enabled: \"\"\n",
            Some("unattended:\n  enabled: true\n"),
        ),
        cfg(
            "config block without enabled",
            "unattended:\n  mode: panel\n",
            None,
        ),
        // github_apps: recognized, nudge alias, and the three probe arms.
        app(
            "apps recognized",
            r#"["chatgpt-codex-connector"]"#,
            "[]",
            None,
        ),
        app(
            "apps nudge alias",
            r#"["my-codex"]"#,
            r#"["my-codex-bot"]"#,
            None,
        ),
        app(
            "apps typo probe false",
            r#"["chatgpt-codex-connector-typo"]"#,
            "[]",
            Some("0"),
        ),
        app(
            "apps probe none",
            r#"["never-acted-bot"]"#,
            "[]",
            Some("none"),
        ),
        app("apps probe true", r#"["quiet-reviewer"]"#, "[]", Some("3")),
        app(
            "apps mixed refusal",
            r#"["chatgpt-codex-connector", "chatgpt-codex-connector-typo"]"#,
            "[]",
            Some("0"),
        ),
        // invocation renders: bare, PR target, branch target, and the
        // validation refusals. No --root case: sizing would round-trip a
        // binary the leg reaches through verb_call.
        inv("invocation bare claude", &["--harness", "claude"]),
        inv(
            "invocation pr target",
            &[
                "--harness",
                "claude",
                "--pr-number",
                "4183",
                "--head-sha",
                "f4bbd762d65",
                "--base-branch",
                "origin/main",
            ],
        ),
        inv(
            "invocation branch target codex",
            &[
                "--harness",
                "codex",
                "--branch",
                "feature/x-b546",
                "--head-sha",
                "f4bbd762d65",
                "--base-branch",
                "main",
            ],
        ),
        inv(
            "invocation bad sha",
            &[
                "--harness",
                "claude",
                "--pr-number",
                "4183",
                "--head-sha",
                "nope",
                "--base-branch",
                "main",
            ],
        ),
        inv(
            "invocation both targets",
            &[
                "--pr-number",
                "4183",
                "--branch",
                "b",
                "--head-sha",
                "f4bbd762d65",
                "--base-branch",
                "main",
            ],
        ),
        inv("invocation missing pr fields", &["--pr-number", "4183"]),
        inv("invocation ambient unknown", &[]),
        // skill-presence door: the probe's outcomes with pinned roots.
        skp(
            "skill presence found",
            &["--name", "my-review-skill", "--harness", "claude"],
            Skills::Found,
        ),
        skp(
            "skill presence absent",
            &["--name", "no-such-skill", "--harness", "claude"],
            Skills::Absent,
        ),
        skp(
            "skill presence plugin",
            &["--name", "fno:code-review", "--harness", "claude"],
            Skills::None,
        ),
        skp(
            "skill presence codex",
            &["--name", "my-review-skill", "--harness", "codex"],
            Skills::None,
        ),
        skp(
            "skill presence no roots",
            &["--name", "my-review-skill", "--harness", "claude"],
            Skills::None,
        ),
    ]
}

// --- CASE TABLE END (lockstep with review_capability_golden_driver rev()/cfg()/app()/inv()/skp()) ---

/// Build one case's fixtures under `case_dir`; returns the child's cwd.
fn build_fixtures(case_dir: &Path, spec: &Case) -> PathBuf {
    std::fs::create_dir_all(case_dir.join("home")).unwrap();
    let repo = case_dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git_env = [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
    ];
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .envs(git_env)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git spawns");
        assert!(
            status.success(),
            "git {args:?} failed in {}",
            repo.display()
        );
    };
    git(&["init", "-q", "-b", "main"]);
    // `display_name` falls back to `git config user.name`, so the fixture
    // pins a name BOTH legs read back: the operator reason says "ask t".
    git(&["config", "user.email", "t@example.test"]);
    git(&["config", "user.name", "t"]);
    if let Some(text) = spec.settings_yaml {
        std::fs::create_dir(repo.join(".fno")).unwrap();
        std::fs::write(repo.join(".fno/settings.yaml"), text).unwrap();
    }
    if let Some(text) = spec.global_yaml {
        std::fs::write(case_dir.join("global-settings.yaml"), text).unwrap();
    }
    match spec.skills {
        Skills::Found => {
            let root = case_dir.join("home/.claude/skills");
            std::fs::create_dir_all(&root).unwrap();
            let skill = root.join("my-review-skill");
            std::fs::create_dir(&skill).unwrap();
            std::fs::write(skill.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        }
        Skills::Absent => {
            std::fs::create_dir_all(case_dir.join("home/.claude/skills")).unwrap();
        }
        Skills::None => {}
    }
    if let Some(total) = spec.gh_stub {
        let bindir = case_dir.join("bin");
        std::fs::create_dir(&bindir).unwrap();
        let stub = bindir.join("gh");
        std::fs::write(&stub, gh_stub_script(total)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&stub).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).unwrap();
    }
    repo
}

/// The stub `gh`: `repo view` answers a slug, then `api search` answers the
/// case's total; `"none"` exits 1 on the first call so the probe is
/// unanswerable. Deterministic, no network.
fn gh_stub_script(total: &str) -> String {
    if total == "none" {
        "#!/bin/sh\nexit 1\n".to_string()
    } else {
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"repo\" ]\nthen\n  echo \"org/repo\"\n  exit 0\nfi\necho \"{total}\"\n"
        )
    }
}

/// The child's env: `env_clear` + exactly the pins. The strongest scrub: no
/// machine marker can leak into `detect_session` or the config walk.
fn case_environ(case_dir: &Path, spec: &Case) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
        (
            "HOME".into(),
            case_dir.join("home").to_string_lossy().into(),
        ),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
        (
            "FNO_GLOBAL_SETTINGS_PATH".into(),
            case_dir
                .join("global-settings.yaml")
                .to_string_lossy()
                .into(),
        ),
    ];
    for (k, v) in spec.markers {
        env.push(((*k).into(), (*v).into()));
    }
    if let Some(_total) = spec.gh_stub {
        let base_path = env.remove(0);
        if let ("PATH", old) = &base_path {
            env.insert(
                0,
                (
                    "PATH".into(),
                    format!("{}:{}", case_dir.join("bin").display(), old),
                ),
            );
        }
    }
    env
}

/// Run the door for one case and return its (exit, stdout, stderr) as a
/// `Golden` with both streams, fixture paths normalized to `<TMP>`.
fn run_child(root: &Path, spec: &Case) -> Golden {
    let case_dir = root.join(common::slug(spec.label));
    let repo = build_fixtures(&case_dir, spec);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.args(["review-level", "capability"]);
    let mut stdin_payload = String::new();
    match &spec.action {
        Action::Reviewers {
            reviewers,
            peers,
            peer_identity,
        } => {
            cmd.args(["reviewers"]);
            stdin_payload = format!(
                "{{\"reviewers\": {reviewers}, \"descriptors\": {DESCRIPTORS}, \"peers\": {peers}, \"peer_identity\": {}}}",
                match peer_identity {
                    Some(id) => format!("\"{id}\""),
                    None => "null".to_string(),
                }
            );
            cmd.stdin(Stdio::piped());
        }
        Action::GithubApps { apps, nudge } => {
            cmd.args(["github-apps"]);
            stdin_payload = format!("{{\"apps\": {apps}, \"nudge_logins\": {nudge}}}");
            cmd.stdin(Stdio::piped());
        }
        Action::Invocation { flags } => {
            cmd.args(["invocation"]);
            cmd.args(flags.iter());
        }
        Action::SkillPresence { flags } => {
            cmd.args(["skill-presence"]);
            cmd.args(flags.iter());
        }
    }
    let env = case_environ(&case_dir, spec);
    cmd.env_clear();
    for (k, v) in &env {
        cmd.env(k, v);
    }
    // The triad-spawning guard's law: a test that spawns the client names its
    // owner, so a daemon it reaches cannot outlive the run. The pins stay
    // authoritative for detect_session; FNO_TEST_OWNER_* is not an identity
    // marker.
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.current_dir(&repo);
    let mut child = cmd.spawn().expect("fno-agents binary spawns");
    if !stdin_payload.is_empty() {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin was piped for a payload action")
            .write_all(stdin_payload.as_bytes())
            .expect("payload writes to the door's stdin");
    }
    let output = child.wait_with_output().expect("fno-agents binary runs");
    let norm = |text: String| -> String {
        // The driver normalizes its temp ROOT (the case dir lives one level
        // under it), so the test must replace the same span or the `<TMP>`
        // suffixes diverge by the slug segment.
        let raw = root.to_string_lossy().into_owned();
        let resolved = std::fs::canonicalize(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| raw.clone());
        text.replace(resolved.as_str(), "<TMP>")
            .replace(raw.as_str(), "<TMP>")
    };
    Golden {
        exit: output.status.code(),
        streams: vec![
            norm(String::from_utf8_lossy(&output.stdout).into_owned()),
            norm(String::from_utf8_lossy(&output.stderr).into_owned()),
        ],
    }
}

/// The capture-mode oracle: the Python leg through the driver's `--case`
/// door. The driver owns every fixture and env pin, so the caller passes a
/// minimal env; the driver normalizes its own paths to `<TMP>`.
fn run_python_oracle(spec: &Case) -> Golden {
    let driver =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/review_capability_golden_driver.py");
    let output = Command::new(python_executable())
        .arg(&driver)
        .arg("--case")
        .arg(common::slug(spec.label))
        .env("PYTHONPATH", python_path())
        .output()
        .expect("capture driver spawns");
    Golden {
        exit: output.status.code(),
        streams: vec![
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ],
    }
}

/// Repo `cli/src` so Python can import the real `fno` package.
fn python_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cli/src")
}

#[test]
fn review_capability_golden_parity() {
    let root =
        std::env::temp_dir().join(format!("review-capability-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for spec in cases() {
        let rust = run_child(&root, &spec);
        let oracle = capture_mode().then(|| run_python_oracle(&spec));
        assert_golden("review_capability", spec.label, &rust, oracle);
    }
    let _ = std::fs::remove_dir_all(&root);
}
