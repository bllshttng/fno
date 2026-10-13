//! Golden parity for the raw `pr-closure-*` produce verbs.
//!
//! parity-stage: characterization
//! parity-oracle: tests/fixtures/pr_closure_produce/oracle.py (frozen copy
//! of the retired cli/src/fno/pr/closure.py)
//!
//! Each case in the fixture `cases.json` builds the same hermetic graph,
//! config, and git fixture the deleted oracle's builder made, runs the Rust
//! verb, and compares the bytes against goldens frozen from that oracle
//! while the port was differential. Both sides are masked identically
//! (case paths become `<CASE>`, error details become `<ERR>`) before the
//! golden comparison, so the frozen goldens never embed a machine's
//! tempdir. Freeze: `FNO_CAPTURE_GOLDEN=1 cargo -p fno-agents --test
//! pr_closure_produce_parity test produce_verbs`.
//!
//! `pr-closure-context` has no oracle leg: Python exposes it only as an
//! in-process library call, never a wire verb, so its JSON envelope is a
//! new Rust-binary surface. Its bytes and failure paths are pinned by the
//! stub-gh characterization below; the Python library call it mirrors is
//! `fetch_pr_closure_context` in the oracle file.

mod common;

use common::{
    assert_golden as assert_golden_common, capture_mode, golden_dir, make_script, slug, Golden,
};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CORRUPT_BYTES: &[u8] = b"this fixture graph is deliberately corrupt\n";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pr_closure_produce")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// -------------------------------------------------------------------------
// Fixture build. Mirrors the oracle's build_fixture / build_git so capture
// mode compares like with like.
// -------------------------------------------------------------------------

fn build_git(case_dir: &Path, spec: &Value) {
    let run = |argv: &[&str]| {
        let out = Command::new("git")
            .args(argv)
            .current_dir(case_dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            argv,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "fixture@example.com"]);
    run(&["config", "user.name", "Fixture"]);
    run(&["commit", "--allow-empty", "-q", "-m", "seed"]);
    if spec.as_str() == Some("detached") {
        run(&["checkout", "--detach", "-q"]);
    } else {
        let branch = spec
            .get("branch")
            .and_then(Value::as_str)
            .expect("branch name");
        run(&["checkout", "-q", "-b", branch]);
    }
}

fn build_fixture(root: &Path, case: &Value, index: usize) -> PathBuf {
    let case_dir = root.join(format!("case-{index:02}"));
    std::fs::create_dir_all(&case_dir).unwrap();
    // state_dir is a TOP-LEVEL key: under a [table] header the config
    // loader reads it as a table field and warns it is unmodeled.
    std::fs::write(
        case_dir.join("config.toml"),
        format!("state_dir = \"{}\"\n", case_dir.display()),
    )
    .unwrap();
    let rows: Vec<Value> = case
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !rows.is_empty() {
        fno_agents::graph_store::seed_rows_with_slugs(&case_dir.join("graph.json"), &rows)
            .expect("seed fixture rows");
    }
    if case
        .get("corrupt")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        std::fs::write(case_dir.join("graph.json"), CORRUPT_BYTES).unwrap();
        std::fs::write(case_dir.join("graph.db"), CORRUPT_BYTES).unwrap();
    }
    if let Some(spec) = case.get("git") {
        build_git(&case_dir, spec);
    }
    std::fs::write(
        case_dir.join("case.json"),
        serde_json::to_string_pretty(case).unwrap(),
    )
    .unwrap();
    case_dir
}

// -------------------------------------------------------------------------
// Child runs. Every child gets the same hermetic env: the fixture's config
// and state dir, a throwaway agents home, a pinned PATH, and no leaked
// harness or capture variables.
// -------------------------------------------------------------------------

/// The pinned PATH for a child run. Ambient is /usr/bin:/bin; Stub prepends
/// a stub bin dir to it; Empty pins nothing, so a gh lookup fails even on
/// CI runners that ship an executable /usr/bin/gh.
enum PathPlan<'a> {
    Ambient,
    Stub(&'a Path),
    Empty,
}

fn pinned_env(
    cmd: &mut Command,
    case_dir: &Path,
    backend_github: bool,
    home: &Path,
    plan: PathPlan<'_>,
) {
    for var in [
        "FNO_CAPTURE_GOLDEN",
        "FNO_HARNESS_NAME",
        "FNO_HARNESS_SESSION_ID",
        "FNO_TRACKER_BACKEND",
    ] {
        cmd.env_remove(var);
    }
    cmd.env("FNO_CONFIG", case_dir.join("config.toml"));
    cmd.env("FNO_STATE_DIR", case_dir);
    if backend_github {
        cmd.env("FNO_TRACKER_BACKEND", "github");
    }
    cmd.env("FNO_AGENTS_HOME", home);
    let path = match plan {
        PathPlan::Ambient => "/usr/bin:/bin".to_string(),
        PathPlan::Stub(dir) => format!("{}:/usr/bin:/bin", dir.display()),
        PathPlan::Empty => String::new(),
    };
    cmd.env("PATH", path);
}

fn run_child(
    argv: &[String],
    stdin: Option<&str>,
    case_dir: &Path,
    backend_github: bool,
    home: &Path,
    plan: PathPlan<'_>,
) -> Golden {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.args(argv)
        .current_dir(case_dir)
        .envs(fno_agents::test_run::self_owner_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    pinned_env(&mut cmd, case_dir, backend_github, home, plan);
    let mut child = cmd.spawn().expect("spawn fno-agents");
    if let Some(payload) = stdin {
        // The child may answer and exit before draining the pipe; a broken
        // pipe here is the child's answer, not our failure.
        let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("wait fno-agents");
    Golden {
        exit: out.status.code(),
        streams: vec![
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ],
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Envelope mode: the oracle reads the `case.json` the fixture builder
/// wrote and answers one `{"exit","stdout","stderr"}` object.
fn run_oracle(case_dir: &Path, backend_github: bool, home: &Path) -> Golden {
    let uv = find_in_path("uv").unwrap_or_else(|| PathBuf::from("uv"));
    let mut cmd = Command::new(uv);
    cmd.args(["run", "python"])
        .arg(fixture_dir().join("oracle.py"))
        .arg("--case")
        .arg(case_dir)
        .current_dir(case_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    pinned_env(&mut cmd, case_dir, backend_github, home, PathPlan::Ambient);
    cmd.env("PYTHONPATH", repo_root().join("cli/src"));
    let out = cmd.output().expect("run the oracle");
    let parsed: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "oracle envelope is not JSON ({e}): {} / stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    Golden {
        exit: Some(
            parsed
                .get("exit")
                .and_then(Value::as_i64)
                .expect("oracle exit") as i32,
        ),
        streams: vec![
            parsed
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            parsed
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ],
    }
}

// -------------------------------------------------------------------------
// Masking. Byte-mirrors the oracle's mask: both spellings of the case dir
// (declared, then canonicalized) become `<CASE>`, then the two
// prefix-retaining error rewrites run in the same order.
// -------------------------------------------------------------------------

fn mask(text: &str, case_dir: &Path) -> String {
    let mut out = text.to_string();
    let mut spellings = vec![case_dir.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(case_dir) {
        let real = real.to_string_lossy().into_owned();
        if !spellings.contains(&real) {
            spellings.push(real);
        }
    }
    for spelling in spellings {
        out = out.replace(&spelling, "<CASE>");
    }
    let re = regex::Regex::new(r"the graph read failed \([^)]*\)").expect("static regex");
    out = re
        .replace_all(&out, "the graph read failed (<ERR>)")
        .into_owned();
    let re = regex::Regex::new(r#"graph read failed: [^"]*"#).expect("static regex");
    out = re
        .replace_all(&out, "graph read failed: <ERR>")
        .into_owned();
    out
}

fn mask_golden(g: Golden, case_dir: &Path) -> Golden {
    Golden {
        exit: g.exit,
        streams: g.streams.into_iter().map(|s| mask(&s, case_dir)).collect(),
    }
}

// -------------------------------------------------------------------------
// The differential loop.
// -------------------------------------------------------------------------

fn invocation(case: &Value) -> (Vec<String>, Option<String>) {
    let verb = case.get("verb").and_then(Value::as_str).expect("case verb");
    let stdin = serde_json::to_string(case).unwrap();
    match verb {
        // The trailer leg is argv-driven and must never block on stdin.
        "trailer" => {
            let mut argv = vec!["pr-closure-trailer".to_string()];
            if let Some(node) = case.get("node").and_then(Value::as_str) {
                argv.push(node.to_string());
            }
            for extra in case
                .get("extra")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                argv.push("--extra".to_string());
                argv.push(extra.as_str().expect("extra id str").to_string());
            }
            (argv, None)
        }
        // Stdin verbs read the whole case object and pick only their keys,
        // exactly as the oracle legs do.
        "branch-ids" => (vec!["pr-closure-branch-ids".to_string()], Some(stdin)),
        "ensure" => (vec!["pr-closure-ensure".to_string()], Some(stdin)),
        // bind-created takes its payload as one JSON argv element, matching
        // its CLI door (a TTY passthrough stdin must never be read).
        "bind" => (vec!["pr-closure-bind-created".to_string(), stdin], None),
        other => panic!("cases.json carries an unknown verb: {other}"),
    }
}

#[test]
fn produce_verbs_match_the_python_oracle() {
    let cases_text =
        std::fs::read_to_string(fixture_dir().join("cases.json")).expect("cases fixture");
    let cases: Vec<Value> = serde_json::from_str(&cases_text).expect("cases array");
    let root = tempfile::tempdir().expect("fixture root");
    let home = root.path().join("agents-home");
    std::fs::create_dir_all(&home).unwrap();
    let capture = capture_mode();

    for (index, case) in cases.iter().enumerate() {
        let label = case
            .get("label")
            .and_then(Value::as_str)
            .expect("case label")
            .to_string();
        let case_dir = build_fixture(root.path(), case, index);
        let backend_github = case.get("backend").and_then(Value::as_str) == Some("github");
        let (argv, stdin) = invocation(case);

        let rust = run_child(
            &argv,
            stdin.as_deref(),
            &case_dir,
            backend_github,
            &home,
            PathPlan::Ambient,
        );

        // One-stream discipline mirrors the freeze: a case whose oracle leg
        // printed nothing on stderr freezes one .out stream, and both sides
        // are compared on that stream alone. In normal mode the frozen .err
        // file's existence is the same fact.
        let (rust, oracle) = if capture {
            let mut o = mask_golden(run_oracle(&case_dir, backend_github, &home), &case_dir);
            let has_err = !o.streams.get(1).is_none_or(String::is_empty);
            o.streams.truncate(if has_err { 2 } else { 1 });
            let mut r = mask_golden(rust, &case_dir);
            r.streams.truncate(if has_err { 2 } else { 1 });
            (r, Some(o))
        } else {
            let has_err = golden_dir("pr_closure_produce")
                .join(format!("{}.err", slug(&label)))
                .exists();
            let mut r = mask_golden(rust, &case_dir);
            r.streams.truncate(if has_err { 2 } else { 1 });
            (r, None)
        };
        assert_golden_common("pr_closure_produce", &label, &rust, oracle);
    }
}

// -------------------------------------------------------------------------
// Bind stamping needs ambient harness identity; the hermetic child has
// none, so the stamp skips with a named stderr note. The differential
// loop cannot pin that side (a single-stream golden never compares the
// stderr side at all), so the exact note bytes are pinned here: any
// other stderr line still fails.
// -------------------------------------------------------------------------

#[test]
fn bind_stamp_stderr_is_pinned() {
    let root = tempfile::tempdir().expect("fixture root");
    let home = root.path().join("agents-home");
    std::fs::create_dir_all(&home).unwrap();
    let case = json!({
        "label": "bind silence",
        "verb": "bind",
        "url": "https://github.com/acme/widget/pull/41",
        "node": "x-aaaa",
        "rows": [
            {
                "id": "x-aaaa",
                "slug": "node-one",
                "title": "Node one",
                "type": "feature",
                "status": "ready",
                "priority": "p1",
                "details": "seed"
            }
        ]
    });
    let case_dir = build_fixture(root.path(), &case, 0);
    let payload = serde_json::to_string(&case).unwrap();
    let got = run_child(
        // bind-created takes its payload as one JSON argv element (its CLI
        // door); stdin passes no bytes, so a regression to a stdin read
        // sees EOF and exits 2 here instead of passing.
        &["pr-closure-bind-created".to_string(), payload],
        None,
        &case_dir,
        false,
        &home,
        PathPlan::Ambient,
    );
    assert_eq!(got.exit, Some(0));
    assert_eq!(
        got.streams[1],
        "update: no ambient identity to stamp ship provenance for x-aaaa \
         (missing harness); run the link inside a session. Skipped.\n"
    );
    assert!(
        got.streams[0].contains("\"outcome\":\"bound\""),
        "expected a bound outcome in stdout: {}",
        got.streams[0]
    );
}

// -------------------------------------------------------------------------
// pr-closure-context characterization. There is no oracle leg and no
// Python wire envelope to differ against: the envelope below is the new
// contract, and the row mapping mirrors the Python library call
// `fetch_pr_closure_context`. The 30s timeout path is not exercised; a
// stub that sleeps that long costs more than it proves.
// -------------------------------------------------------------------------

const CONTEXT_ARGV: [&str; 1] = ["pr-closure-context"];
const CONTEXT_PAYLOAD: &str = r#"{"pr":41,"repo":"acme/widget"}"#;
const CONTEXT_ROW: &str = r#"{"number":41,"body":"Fixes x-aaaa","url":"https://github.com/acme/widget/pull/41","state":"OPEN","mergedAt":null,"files":[{"path":"crates/a.rs"},{"path":"docs/b.md"}]}"#;
const CONTEXT_ENVELOPE: &str = r#"{"number":41,"body":"Fixes x-aaaa","url":"https://github.com/acme/widget/pull/41","state":"OPEN","merged_at":null,"changed_files":["crates/a.rs","docs/b.md"]}"#;

#[test]
fn context_envelope_and_failure_paths_are_pinned() {
    let root = tempfile::tempdir().expect("fixture root");
    let home = root.path().join("agents-home");
    std::fs::create_dir_all(&home).unwrap();
    let case = json!({"label": "context harness", "verb": "context"});
    let case_dir = build_fixture(root.path(), &case, 0);
    let bin_dir = root.path().join("stub-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let argv: Vec<String> = CONTEXT_ARGV.iter().map(|s| s.to_string()).collect();

    // Happy path: the stub gh prints one gh-JSON row; the envelope
    // re-serializes it in the dataclass field order with snake_case keys.
    make_script(
        &bin_dir,
        "gh",
        &format!("cat >/dev/null\nprintf '%s' '{CONTEXT_ROW}'\n"),
    );
    let got = run_child(
        &argv,
        Some(CONTEXT_PAYLOAD),
        &case_dir,
        false,
        &home,
        PathPlan::Stub(&bin_dir),
    );
    assert_eq!(got.exit, Some(0));
    assert_eq!(got.streams[0], format!("{CONTEXT_ENVELOPE}\n"));
    assert_eq!(got.streams[1], "");

    // No gh anywhere on PATH. An empty PATH keeps the leg hermetic: CI
    // runners ship an executable /usr/bin/gh, which the lookup would find.
    let got = run_child(
        &argv,
        Some(CONTEXT_PAYLOAD),
        &case_dir,
        false,
        &home,
        PathPlan::Empty,
    );
    assert_eq!(got.exit, Some(3));
    assert_eq!(got.streams[0], "");
    assert_eq!(
        got.streams[1],
        "pr-closure-context: gh CLI not found on PATH\n"
    );

    // gh runs and fails: the exit code and the trimmed stderr survive.
    make_script(
        &bin_dir,
        "gh",
        "cat >/dev/null\necho 'simulated gh failure' >&2\nexit 7\n",
    );
    let got = run_child(
        &argv,
        Some(CONTEXT_PAYLOAD),
        &case_dir,
        false,
        &home,
        PathPlan::Stub(&bin_dir),
    );
    assert_eq!(got.exit, Some(3));
    assert_eq!(got.streams[0], "");
    assert_eq!(
        got.streams[1],
        "pr-closure-context: gh pr view #41 failed (rc=7): simulated gh failure\n"
    );

    // gh succeeds but prints nothing.
    make_script(&bin_dir, "gh", "cat >/dev/null\nexit 0\n");
    let got = run_child(
        &argv,
        Some(CONTEXT_PAYLOAD),
        &case_dir,
        false,
        &home,
        PathPlan::Stub(&bin_dir),
    );
    assert_eq!(got.exit, Some(3));
    assert_eq!(got.streams[0], "");
    assert_eq!(
        got.streams[1],
        "pr-closure-context: gh pr view #41 returned no output (exit 0)\n"
    );

    // gh prints something that is not JSON. The expected text carries the
    // Rust parser's own error, computed here so it can never be hard-coded
    // wrong; the Python JSONDecodeError wording differs by design.
    make_script(&bin_dir, "gh", "cat >/dev/null\nprintf '%s' 'not json'\n");
    let got = run_child(
        &argv,
        Some(CONTEXT_PAYLOAD),
        &case_dir,
        false,
        &home,
        PathPlan::Stub(&bin_dir),
    );
    assert_eq!(got.exit, Some(3));
    assert_eq!(got.streams[0], "");
    let parse_error = serde_json::from_str::<Value>("not json").unwrap_err();
    assert_eq!(
        got.streams[1],
        format!("pr-closure-context: gh stdout was not JSON: {parse_error}\n")
    );
}
