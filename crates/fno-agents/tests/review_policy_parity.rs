//! parity-stage: characterization
//! parity-oracle: fno.review.policy
//! (kinds cases frozen from fno.review.provider_resolution.available_provider_kinds)
//!
//! Golden parity for the review-policy native door, frozen against the live
//! Python verb before its deletion. The case table and fixture builders
//! below mirror the capture driver (hermetic per-case sandbox, whitelist
//! env, migration sentinel pre-seeded so only the verb's own bytes freeze;
//! the record carries no paths, so no normalization applies). Keep the
//! table and the builders in lockstep when a case is added.
//!
//! Provenance notes from the capture, both load-bearing for the port:
//! record.harness reads the harness field with the legacy cli alias
//! (AliasChoices("harness", "cli"), harness wins), required, and a harness
//! outside the record literal makes load_providers RAISE, which is the
//! fail-closed shape fail_invalid_record freezes. An unparseable config
//! layer never raises the record load (it reads as empty) but does fail the
//! settings load, which is why the switch read is strict and fails open.
//! Pydantic refusals on untouched metadata fields (tags, reset_timezone,
//! window_minutes, account_id, description) are NOT mirrored.

mod common;

use common::{assert_golden as assert_golden_common, Golden};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Every prefix the capture driver's whitelist left unset. The scrub runs
/// per case, so a case's marker is gone again by the next case, exactly as
/// under the subprocess driver.
const SCRUB_PREFIXES: [&str; 11] = [
    "FNO_",
    "CLAUDE_",
    "CODEX_",
    "ANTHROPIC_",
    "OPENAI_",
    "ZAI_",
    "GLM_",
    "GEMINI_",
    "OPENCODE_",
    "HERMES_",
    "TARGET_",
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

fn write_file(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn records_toml() -> String {
    "\
[[accounts.records]]
id = \"codex-main\"
name = \"Codex Main\"
harness = \"codex\"
auth = \"managed\"
"
    .to_string()
}

fn records_switch_toml() -> String {
    format!(
        "{}\
[review.cross_model]
enabled = true
",
        records_toml()
    )
}

fn two_codex_toml() -> String {
    format!(
        "{}\
[[accounts.records]]
id = \"codex-alt\"
name = \"Codex Alt\"
harness = \"codex\"
auth = \"managed\"
",
        records_switch_toml()
    )
}

fn other_record_toml(harness: &str) -> String {
    format!(
        "\
[[accounts.records]]
id = \"other-main\"
name = \"Other Main\"
harness = \"{harness}\"
auth = \"managed\"

[review.cross_model]
enabled = true
"
    )
}

fn legacy_record_toml() -> String {
    "\
[[providers.records]]
id = \"codex-main\"
name = \"Codex Main\"
cli = \"codex\"
auth = \"managed\"

[review.cross_model]
enabled = true
"
    .to_string()
}

fn bad_record_toml() -> String {
    "\
[[accounts.records]]
id = \"bad-one\"
name = \"Bad\"
harness = \"nosuch\"
auth = \"managed\"

[review.cross_model]
enabled = true
"
    .to_string()
}

/// One frozen case: fixtures, markers, and what the door is asked.
struct Case {
    label: &'static str,
    files: Vec<(String, String)>,
    markers: &'static [&'static str],
    kind: Kind,
}

enum Kind {
    Assess {
        size: Option<&'static str>,
        surfaces: Vec<String>,
        session: Option<&'static str>,
        state: bool,
    },
    Kinds,
    Refusal,
    Alias,
    UnknownFlag,
}

fn ledger_with(pid: &str) -> String {
    format!("{{\"entries\": [{{\"session_id\": \"sess-a\", \"provider_id\": \"{pid}\"}}]}}")
}

fn ledger_two_rows() -> String {
    "{\"entries\": [{\"session_id\": \"sess-a\", \"provider_id\": \"claude\"}, \
     {\"session_id\": \"sess-a\", \"provider_id\": \"codex\"}]}"
        .to_string()
}

const STATE_FILE: &str = "---\nsession_id: sess-a\n---\n# state\n";

fn runtime_state(rlu: Option<f64>, lea: Option<f64>, usage_pct: Option<f64>, now: f64) -> String {
    let mut health = "{}".to_string();
    if rlu.is_some() || lea.is_some() {
        let mut parts: Vec<String> = Vec::new();
        if let Some(v) = rlu {
            parts.push(format!("\"rate_limited_until\": {v}"));
        }
        if let Some(v) = lea {
            parts.push(format!("\"last_error_at\": {v}"));
        }
        health = format!("{{\"codex-main\": {{{}}}}}", parts.join(", "));
    }
    let mut usage = "{}".to_string();
    if let Some(pct) = usage_pct {
        usage = format!(
            "{{\"codex-main\": {{\"probed_at\": {:.1}, \"partial\": false, \
             \"windows\": [{{\"label\": \"5h\", \"used_pct\": {}, \"resets_at\": {:.1}}}]}}}}",
            now - 10.0,
            pct,
            now + 3600.0
        );
    }
    format!("{{\"provider_health\": {health}, \"usage\": {usage}}}")
}

fn assess(
    size: Option<&'static str>,
    surfaces: Vec<String>,
    session: Option<&'static str>,
    state: bool,
) -> Kind {
    Kind::Assess {
        size,
        surfaces,
        session,
        state,
    }
}

/// The case table, in the capture driver's order.
fn cases(now: f64) -> Vec<Case> {
    let mut v: Vec<Case> = Vec::new();
    let bare: Vec<(String, String)> = vec![];
    // Policy matrix.
    for (label, size) in [
        ("policy-size-s", Some("S")),
        ("policy-size-m", Some("M")),
        ("policy-size-l", Some("L")),
        ("policy-size-absent", None),
        ("policy-size-unknown", Some("XL")),
        ("policy-size-padded-lower", Some(" l ")),
    ] {
        v.push(Case {
            label,
            files: bare.clone(),
            markers: &[],
            kind: assess(size, vec![], None, false),
        });
    }
    // Surfaces.
    for (label, size, surfaces) in [
        ("surface-auth-l", Some("L"), vec!["auth"]),
        ("surface-two-values", Some("S"), vec!["docs", "auth"]),
        ("surface-padded-upper", Some("L"), vec![" Auth "]),
        ("surface-unknown", Some("L"), vec!["docs"]),
    ] {
        v.push(Case {
            label,
            files: bare.clone(),
            markers: &[],
            kind: assess(
                size,
                surfaces.iter().map(|s| s.to_string()).collect(),
                None,
                false,
            ),
        });
    }
    // Ambient harness.
    v.push(Case {
        label: "ambient-claude-high-unresolved",
        files: bare.clone(),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "ambient-codex-high-unresolved",
        files: bare.clone(),
        markers: &["CODEX_THREAD_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "ambient-two-families",
        files: bare.clone(),
        markers: &["CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID"],
        kind: assess(Some("M"), vec![], None, false),
    });
    // Ledger identity.
    v.push(Case {
        label: "ledger-kind-row-diverse",
        files: vec![("state/ledger.json".into(), ledger_with("codex"))],
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("M"), vec![], Some("sess-a"), false),
    });
    v.push(Case {
        label: "ledger-account-id-row",
        files: vec![
            ("home/.fno/config.toml".into(), records_toml()),
            ("state/ledger.json".into(), ledger_with("codex-main")),
        ],
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("M"), vec![], Some("sess-a"), false),
    });
    v.push(Case {
        label: "ledger-unmappable-row",
        files: vec![("state/ledger.json".into(), ledger_with("ghost-account"))],
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("M"), vec![], Some("sess-a"), false),
    });
    v.push(Case {
        label: "ledger-two-rows-latest-wins",
        files: vec![("state/ledger.json".into(), ledger_two_rows())],
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("M"), vec![], Some("sess-a"), false),
    });
    v.push(Case {
        label: "state-file-session",
        files: vec![
            ("state/ledger.json".into(), ledger_with("codex")),
            (".fno/target-state.md".into(), STATE_FILE.to_string()),
        ],
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("M"), vec![], None, true),
    });
    v.push(Case {
        label: "state-file-other-session",
        files: vec![
            ("state/ledger.json".into(), ledger_with("codex")),
            (".fno/target-state.md".into(), STATE_FILE.to_string()),
        ],
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("M"), vec![], Some("sess-b"), true),
    });
    // Capacity with the switch on: global records + switch, a claude ledger
    // row fixes the implementer, the claude marker stands ambient.
    let cap_files = |config: String, runtime: String| -> Vec<(String, String)> {
        vec![
            ("home/.fno/config.toml".into(), config),
            ("state/ledger.json".into(), ledger_with("claude")),
            ("runtime-state.json".into(), runtime),
        ]
    };
    let exhausted = runtime_state(None, None, Some(100.0), now);
    let healthy = runtime_state(None, None, Some(10.0), now);
    v.push(Case {
        label: "cap-healthy-codex-claude-implementer",
        files: cap_files(records_switch_toml(), healthy.clone()),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "cap-codex-exhausted-claude-implementer",
        files: cap_files(records_switch_toml(), exhausted.clone()),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "cap-two-codex-accounts-one-exhausted",
        files: cap_files(two_codex_toml(), exhausted),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "cap-live-lock",
        files: cap_files(
            records_switch_toml(),
            runtime_state(Some(now + 3600.0), Some(now - 10.0), None, now),
        ),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "cap-old-lock",
        files: cap_files(
            records_switch_toml(),
            runtime_state(Some(now + 3600.0), Some(now - 7200.0), None, now),
        ),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "cap-gemini-record",
        files: cap_files(other_record_toml("gemini"), "{}".to_string()),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "cap-opencode-record-ignored",
        files: cap_files(other_record_toml("opencode"), "{}".to_string()),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "cap-legacy-providers-block",
        files: cap_files(legacy_record_toml(), "{}".to_string()),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "cap-switch-off",
        files: cap_files(records_toml(), healthy),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("M"), vec![], None, false),
    });
    v.push(Case {
        label: "cap-switch-absent",
        files: vec![("state/ledger.json".into(), ledger_with("claude"))],
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("M"), vec![], None, false),
    });
    // Read failures.
    v.push(Case {
        label: "fail-invalid-record",
        files: cap_files(bad_record_toml(), "{}".to_string()),
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    v.push(Case {
        label: "fail-config-unparseable",
        files: vec![(
            ".fno/config.toml".into(),
            "[review.cross_model\nenabled = ".into(),
        )],
        markers: &["CLAUDE_CODE_SESSION_ID"],
        kind: assess(Some("L"), vec!["auth".to_string()], None, false),
    });
    // Verb shapes.
    v.push(Case {
        label: "refusal-bare",
        files: bare.clone(),
        markers: &[],
        kind: Kind::Refusal,
    });
    v.push(Case {
        label: "alias-root-spelling",
        files: bare.clone(),
        markers: &[],
        kind: Kind::Alias,
    });
    v.push(Case {
        label: "unknown-flag",
        files: bare.clone(),
        markers: &[],
        kind: Kind::UnknownFlag,
    });
    // Kinds door, frozen in the record shape the capture driver built: the
    // declared-order config reads codex-main then gem-main.
    let kinds_toml = format!(
        "{}\
[[accounts.records]]
id = \"gem-main\"
name = \"Gemini Main\"
harness = \"gemini\"
auth = \"managed\"
",
        records_toml()
    );
    for (label, config, runtime) in [
        ("kinds-no-records", String::new(), "{}".to_string()),
        (
            "kinds-codex-gemini-order",
            kinds_toml.clone(),
            "{}".to_string(),
        ),
        (
            "kinds-codex-exhausted-gemini-healthy",
            kinds_toml.clone(),
            runtime_state(None, None, Some(100.0), now),
        ),
    ] {
        v.push(Case {
            label,
            files: vec![
                ("home/.fno/config.toml".into(), config),
                ("runtime-state.json".into(), runtime),
            ],
            markers: &[],
            kind: Kind::Kinds,
        });
    }
    v
}

#[test]
fn review_policy_golden_parity() {
    let _guard = env_lock();
    let _env = EnvGuard::take_snapshot();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let root = std::env::temp_dir().join(format!("review-policy-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    for test_case in cases(now) {
        let base = root.join(common::slug(test_case.label));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::create_dir_all(base.join("state")).unwrap();
        write_file(&base.join("state/.path-migration-done"), "migrated\n");
        for (rel, content) in &test_case.files {
            write_file(&base.join(rel), content);
        }
        // Pin the case env exactly as the capture driver did: scrub, then
        // the pin set, then the case's own markers.
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
        std::env::set_var("HOME", base.join("home"));
        std::env::set_var("FNO_STATE_DIR", base.join("state"));
        std::env::set_var("FNO_RUNTIME_STATE_PATH", base.join("runtime-state.json"));
        std::env::set_var("FNO_REPO_ROOT", &base);
        std::env::set_var("PWD", &base);
        std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
        for marker in test_case.markers {
            std::env::set_var(marker, "sess-a");
        }
        let (exit, streams) = match &test_case.kind {
            Kind::UnknownFlag => {
                assert!(
                    fno_agents::review_policy::parse_assess_args(&["--bogus-flag".to_string()])
                        .is_err(),
                    "[{}] parser must refuse an unknown flag",
                    test_case.label
                );
                (Some(2), vec![String::new()])
            }
            Kind::Refusal => (
                Some(2),
                vec![
                    String::new(),
                    format!(
                        "{}\n",
                        "sigma is removed: the review is bare /fno:review (the fno review lane), \
which runs inline on every harness and emits its own attestation. \
`fno do review classify` and `fno do review resolve-level` remain for \
the lane's emit step."
                    ),
                ],
            ),
            Kind::Alias => {
                let (out, exit) = assess_alias_outcome(&base);
                (
                    Some(exit),
                    vec![out, "fno review is now fno do review\n".to_string()],
                )
            }
            Kind::Kinds => {
                let record = fno_agents::review_policy::kinds_record(&base);
                (Some(0), vec![record])
            }
            Kind::Assess {
                size,
                surfaces,
                session,
                state,
            } => {
                let state_path = state.then(|| base.join(".fno/target-state.md"));
                let (out, exit) = fno_agents::review_policy::assess_outcome(
                    &base,
                    session.as_deref(),
                    state_path.as_deref(),
                    size.as_deref(),
                    surfaces,
                );
                (Some(exit), vec![out])
            }
        };
        assert_golden_common(
            "review_policy",
            test_case.label,
            &Golden { exit, streams },
            None,
        );
    }
}

fn assess_alias_outcome(base: &Path) -> (String, i32) {
    fno_agents::review_policy::assess_outcome(base, None, None, Some("M"), &[])
}

/// The door's own posture through the real binary: help exits 0 on stdout,
/// the bare assess refuses with the sigma line on stderr and exit 2, an
/// unknown flag exits 2 with empty stdout, and kinds prints the claude-first
/// record (the claude-first invariant is machine-independent; the golden
/// bytes are pinned by the in-process parity above).
#[test]
fn review_policy_door_posture() {
    let bin = env!("CARGO_BIN_EXE_fno-agents");
    // Every spawn declares its test-owner pair so the keeper-reap attributes
    // the children (the static guard in test_owned_keeper_reap.rs reads this
    // file for that declaration).
    let out = Command::new(bin)
        .args(["review-policy", "--help"])
        .envs(fno_agents::test_run::self_owner_env())
        .output()
        .expect("fno-agents binary spawns");
    assert!(out.status.success(), "--help must exit 0");
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("usage: fno-agents review-policy"),
        "--help must print the usage line to stdout"
    );
    let out = Command::new(bin)
        .args(["review-policy", "assess"])
        .envs(fno_agents::test_run::self_owner_env())
        .output()
        .expect("fno-agents binary spawns");
    assert_eq!(
        out.status.code(),
        Some(2),
        "bare assess must refuse with exit 2"
    );
    assert!(out.stdout.is_empty(), "refusal must keep stdout empty");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .starts_with("sigma is removed: the review is bare /fno:review"),
        "refusal must teach the sigma line: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(bin)
        .args(["review-policy", "assess", "--bogus-flag"])
        .envs(fno_agents::test_run::self_owner_env())
        .output()
        .expect("fno-agents binary spawns");
    assert_eq!(out.status.code(), Some(2), "unknown flag must exit 2");
    assert!(out.stdout.is_empty(), "unknown flag must keep stdout empty");
    let out = Command::new(bin)
        .args(["review-policy", "kinds"])
        .env(
            "FNO_RUNTIME_STATE_PATH",
            "/nonexistent-review-policy-posture.json",
        )
        .envs(fno_agents::test_run::self_owner_env())
        .output()
        .expect("fno-agents binary spawns");
    assert!(out.status.success(), "kinds must exit 0");
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("{\"available\": [\"claude\""),
        "kinds must print the claude-first record: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}
