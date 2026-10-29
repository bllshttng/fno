//! Characterization for the think-inspect port (protocol steps 2-4,
//! docs/architecture/dual-implementation-inventory.md). The Rust leg is
//! `fno_agents::think_inspect`; the Python leg it freezes against was
//! `cli/src/fno/think_inspect.py`. Every receipt section (node summary,
//! repository, pull requests, pitfalls, database, graph recall with its
//! decision and plan-ruling evidence), the whole receipt with injected
//! sources, the human render, and the missing-repository refusal replay one
//! frozen case from `tests/golden/think_inspect/pure.json`, captured from
//! the Python leg before it was deleted. The database cases were cut by
//! hand when the codemap schema grounding was removed.

//! parity-stage: characterization
//! parity-oracle: fno.think_inspect

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use fno_agents::think_inspect::{self, Inputs, Outcome};
use serde_json::{json, Value};

fn cases() -> Vec<Value> {
    let path = format!(
        "{}/tests/golden/think_inspect/pure.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect("read think_inspect goldens");
    let doc: Value = serde_json::from_str(&text).expect("parse think_inspect goldens");
    doc["cases"].as_array().expect("cases").clone()
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

fn opt(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn outcome(v: &Value) -> Outcome {
    (opt(&v[0]), opt(&v[1]))
}

fn rows(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// A fresh temp dir by its canonical path, so every path the leg prints
/// normalizes to `<TMP>` the way the driver's resolved temp dir did.
fn tmp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
    (dir, root)
}

fn normalized(v: &Value, root: &Path) -> Value {
    let text = serde_json::to_string(v)
        .expect("serialize")
        .replace(&root.display().to_string(), "<TMP>");
    serde_json::from_str(&text).expect("reparse")
}

fn write_files(root: &Path, files: &Value) {
    if let Some(map) = files.as_object() {
        for (rel, body) in map {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, body.as_str().unwrap_or("")).expect("write");
        }
    }
}

fn decisions_answer(v: &Value) -> Result<Vec<Value>, String> {
    match v.get("raise").and_then(Value::as_str) {
        Some(message) => Err(message.to_string()),
        None => Ok(rows(v)),
    }
}

fn graph_section(input: &Value) -> Value {
    let entries = input["entries"].as_array().cloned();
    let archive = rows(&input["archive"]);
    let error = opt(&input["error"]);
    let archive_error = opt(&input["archive_error"]);
    let answer = input["decisions"].clone();
    let decisions = move |_node: &str| decisions_answer(&answer);
    think_inspect::graph_section(
        s(input, "seed"),
        entries.as_deref(),
        &archive,
        error.as_deref(),
        archive_error.as_deref(),
        None,
        &decisions,
    )
}

fn build_receipt(input: &Value) -> Value {
    let (_guard, root) = tmp();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    write_files(&repo, &input["repo_files"]);
    let plans = root.join("plans");
    if !input["plans"].is_null() {
        std::fs::create_dir_all(&plans).expect("plans");
        write_files(&plans, &input["plans"]);
    }
    let home = root.join("home");
    if let Some(body) = input["lessons"].as_str() {
        let history = home.join(".fno").join("history");
        std::fs::create_dir_all(&history).expect("history");
        std::fs::write(history.join("lesson-candidates.jsonl"), body).expect("lessons");
    }
    let commands = input["commands"].clone();
    let searched: RefCell<Option<String>> = RefCell::new(None);
    let run = |argv: &[&str], _repo: &Path| -> Outcome {
        if argv.len() > 6 && argv[..3] == ["gh", "pr", "list"] {
            *searched.borrow_mut() = Some(argv[6].to_string());
        }
        let key = argv[..3.min(argv.len())].join(" ");
        match commands.get(key.as_str()) {
            Some(answer) => outcome(answer),
            None => (None, Some("unstubbed".to_string())),
        }
    };
    let answer = input["decisions"].clone();
    let decisions = move |_node: &str| decisions_answer(&answer);
    let inputs = Inputs {
        graph: Some((
            input["graph"]["entries"].as_array().cloned(),
            opt(&input["graph"]["error"]),
        )),
        archive: Some(rows(&input["archive"])),
        plans_path: Some(plans),
        home: Some(home),
        run: &run,
        decisions: &decisions,
    };
    let receipt = think_inspect::build_receipt(s(input, "seed"), &repo, &inputs);
    let render = think_inspect::render_receipt(&receipt);
    let gh_search = searched.borrow().clone();
    normalized(
        &json!({"receipt": receipt, "gh_search": gh_search, "render": render}),
        &root,
    )
}

fn answer(f: &str, input: &Value) -> Value {
    match f {
        "node_summary" => think_inspect::node_summary(
            &input["entry"],
            input["archived"].as_bool().unwrap_or(false),
        ),
        "repository" => think_inspect::repository_section(
            s(input, "root"),
            &outcome(&input["branch"]),
            &outcome(&input["head"]),
            &outcome(&input["status"]),
            &outcome(&input["log"]),
        ),
        "pull_requests" => {
            think_inspect::pull_requests_section(&(opt(&input["stdout"]), opt(&input["error"])))
        }
        "pitfall_headings" => {
            let (_guard, root) = tmp();
            write_files(&root, &input["files"]);
            let (source, headings) = think_inspect::pitfall_headings(&root);
            json!([source, headings])
        }
        "database" => {
            let (_guard, root) = tmp();
            let repo = root.join("repo");
            std::fs::create_dir_all(&repo).expect("repo");
            write_files(&repo, &input["files"]);
            think_inspect::database_section(&repo)
        }
        "pitfalls" => {
            let (_guard, root) = tmp();
            let repo = root.join("repo");
            std::fs::create_dir_all(&repo).expect("repo");
            write_files(&repo, &input["files"]);
            let plans = root.join("plans");
            if let Some(names) = input["plans"].as_array() {
                std::fs::create_dir_all(&plans).expect("plans");
                for name in names {
                    std::fs::write(plans.join(name.as_str().unwrap_or("")), "# Retro\n")
                        .expect("plan");
                }
            }
            let home = root.join("home");
            if let Some(body) = input["lessons"].as_str() {
                let history = home.join(".fno").join("history");
                std::fs::create_dir_all(&history).expect("history");
                std::fs::write(history.join("lesson-candidates.jsonl"), body).expect("lessons");
            }
            normalized(
                &think_inspect::pitfalls_section(&repo, &plans, &home),
                &root,
            )
        }
        "graph_section" => graph_section(input),
        "build_receipt" => build_receipt(input),
        "render_receipt" => json!(think_inspect::render_receipt(input)),
        "cli_missing_repo" => {
            let (_guard, root) = tmp();
            let args: Vec<String> = input["args"]
                .as_array()
                .expect("args")
                .iter()
                .map(|a| {
                    a.as_str()
                        .unwrap_or("")
                        .replace("<TMP>", &root.display().to_string())
                })
                .collect();
            // The refusal text goes to stderr, which the replay cannot read;
            // the exit code and the refused-before-any-read path are pinned.
            let code = think_inspect::run(&args);
            json!({"exit": code})
        }
        other => panic!("no Rust answer for golden fn {other}"),
    }
}

fn expected(f: &str, output: &Value) -> Value {
    match f {
        "cli_missing_repo" => json!({"exit": output["exit"]}),
        _ => output.clone(),
    }
}

#[test]
fn every_frozen_think_inspect_case_replays_on_the_rust_leg() {
    let cases = cases();
    assert!(cases.len() >= 70, "golden set shrank: {}", cases.len());
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let f = s(case, "fn");
        let got = answer(f, &case["input"]);
        let want = expected(f, &case["output"]);
        if got != want {
            failures.push(format!(
                "case {i} {f}\n  input:    {}\n  expected: {}\n  got:      {}",
                case["input"], want, got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} think_inspect cases diverge:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
