//! parity-stage: characterization
//! parity-oracle: (none - the Python legs were deleted in the same change; the goldens are the contract)
//!
//! Characterization replay for the target-family port. Every
//! `tests/golden/target_family/` case plays against the native doors through
//! the same hermetic fixture the capture driver built, with the same
//! normalization tokens, and compares byte-for-byte against the frozen
//! `.exit/.out/.err` files. The capture (`tests/capture/target_family_capture.py`)
//! froze the Python leg's answers; this file pins the Rust port to them.
//! Keep the case table and the normalizer in lockstep with the driver when
//! a case is added.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

const AGENTS_BIN: &str = env!("CARGO_BIN_EXE_fno-agents");
const SID: &str = "a1b2c3d4-1111-2222-3333-444455556666";
const NODE: &str = "x-1234";

/// A minimal immutable target manifest, frontmatter plus the body keys
/// init-target-state.sh writes below it. No `status:` field: the immutable
/// detection keys on its absence.
const MANIFEST: &str = r#"---
harness: claude
harness_session_id: SID
fno_id: fno-test-0001
graph_node_id: NODE
target_claim_key: node:NODE
target_claim_holder: review-session:SID
owner_cwd: "<ROOT>"
input: fixture target
plan_path: ""
---

graph_node_id: NODE
target_claim_key: node:NODE
target_claim_holder: review-session:SID
"#;

/// A plan whose frontmatter carries the dispatch_hold block the gate reads.
const HOLD_PLAN: &str = r#"---
status: ready
dispatch_hold:
  reason: fixture hold
  release_when: never
  set_by: capture
  review_on: 2026-12-01
---

# Fixture plan
"#;

/// The hermetic env block, the capture driver's `base_env` verbatim: temp
/// HOME/state roots, the claude session pins, the other harness markers
/// blanked, git config isolated. env_clear first: an inherited FNO_* pin
/// would pull a host state root into the replay.
fn base_env(root: &Path) -> Vec<(String, String)> {
    let s = |p: &Path| p.to_string_lossy().to_string();
    let mut env: Vec<(String, String)> = Vec::new();
    env.push(("HOME".into(), s(root)));
    env.push(("FNO_STATE_DIR".into(), s(root)));
    env.push(("FNO_HOME".into(), s(root)));
    env.push(("FNO_AGENTS_HOME".into(), s(&root.join("agents-home"))));
    env.push(("FNO_BUS_DIR".into(), s(&root.join("bus"))));
    env.push(("FNO_EVENTS_PATH".into(), s(&root.join("events.jsonl"))));
    env.push(("FNO_CLAIMS_ROOT".into(), s(root)));
    env.push(("FNO_HARNESS_NAME".into(), "claude".into()));
    env.push(("FNO_HARNESS_SESSION_ID".into(), SID.into()));
    env.push(("CLAUDE_CODE_SESSION_ID".into(), SID.into()));
    env.push(("CODEX_THREAD_ID".into(), String::new()));
    env.push(("CODEX_SESSION_ID".into(), String::new()));
    env.push(("GEMINI_SESSION_ID".into(), String::new()));
    env.push(("OPENCODE_SESSION_ID".into(), String::new()));
    env.push(("CLAUDE_SESSION_ID".into(), String::new()));
    env.push(("FNO_AGENTS_BIN".into(), AGENTS_BIN.into()));
    env.push(("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()));
    env.push(("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()));
    env.push(("FNO_TEST_HERMETIC".into(), "1".into()));
    env.push(("FNO_STYLE_ENFORCE".into(), "0".into()));
    env.push(("PATH".into(), std::env::var("PATH").unwrap_or_default()));
    env
}

fn spawn_op(root: &Path, env: &[(String, String)], args: &[String]) -> std::process::Output {
    let mut cmd = Command::new(AGENTS_BIN);
    cmd.args(args).current_dir(root);
    cmd.env_clear();
    cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    // The child is this test's own spawn: the owner marker keeps a
    // concurrent keeper/reaper from reaping it mid-run, the same
    // declaration every other bin-spawning test file carries.
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.output().expect("the fno-agents binary spawns")
}

fn write_manifest_at(root: &Path, node: &str) {
    let dir = root.join(".fno");
    std::fs::create_dir_all(&dir).unwrap();
    let text = MANIFEST
        .replace("SID", SID)
        .replace("NODE", node)
        .replace("<ROOT>", &root.to_string_lossy().to_string());
    std::fs::write(dir.join("target-state.md"), text).unwrap();
}

fn git(root: &Path, args: &[&str], env: &[(String, String)]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .status()
        .expect("git spawns");
    assert!(
        status.success(),
        "git {args:?} failed in {}",
        root.display()
    );
}

/// Mint one node through the native create and return its id.
fn seed_node(env: &[(String, String)], root: &Path, title: &str) -> String {
    let out = spawn_op(
        root,
        env,
        &[
            "backlog".to_string(),
            "idea".to_string(),
            title.to_string(),
            "--difficulty".to_string(),
            "high".to_string(),
            "--separate".to_string(),
        ],
    );
    assert!(
        out.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("seed prints a JSON receipt");
    value["id"]
        .as_str()
        .expect("seed receipt carries id")
        .to_string()
}

/// The shared normalization contract with the capture driver, in the
/// driver's own order: drop-prefixes, rstrip, sorted compact JSON re-dump,
/// the five regex tokens, then the temp root to `<ROOT>`.
fn normalize(text: &str, root: &Path) -> String {
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with("fno: this claude session joined fno"))
        .collect();
    let mut out = kept.join("\n");
    while out.ends_with('\n') {
        out.pop();
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&out) {
        if value.is_object() {
            let sorted: BTreeMap<String, serde_json::Value> = value
                .as_object()
                .expect("checked is_object")
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            out = serde_json::to_string(&sorted).unwrap_or(out);
        }
    }
    out = Regexes::get().ts.replace_all(&out, "<TS>").into_owned();
    out = Regexes::get().date.replace_all(&out, "<DATE>").into_owned();
    out = Regexes::get().time.replace_all(&out, "<TIME>").into_owned();
    out = Regexes::get()
        .archived
        .replace_all(&out, ".archived.<TS>.md")
        .into_owned();
    out = Regexes::get()
        .node
        .replace_all(&out, "NODE-HEX")
        .into_owned();
    out = Regexes::get().sha.replace_all(&out, "SHA40").into_owned();
    out = Regexes::get().hex8.replace_all(&out, "HEX8").into_owned();
    if let Ok(real) = root.canonicalize() {
        out = out.replace(&real.to_string_lossy().to_string(), "<ROOT>");
    }
    out = out.replace(&root.to_string_lossy().to_string(), "<ROOT>");
    out
}

struct Regexes {
    ts: regex::Regex,
    date: regex::Regex,
    time: regex::Regex,
    archived: regex::Regex,
    node: regex::Regex,
    sha: regex::Regex,
    hex8: regex::Regex,
}

impl Regexes {
    fn get() -> &'static Regexes {
        static HOLDER: OnceLock<Regexes> = OnceLock::new();
        HOLDER.get_or_init(|| Regexes {
            ts: regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?").unwrap(),
            date: regex::Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap(),
            time: regex::Regex::new(r"\d{2}:\d{2}:\d{2}").unwrap(),
            archived: regex::Regex::new(r"\.archived\.\d+\.md").unwrap(),
            node: regex::Regex::new(r"\b[a-z]{1,8}-[0-9a-f]{4,8}\b").unwrap(),
            sha: regex::Regex::new(r"\b[0-9a-fA-F]{40}\b").unwrap(),
            hex8: regex::Regex::new(r"\b[0-9a-fA-F]{8}\b").unwrap(),
        })
    }
}

/// One replay case: the capture driver's row, flags spelled the way the
/// runner's fixture builder reads them.
#[derive(Default)]
struct Case {
    label: &'static str,
    /// The capture driver's argv under `fno` (`do state show`), not the op
    /// spelling; the runner maps the family head to its op.
    argv: &'static [&'static str],
    manifest: bool,
    preset_plan_path: bool,
    config: Option<&'static str>,
    plan_no_kills: bool,
    git_init: bool,
    seed: Option<&'static str>,
    contain: bool,
    hold_plan: bool,
    preset_stub: bool,
    preset_stub_drift: bool,
    node_arg: bool,
    handoff: bool,
}

const STUBS_JSON: &str = r#"[{"stub_id": "create-user", "file": "src/api.ts", "symbol": "createUser", "contract_ref": "design.md#interface-contract", "kind": "function"}]"#;
const STUBS_JSON_MISSING: &str = r#"[{"stub_id": "x"}]"#;

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "state show missing file",
            argv: &["do", "state", "show"],
            ..Default::default()
        },
        Case {
            label: "state show full",
            argv: &["do", "state", "show"],
            manifest: true,
            ..Default::default()
        },
        Case {
            label: "state show field",
            argv: &["do", "state", "show", "--field", "plan_path"],
            manifest: true,
            ..Default::default()
        },
        Case {
            label: "state show json",
            argv: &["do", "state", "--json", "show"],
            manifest: true,
            ..Default::default()
        },
        Case {
            label: "state set other field refused",
            argv: &["do", "state", "set", "--field", "status", "--value", "done"],
            manifest: true,
            ..Default::default()
        },
        Case {
            label: "state set plan_path twice refused",
            argv: &[
                "do",
                "state",
                "set",
                "--field",
                "plan_path",
                "--value",
                "plans/x.md",
            ],
            manifest: true,
            preset_plan_path: true,
            ..Default::default()
        },
        Case {
            label: "state init bootstrap redirect",
            argv: &["do", "state", "init"],
            ..Default::default()
        },
        Case {
            label: "state init allow stub",
            argv: &["do", "state", "init", "--allow-stub"],
            ..Default::default()
        },
        Case {
            label: "state init force overwrite",
            argv: &["do", "state", "init", "--allow-stub", "--force"],
            ..Default::default()
        },
        Case {
            label: "state archive",
            argv: &["do", "state", "archive"],
            manifest: true,
            ..Default::default()
        },
        Case {
            label: "phase kill check default no state",
            argv: &["do", "phase", "kill-check"],
            ..Default::default()
        },
        Case {
            label: "phase kill check with plan arg",
            argv: &["do", "phase", "kill-check", "plans/no-kills.md"],
            plan_no_kills: true,
            ..Default::default()
        },
        Case {
            label: "target check review gate clear",
            argv: &["do", "target", "check-review-gate"],
            ..Default::default()
        },
        Case {
            label: "target check review gate refused",
            argv: &["do", "target", "check-review-gate"],
            config: Some("[review]\nreviewers = [\"sigma\"]\n"),
            ..Default::default()
        },
        Case {
            label: "target check contained no input",
            argv: &["do", "target", "check-contained"],
            ..Default::default()
        },
        Case {
            label: "target check contained plain node",
            argv: &["do", "target", "check-contained"],
            seed: Some("fixture plain target node"),
            ..Default::default()
        },
        Case {
            label: "target check contained child refused",
            argv: &["do", "target", "check-contained"],
            seed: Some("fixture epic parent"),
            contain: true,
            ..Default::default()
        },
        Case {
            label: "target check dispatch hold no input",
            argv: &["do", "target", "check-dispatch-hold"],
            ..Default::default()
        },
        Case {
            label: "target check dispatch hold held",
            argv: &["do", "target", "check-dispatch-hold"],
            seed: Some("fixture held node"),
            hold_plan: true,
            ..Default::default()
        },
        Case {
            label: "target resolve owned identity",
            argv: &["do", "target", "resolve-owned-identity"],
            ..Default::default()
        },
        Case {
            label: "target denominator ratio empty",
            argv: &["do", "target", "denominator-ratio", "--json"],
            ..Default::default()
        },
        Case {
            label: "target resolve model no pin",
            argv: &["do", "target", "resolve-model"],
            seed: Some("fixture unpinned node"),
            node_arg: true,
            ..Default::default()
        },
        Case {
            label: "target resolve model missing node",
            argv: &["do", "target", "resolve-model", "x-0000"],
            ..Default::default()
        },
        Case {
            label: "target request self review no repo",
            argv: &["do", "target", "request-self-review"],
            ..Default::default()
        },
        Case {
            label: "target request self review no pr",
            argv: &["do", "target", "request-self-review"],
            git_init: true,
            ..Default::default()
        },
        Case {
            label: "target review invocation",
            argv: &["do", "target", "review-invocation"],
            ..Default::default()
        },
        Case {
            label: "stub manifest write valid",
            argv: &[
                "do",
                "pr",
                "stub-manifest",
                "write",
                "--node",
                "x-1234",
                "--contract-version",
                "1",
                "--contract-ref",
                "design.md#interface-contract",
                "--contract-test",
                "true",
                "--stubs-json",
                STUBS_JSON,
            ],
            ..Default::default()
        },
        Case {
            label: "stub manifest write malformed",
            argv: &[
                "do",
                "pr",
                "stub-manifest",
                "write",
                "--node",
                "x-1234",
                "--contract-version",
                "1",
                "--contract-ref",
                "ref",
                "--contract-test",
                "true",
                "--stubs-json",
                STUBS_JSON_MISSING,
            ],
            ..Default::default()
        },
        Case {
            label: "stub manifest write missing contract test",
            argv: &[
                "do",
                "pr",
                "stub-manifest",
                "write",
                "--node",
                "x-1234",
                "--stubs-json",
                "[]",
            ],
            ..Default::default()
        },
        Case {
            label: "stub manifest reconcile validate authorize",
            argv: &[
                "do",
                "pr",
                "stub-manifest",
                "reconcile-validate",
                "--node",
                "x-1234",
            ],
            preset_stub: true,
            ..Default::default()
        },
        Case {
            label: "stub manifest reconcile validate drift",
            argv: &[
                "do",
                "pr",
                "stub-manifest",
                "reconcile-validate",
                "--node",
                "x-1234",
            ],
            preset_stub_drift: true,
            ..Default::default()
        },
        Case {
            label: "stub manifest reconcile validate missing manifest",
            argv: &[
                "do",
                "pr",
                "stub-manifest",
                "reconcile-validate",
                "--node",
                "x-9999",
            ],
            ..Default::default()
        },
        Case {
            label: "stub manifest reconcile finalize valid",
            argv: &[
                "do",
                "pr",
                "stub-manifest",
                "reconcile-finalize",
                "--node",
                "x-1234",
            ],
            preset_stub: true,
            ..Default::default()
        },
        Case {
            label: "stub manifest reconcile finalize missing",
            argv: &[
                "do",
                "pr",
                "stub-manifest",
                "reconcile-finalize",
                "--node",
                "x-9999",
            ],
            ..Default::default()
        },
        Case {
            label: "handoff prepare absent claim",
            argv: &["module"],
            handoff: true,
            ..Default::default()
        },
    ]
}

/// One replay: build the fixture root, mint the seed, map the family argv to
/// its op, spawn the binary, normalize, and compare against the frozen
/// golden. Err carries a diff-ready failure block.
fn play_case(case: &Case) -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|e| format!("tempdir failed: {e}"))?;
    let root = dir.path();
    let env0 = base_env(root);
    let _ = std::fs::write(root.join(".path-migration-done"), "captured\n");
    let _ = std::fs::write(root.join("settings.yaml"), "");
    let mut node = NODE.to_string();
    if case.manifest {
        write_manifest_at(root, &node);
    }
    if case.preset_plan_path {
        write_manifest_at(root, &node);
        let path = root.join(".fno").join("target-state.md");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("plan_path: \"\"", "plan_path: plans/x.md"),
        )
        .unwrap();
    }
    if let Some(text) = case.config {
        let dir = root.join(".fno");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }
    if case.plan_no_kills {
        let plans = root.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::write(
            plans.join("no-kills.md"),
            "---\nstatus: ready\n---\n\n# no kill criteria here\n",
        )
        .unwrap();
    }
    if case.git_init {
        git(root, &["init", "-q", "-b", "main"], &env0);
        git(root, &["config", "user.email", "cap@example.com"], &env0);
        git(root, &["config", "user.name", "capture"], &env0);
        std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
        git(root, &["add", "seed.txt"], &env0);
        git(root, &["commit", "-q", "-m", "seed"], &env0);
    }
    let mut env = env0.clone();
    if let Some(title) = case.seed {
        node = seed_node(&env, root, title);
        if case.manifest {
            write_manifest_at(root, &node);
        }
        env.push(("TARGET_INPUT".into(), node.clone()));
    }
    if case.contain {
        let child = seed_node(&env, root, "fixture contained child");
        let args = vec![
            "backlog".to_string(),
            "contain".to_string(),
            node.clone(),
            child.clone(),
        ];
        let out = spawn_op(root, &env, &args);
        assert!(
            out.status.success(),
            "contain failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        env.push(("TARGET_INPUT".into(), child));
    }
    if case.hold_plan {
        let plans = root.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::write(plans.join("hold.md"), HOLD_PLAN).unwrap();
        let args = vec![
            "backlog".to_string(),
            "update".to_string(),
            node.clone(),
            "plan_path=plans/hold.md".to_string(),
        ];
        let out = spawn_op(root, &env, &args);
        assert!(
            out.status.success(),
            "hold_plan update failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        env.push(("TARGET_PLAN_PATH".into(), "plans/hold.md".into()));
    }
    if case.preset_stub || case.preset_stub_drift {
        let dir = root.join(".fno");
        std::fs::create_dir_all(&dir).unwrap();
        let doc = serde_json::json!({
            "node": NODE,
            "contract_version": 1,
            "contract_ref": "design.md#interface-contract",
            "reconciled": false,
            "contract_test": if case.preset_stub_drift { "false" } else { "true" },
            "stubs": [{
                "stub_id": "create-user",
                "file": "src/api.ts",
                "symbol": "createUser",
                "contract_ref": "design.md#interface-contract",
                "kind": "function"
            }],
        });
        let name = format!("stub-manifest-{NODE}.json");
        std::fs::write(
            dir.join(name),
            serde_json::to_string_pretty(&doc).unwrap() + "\n",
        )
        .unwrap();
    }
    let args: Vec<String> = if case.handoff {
        vec![
            "target-gates".to_string(),
            "target-handoff-prepare".to_string(),
            "--state".to_string(),
            root.join(".fno/target-state.md")
                .to_string_lossy()
                .to_string(),
            "--archive".to_string(),
            root.join(".fno/archives/target-state.md")
                .to_string_lossy()
                .to_string(),
            "--claim-key".to_string(),
            format!("node:{node}"),
            "--holder".to_string(),
            format!("review-session:{SID}"),
        ]
    } else {
        let mut a: Vec<&str> = Vec::new();
        let head = case.argv;
        if head.first() == Some(&"do") {
            match head.get(1).copied() {
                Some("state") => {
                    a.push("state-cli");
                    a.extend(head.iter().skip(2));
                }
                Some("phase") => {
                    a.push("kill-check");
                    a.extend(head.iter().skip(2));
                }
                Some("target") => {
                    a.push("target-gates");
                    a.extend(head.iter().skip(2));
                }
                Some("pr") => {
                    a.push("stub-manifest");
                    a.extend(head.iter().skip(3));
                }
                _ => {}
            }
        }
        if case.node_arg {
            a.push(node.as_str());
        }
        a.iter().map(|s| s.to_string()).collect()
    };
    let out = spawn_op(root, &env, &args);
    let code = out.status.code().unwrap_or(-1);
    let stdout = normalize(&String::from_utf8_lossy(&out.stdout), root);
    let stderr = normalize(&String::from_utf8_lossy(&out.stderr), root);
    let key = slug_of(case.label);
    let golden =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/target_family");
    let expected_exit: i32 = std::fs::read_to_string(golden.join(format!("{key}.exit")))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let expected_out = std::fs::read_to_string(golden.join(format!("{key}.out"))).unwrap();
    let expected_err = std::fs::read_to_string(golden.join(format!("{key}.err"))).unwrap();
    if code != expected_exit {
        return Err(format!(
            "[{}] exit {} != golden {}\n--- stderr ---\n{}\n--- stdout ---\n{}",
            case.label, code, expected_exit, stderr, stdout
        ));
    }
    if stdout != expected_out {
        return Err(format!(
            "[{}] stdout differs\n--- golden ---\n{}\n--- run ---\n{}",
            case.label, expected_out, stdout
        ));
    }
    if stderr != expected_err {
        return Err(format!(
            "[{}] stderr differs\n--- golden ---\n{}\n--- run ---\n{}",
            case.label, expected_err, stderr
        ));
    }
    Ok(())
}

/// The slug form the capture driver froze: lowercase alphanumerics, every
/// other run collapsed to one underscore.
fn slug_of(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut prev_us = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

#[test]
fn target_family_replays_the_goldens() {
    let failures: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let table = cases();
    std::thread::scope(|scope| {
        for case in &table {
            let failures = &failures;
            scope.spawn(move || {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| play_case(case))) {
                    Ok(Ok(())) => {}
                    Ok(Err(block)) => failures.lock().unwrap().push(block),
                    Err(_) => failures
                        .lock()
                        .unwrap()
                        .push(format!("[{}] panicked", case.label)),
                }
            });
        }
    });
    let blocks = failures.into_inner().unwrap();
    assert!(
        blocks.is_empty(),
        "{} of {} replay cases differ:\n\n{}",
        blocks.len(),
        table.len(),
        blocks.join("\n\n")
    );
}

/// The state-io atomicity contract, re-hosted from the deleted test_state_io:
/// eight parallel force-writers on one manifest leave it parseable, never
/// torn - the atomic-replace writer (temp sibling + rename) holds the file
/// together under contention.
#[test]
fn state_force_writes_stay_atomic_under_parallel_writers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let env = base_env(root);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let env = &env;
            let root = root;
            scope.spawn(move || {
                let args = vec![
                    "state-cli".to_string(),
                    "init".to_string(),
                    "--allow-stub".to_string(),
                    "--force".to_string(),
                ];
                let out = spawn_op(root, env, &args);
                assert!(
                    out.status.success(),
                    "parallel init failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            });
        }
    });
    let manifest = std::fs::read_to_string(root.join(".fno/target-state.md")).unwrap();
    assert!(
        manifest.matches("---").count() >= 2,
        "torn write: frontmatter fences lost:\n{manifest}"
    );
    assert!(
        manifest.contains("harness:"),
        "torn write lost the frontmatter body:\n{manifest}"
    );
}
