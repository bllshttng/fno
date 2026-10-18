//! parity-stage: differential
//! parity-oracle: fno.pr._merge.run_merge
//!
//! The goldens under tests/golden/merge_verb/ were captured from the Python
//! leg by capture_driver.py: its default mode runs the oracle on each hermetic
//! fixture and freezes the masked streams plus a folded stub-call journal.
//! This test is the Rust half of that protocol. It rebuilds the identical
//! fixture tree with `--build-only` (import-free, no oracle run), drives the
//! operand through the production argv door
//! (`fno-agents authorized-merge merge ...`), and compares masked streams and
//! call folds against the frozen goldens. Capture mode is a no-op here: the
//! driver is the capture path, and a golden can only be re-frozen by running
//! the driver against the Python leg, never from this binary.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use common::{assert_golden, Golden};
use serde_json::{json, Value};

mod common;

/// The stage-1 case set: transport, posture, hold and lock legs, in cases.json
/// order. The effect legs (merged, flake accepted, cleanup partial) and the
/// fence end-to-end leg stay on the Python side until their port stages land;
/// the hold and merge-result families carry their own parity targets there.
const IN_SCOPE: [&str; 14] = [
    "merge_no_pr_arg",
    "merge_unknown_arg",
    "merge_invalid_pr",
    "merge_invoker_skip",
    "merge_repo_unreadable",
    "merge_gh_missing",
    "merge_lock_held",
    "merge_decide_refused",
    "merge_decide_head_changed",
    "merge_decide_unknown",
    "merge_decide_unavailable",
    "merge_flake_held",
    "merge_grant_refused",
    "merge_grant_held",
];

/// The check-stage cases: the hold door's three frozen legs. The hold and
/// merge-result families carry their own parity targets, so these run beside
/// the stage-1 merge set.
const IN_SCOPE_HOLD: [&str; 3] = [
    "hold_no_graph_clear",
    "hold_graph_with_hold_row",
    "hold_corrupt_graph",
];

/// Session identity stripped before the leg runs, mirroring STRIP_VARS in
/// capture_driver.py: a cargo-test parent carries live-session pins
/// (harness ids, plugin roots, this crate's own capture switch) that would
/// otherwise reach into the hermetic fixture.
const STRIP_VARS: [&str; 21] = [
    "CLAUDE_CODE_SESSION_ID",
    "TARGET_SESSION_ID",
    "SESSION_ID",
    "FNO_HARNESS_NAME",
    "FNO_HARNESS_SESSION_ID",
    "CLAUDE_PLUGIN_ROOT",
    "TARGET_MISSION_ID",
    "FNO_AGENTS_BIN",
    "FNO_AGENTS_FRONT",
    "FNO_AGENTS_WORKER",
    "FNO_REPO_ROOT",
    "FNO_CAPTURE_GOLDEN",
    "FNO_TRACKER_BACKEND",
    "FNO_EVENTS_PATH",
    "FNO_STATE_DIR",
    "FNO_CLAIMS_ROOT",
    "FNO_CONFIG",
    "FNO_AGENTS_HOME",
    "FNO_MERGE_CASE_DIR",
    "GIT_DIR",
    "GIT_WORK_TREE",
];

#[test]
fn merge_verb_matches_python_goldens() {
    if common::capture_mode() {
        // See the module docs: the driver is the capture path.
        return;
    }
    let root = tempfile::tempdir().expect("fixture tempdir");
    let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/merge_verb/capture_driver.py");
    let built = Command::new("python3")
        .arg(&driver)
        .arg("--build-only")
        .arg(root.path())
        .output()
        .expect("capture_driver --build-only runs");
    assert!(
        built.status.success(),
        "--build-only failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let start = built
        .stdout
        .iter()
        .position(|&b| b == b'{')
        .expect("--build-only prints a JSON map");
    let map: Value = serde_json::from_slice(&built.stdout[start..]).expect("build map parses");
    let stubbin = PathBuf::from(map["stubbin"].as_str().expect("stubbin path"));
    let cases: HashMap<&str, &Value> = map["cases"]
        .as_array()
        .expect("cases list")
        .iter()
        .map(|entry| (entry["slug"].as_str().expect("case slug"), entry))
        .collect();

    for slug in IN_SCOPE.into_iter().chain(IN_SCOPE_HOLD) {
        let entry = cases[slug];
        let case_dir = PathBuf::from(entry["dir"].as_str().expect("case dir"));
        run_case(&case_dir, &stubbin);
    }
}

/// The case's pr argument in the corpus's two spellings: the merge family
/// froze strings, the hold and result families froze ints. The Python
/// driver int()s either, so the harness mirrors that instead of assuming
/// one JSON type.
fn case_pr(case: &Value) -> Option<String> {
    case["pr"]
        .as_str()
        .filter(|pr| !pr.is_empty())
        .map(str::to_string)
        .or_else(|| case["pr"].as_i64().map(|pr| pr.to_string()))
}

/// Run one case's operand child on the fixture and compare streams + folds.
fn run_case(case_dir: &Path, stubbin: &Path) {
    let case: Value = serde_json::from_str(
        &fs::read_to_string(case_dir.join("case.json")).expect("case.json exists"),
    )
    .expect("case.json parses");
    let label = case["label"].as_str().expect("case label");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    // The keeper-reap guard requires every triad-spawning test to declare
    // its owner, so the spawned client is attributed to this test process.
    cmd.envs(fno_agents::test_run::self_owner_env());
    // `authorized-merge` is the registered top-level verb; the entry word
    // below is its operand door, never a verb of its own.
    cmd.arg("authorized-merge");
    // The door word follows the case's entry: the hold cases drive the
    // check-stage door the deleted Python leaf answered.
    match case["entry"].as_str().unwrap_or("merge") {
        "hold" => {
            cmd.arg("hold-check");
            if let Some(pr) = case_pr(&case) {
                cmd.arg(pr);
            }
        }
        _ => {
            cmd.arg("merge");
            if let Some(pr) = case_pr(&case) {
                cmd.arg(pr);
            }
            if let Some(argv) = case["argv"].as_array() {
                for arg in argv {
                    cmd.arg(arg.as_str().expect("argv entries are strings"));
                }
            }
        }
    }

    // The environment cmd_case pins: session identity stripped, the fixture's
    // config/state/home/claims world installed, stub binaries on the
    // FNO_AGENTS_* knobs, git de-promped and de-configured, then the case's
    // own env map applied last.
    for var in STRIP_VARS {
        cmd.env_remove(var);
    }
    cmd.env("FNO_CONFIG", case_dir.join("config.toml"));
    cmd.env("FNO_STATE_DIR", case_dir);
    cmd.env("FNO_AGENTS_HOME", case_dir.join("home").join("agents-home"));
    cmd.env("HOME", case_dir.join("home"));
    cmd.env(
        "FNO_EVENTS_PATH",
        case_dir.join("journals").join("events.jsonl"),
    );
    cmd.env("FNO_CLAIMS_ROOT", case_dir.join("claims"));
    cmd.env("FNO_MERGE_CASE_DIR", case_dir);
    cmd.env("FNO_AGENTS_BIN", stubbin.join("fno-agents"));
    cmd.env("FNO_AGENTS_WORKER", stubbin.join("fno-agents-worker"));
    cmd.env("FNO_AGENTS_FRONT", stubbin.join("fno-front"));
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
    match case["path"].as_str().unwrap_or_default() {
        "empty" => {
            cmd.env("PATH", "");
        }
        "system" => {
            cmd.env("PATH", "/usr/bin:/bin");
        }
        "gitbin" => {
            cmd.env("PATH", case_dir.join("gitbin"));
        }
        _ => {
            cmd.env("PATH", format!("{}:/usr/bin:/bin", stubbin.display()));
        }
    }
    if let Some(env) = case["env"].as_object() {
        for (key, value) in env {
            cmd.env(key, value.as_str().expect("case env values are strings"));
        }
    }
    // Rust-test-only knob: the Python leg takes authority as a run_merge
    // argument; the operand door reads it from this env var instead.
    cmd.env(
        "FNO_MERGE_AUTHORITY",
        case["authority"].as_str().unwrap_or("manifest"),
    );
    cmd.current_dir(case_dir);
    cmd.stdin(Stdio::null());
    let output = cmd.output().expect("merge operand child runs");

    let masked_out = mask_text(&String::from_utf8_lossy(&output.stdout), case_dir);
    let masked_err = mask_text(&String::from_utf8_lossy(&output.stderr), case_dir);
    // The driver freezes .out always and .err only when the masked stderr is
    // nonempty; assert_golden reads the same shape back.
    let mut streams = vec![masked_out];
    if !masked_err.is_empty() {
        streams.push(masked_err);
    }
    let rust = Golden {
        exit: output.status.code(),
        streams,
    };
    assert_golden("merge_verb", label, &rust, None);
    compare_calls(label, case_dir);
}

/// Compare the folded stub-call journals against the golden `.calls`.
fn compare_calls(label: &str, case_dir: &Path) {
    let key = common::slug(label);
    let golden_text =
        fs::read_to_string(common::golden_dir("merge_verb").join(format!("{key}.calls")))
            .unwrap_or_else(|e| panic!("[{label}] missing golden {key}.calls: {e}"));
    let golden: Value = serde_json::from_str(golden_text.trim())
        .unwrap_or_else(|e| panic!("[{label}] golden {key}.calls does not parse: {e}"));
    let rust_text = mask_text(&fold_calls(case_dir).to_string(), case_dir);
    let rust: Value = serde_json::from_str(&rust_text)
        .unwrap_or_else(|e| panic!("[{label}] folded calls do not parse: {e}"));
    for field in ["agents", "worker", "gh", "front", "asked_keys", "events"] {
        if field == "gh" {
            // The Python fold's gh rows are the coverage REST pre-read; the
            // 2.1 verb drops that hop and its fold parity joins the REST
            // restore in the effect stage. Every other fold key compares.
            continue;
        }
        assert_eq!(
            golden.get(field),
            rust.get(field),
            "[{label}] stub-call fold '{field}' differs from the golden"
        );
    }
}

/// Mirror of fold_calls in capture_driver.py: read the six journal keys in
/// the driver's order and collapse each to first-appearance runs.
fn fold_calls(case_dir: &Path) -> Value {
    let journals = case_dir.join("journals");
    let agents = read_rows(&journals, "fno-agents.jsonl");
    let worker = read_rows(&journals, "fno-agents-worker.jsonl");
    let gh = read_rows(&journals, "gh.jsonl");
    let front = read_rows(&journals, "front.jsonl");
    let events = read_rows(&journals, "events.jsonl");
    let asked_keys = collapse(
        agents
            .iter()
            .filter(|row| row.get("op").and_then(Value::as_str) == Some("claim-status"))
            .map(|row| field_of(row, "key"))
            .collect(),
    );
    json!({
        "agents": collapse(agents.iter().map(|row| field_of(row, "op")).collect()),
        "worker": collapse(worker.iter().map(|row| field_of(row, "op")).collect()),
        "gh": collapse(gh.iter().map(|row| field_of(row, "op")).collect()),
        "front": collapse(front.iter().map(|row| field_of(row, "op")).collect()),
        "asked_keys": asked_keys,
        "events": collapse(events.iter().map(|row| field_of(row, "type")).collect()),
    })
}

/// Missing file folds to no rows; an unparsable line folds to an op sentinel,
/// exactly as the driver does.
fn read_rows(journals: &Path, name: &str) -> Vec<Value> {
    let Ok(text) = fs::read_to_string(journals.join(name)) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or(json!({"op": "<unparsed>"})))
        .collect()
}

fn field_of(row: &Value, key: &str) -> Value {
    row.get(key).cloned().unwrap_or(Value::Null)
}

/// Mirror of collapse in capture_driver.py: first-appearance order; a
/// consecutive run longer than one folds to `<value>*`, a run of exactly one
/// keeps the bare value.
fn collapse(values: Vec<Value>) -> Vec<Value> {
    let mut runs: Vec<(Value, usize)> = Vec::new();
    for value in values {
        match runs.last_mut() {
            Some(entry) if entry.0 == value => entry.1 += 1,
            _ => runs.push((value, 1)),
        }
    }
    runs.into_iter()
        .map(|(value, n)| {
            if n == 1 {
                value
            } else {
                Value::String(format!("{}*", py_str(&value)))
            }
        })
        .collect()
}

/// Python `%s` spelling of a JSON value, for the run-star suffix.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

/// Mirror of mask_text in capture_driver.py: strip host-specific spellings so
/// the frozen bytes stay portable.
fn mask_text(text: &str, case_dir: &Path) -> String {
    let mut out = text.replace(case_dir.to_string_lossy().as_ref(), "<CASE>");
    if let Ok(real) = fs::canonicalize(case_dir) {
        out = out.replace(real.to_string_lossy().as_ref(), "<CASE>");
    }
    let out = replace_span(&out, "(pid=", ')', true, "(pid=<ERR>)");
    let out = replace_span(
        &out,
        "graph is unreadable: ",
        ';',
        false,
        "graph is unreadable: <ERR>",
    );
    collapse_held_durations(&out)
}

/// A regex-free `needle[^terminator]*` substitution. `consume` mirrors
/// whether the Python pattern's match swallowed the terminator: the pid
/// pattern ends with `[^)]*` inside parens, so the `)` is consumed; the
/// graph-unreadable pattern stops before `;`, so the terminator survives.
fn replace_span(
    text: &str,
    needle: &str,
    terminator: char,
    consume: bool,
    replacement: &str,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(needle) {
        let after = &rest[start + needle.len()..];
        match after.find(terminator) {
            Some(end) => {
                out.push_str(&rest[..start]);
                out.push_str(replacement);
                if !consume {
                    out.push(terminator);
                }
                rest = &after[end + terminator.len_utf8()..];
            }
            None => {
                out.push_str(&rest[..start]);
                out.push_str(replacement);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Mirror of the `held \d+s` -> `held <T>s` mask: the wait duration depends
/// on machine speed, so it freezes as a placeholder.
fn collapse_held_durations(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if text[i..].starts_with("held ") {
            let mut j = i + "held ".len();
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + "held ".len() && j < bytes.len() && bytes[j] == b's' {
                out.push_str("held <T>s");
                i = j + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("scan sits on a boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}
