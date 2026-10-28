//! The scoreboard family, characterized against the Python leg it replaced.
//!
//! parity-stage: characterization
//! parity-oracle: fno.scoreboard.fold
//!
//! The Python leg (`cli/src/fno/scoreboard/`, deleted in the port) folded the
//! ledger, the journals, the graph, the registry and the skills git history
//! into the telemetry views. The capture driver
//! (`tests/fixtures/scoreboard/scoreboard_golden_driver.py`) froze 18 fixture
//! cases from the Python leg while it still existed: per case, a fixture tree
//! (ledger, journals, graph.db, skills git repo, plans, transcripts) plus the
//! `fno whoami scoreboard` stdout/stderr/exit under pinned env
//! (HOME/FNO_STATE_DIR/FNO_AGENTS_HOME/FNO_REPO_ROOT + stub `fno`/`gh` on
//! PATH). This test replays each case against the native door from a temp
//! copy and asserts byte parity, with three documented normalizers:
//!
//! - both sides normalize their own case-root absolute path to `<CASE>` (the
//!   plan_path echo and the refusal stderr name the state root); the
//!   fixtures hold the capture host's paths as a `<CAPTURE>` placeholder,
//!   which the replay maps to the sandbox root first, so the same
//!   normalizer answers on both sides;
//! - the event-coverage slug names the repo root flattened (`/` to `-`);
//!   the goldens froze the capture host's slug, so the replay maps its own
//!   flattened token to `<SPACEREPO>` and the goldens hold that placeholder;
//! - `delivery_classes` is an insertion-ordered Python dict keyed by a
//!   class-name set, so capture order was set-hash-order: both sides sort
//!   that one object before comparing. Every other object keeps the leg's
//!   own order.
//!
//! The two refusal cases (`since_zero`, `mutually_exclusive`) pin the door's
//! own refusal bytes: the capture's stderr named the harness (typer's usage
//! line), which was implementation noise, so those goldens hold the native
//! one-line refusal and compare like any other case.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRUB: &[&str] = &[
    "FNO_",
    "TARGET_",
    "CLAUDE_",
    "CODEX_",
    "GEMINI_",
    "OPENCODE_",
    "ANTHROPIC_",
    "OPENAI_",
    "ZAI_",
    "GLM_",
];

/// Recursive copy of one case fixture tree into the replay sandbox.
fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create case dir");
    for entry in std::fs::read_dir(src).expect("read case dir") {
        let entry = entry.expect("case entry");
        let ty = entry.file_type().expect("entry type");
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy case file");
        }
    }
}

/// Rebuild the skills git history the driver created and the capture pruned:
/// one commit, pinned author/committer dates, so the short hash the version
/// resolver prints is stable on every machine.
fn rebuild_git(repo: &Path) {
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
            .env("GIT_AUTHOR_DATE", "2026-06-01T00:00:00+00:00")
            .env("GIT_COMMITTER_DATE", "2026-06-01T00:00:00+00:00")
            .output()
            .expect("run git")
    };
    let init = run(&["init", "-q"]);
    assert!(init.status.success(), "git init failed: {:?}", init.stderr);
    let add = run(&["add", "."]);
    assert!(add.status.success(), "git add failed: {:?}", add.stderr);
    let commit = run(&["commit", "-q", "-m", "skills"]);
    assert!(
        commit.status.success(),
        "git commit failed: {:?}",
        commit.stderr
    );
}

/// Sort every `delivery_classes` object in a JSON document (recursively), or
/// return the text unchanged when it is not JSON (text renderings, refusal
/// stderr). The parity normalizer for the set-ordered Python dict.
fn sort_delivery_classes(text: &str) -> String {
    let Ok(mut doc) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                if let Some(dc) = map.get_mut("delivery_classes") {
                    let taken = dc.take();
                    if let Some(obj) = taken.as_object() {
                        let mut sorted = std::collections::BTreeMap::new();
                        for (k, val) in obj {
                            sorted.insert(k.clone(), val.clone());
                        }
                        *dc = sorted.into_iter().collect();
                    }
                }
                for (_, v) in map.iter_mut() {
                    walk(v);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item);
                }
            }
            _ => {}
        }
    }
    walk(&mut doc);
    format!("{}\n", serde_json::to_string_pretty(&doc).unwrap())
}

/// The fixtures hold the capture host's case-root paths as the `<CAPTURE>`
/// placeholder (the ledger's plan/summary paths); a replay sandbox lives
/// elsewhere, so point every UTF-8 fixture at the sandbox root before the
/// door runs. Binary fixtures (the graph store) fail the UTF-8 check and
/// keep their capture bytes.
fn rewrite_fixture_paths(dir: &Path, from: &str, to: &str) {
    for entry in std::fs::read_dir(dir).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let path = entry.path();
        if path.is_dir() {
            rewrite_fixture_paths(&path, from, to);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            if text.contains(from) {
                std::fs::write(&path, text.replace(from, to)).expect("rewrite fixture");
            }
        }
    }
}

/// Replay one fixture case against the native door from a temp copy.
fn run_case(label: &str, argv: &[String]) -> (i32, String, String) {
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/scoreboard")
        .join("cases")
        .join(label);
    let tmp = std::env::temp_dir().join(format!(
        "scoreboard-parity-{}-{}",
        label,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    copy_dir(&golden, &tmp);
    rebuild_git(&tmp.join("repo"));
    rewrite_fixture_paths(&tmp, "<CAPTURE>", tmp.to_str().expect("utf8 tmp"));
    let bin = env!("CARGO_BIN_EXE_fno-agents");
    let mut cmd = Command::new(bin);
    // The capture driver ran the leg with cwd at the case REPO (the same
    // place `project_log` resolves the space journal from), so the replay
    // pins the same cwd.
    cmd.arg("scoreboard")
        .args(argv)
        .current_dir(&tmp.join("repo"));
    // Scrub the ambient FNO_/provider-prefixed env BEFORE the pins ride, so
    // the scrub cannot strip them.
    for key in std::env::vars_os() {
        let name = key.0.to_string_lossy().into_owned();
        if SCRUB.iter().any(|p| name.starts_with(p)) {
            cmd.env_remove(&key.0);
        }
    }
    cmd.env("HOME", tmp.join("home"))
        .env("FNO_STATE_DIR", tmp.join("state"))
        .env("FNO_AGENTS_HOME", tmp.join("state/agents"))
        .env("FNO_REPO_ROOT", tmp.join("repo"))
        .env("LC_ALL", "C")
        // The capture driver put the case's stub `fno`/`gh` first on PATH
        // (`gh pr diff --name-only` answers the scope-drift diff read); the
        // store exec lane resolves the real binary by path, so the stubs
        // never intercept it.
        .env(
            "PATH",
            format!(
                "{}:{}",
                tmp.join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .envs(fno_agents::test_run::self_owner_env())
        // The emission counter the capture's stub `fno` answered, frozen
        // here at the handoff point the door reads.
        .env(
            "FNO_MUX_STATS_JSON",
            r#"{"touch_emit_failures": 2, "started_at": "2026-10-20T10:00:00Z", "measured_at": "2026-10-25T10:00:00Z"}"#,
        );
    let out = cmd.output().expect("run fno-agents scoreboard");
    // The coverage lister prints the fs::canonicalize'd key, which on macOS
    // resolves /var/folders to /private/var/folders; map both spellings, and
    // map the replay's flattened repo token to the goldens' <SPACEREPO>.
    // Longest-token-first: the short spelling is a substring of the
    // canonical one, so the canonical replaces run first.
    let tmp_canon = std::fs::canonicalize(&tmp).unwrap_or_else(|_| tmp.clone());
    let repo_canon = std::fs::canonicalize(tmp.join("repo")).unwrap_or_else(|_| tmp.join("repo"));
    let flatten = |p: &Path| p.to_string_lossy().replace('/', "-");
    let slug_tokens = [flatten(&repo_canon), flatten(&tmp.join("repo"))];
    let map_case = |text: String| {
        text.replace(&slug_tokens[0], "<SPACEREPO>")
            .replace(&slug_tokens[1], "<SPACEREPO>")
            .replace(tmp_canon.to_str().expect("utf8 tmp canon"), "<CASE>")
            .replace(tmp.to_str().expect("utf8 tmp"), "<CASE>")
    };
    let stdout = map_case(String::from_utf8_lossy(&out.stdout).into_owned());
    let stderr = map_case(String::from_utf8_lossy(&out.stderr).into_owned());
    let code = out.status.code().unwrap_or(-1);
    let _ = std::fs::remove_dir_all(&tmp);
    (code, stdout, stderr)
}

#[test]
fn scoreboard_golden_parity() {
    let cases_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/scoreboard/cases.json");
    let cases: std::collections::BTreeMap<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(&cases_path).expect("read cases.json"))
            .expect("parse cases.json");
    let golden_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/scoreboard");
    for (label, spec) in &cases {
        let argv: Vec<String> = spec
            .get("argv")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let key = spec.get("key").and_then(Value::as_str).unwrap_or_default();
        let golden_file = golden_dir.join(format!("{key}.out"));
        let expected = std::fs::read_to_string(&golden_file).expect("read golden");
        let expected_exit: i32 = std::fs::read_to_string(golden_dir.join(format!("{key}.exit")))
            .expect("read golden exit")
            .trim()
            .parse()
            .expect("parse exit");
        let (code, stdout, stderr) = run_case(label, &argv);
        let norm = sort_delivery_classes(&stdout);
        let expected_norm = sort_delivery_classes(&expected);
        assert_eq!(code, expected_exit, "exit mismatch on {label}");
        assert_eq!(
            norm, expected_norm,
            "stdout mismatch on {label}: got:\n{norm}\n-- expected:\n{expected_norm}"
        );
        let err_golden = golden_dir.join(format!("{key}.err"));
        if err_golden.exists() {
            let expected_err = std::fs::read_to_string(&err_golden).expect("read golden err");
            assert_eq!(stderr, expected_err, "stderr mismatch on {label}");
        }
    }
}
