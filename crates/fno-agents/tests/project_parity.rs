//! parity-stage: characterization
//! parity-oracle: cli/src/fno/project/__init__.py + cli/src/fno/projects/resolve.py
//!
//! Characterization for `fno config project`, on the Rust door as
//! `fno-agents spawn-overlay project`. The goldens under
//! tests/golden/project/ were captured from the Python commands before they
//! were deleted (step 2 of the port protocol,
//! docs/architecture/dual-implementation-inventory.md); the cases in
//! tests/fixtures/project-cases.json run in order, so the twice-init case
//! reads the state the first init left.
//!
//! Three things normalize on both sides: the repo root folds to `<ROOT>`,
//! the redirected HOME folds to `<HOME>`, and the text after a `failed to
//! parse: ` marker folds to `<ERROR>` up to its `; using defaults` tail
//! (that wording is the TOML/YAML parser's own, never a contract). The
//! rename notice the front door prints for the moved `fno project` spelling
//! is front-door behavior and appears in no golden.

use common::{assert_golden, Golden};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

mod common;

fn normalize(text: &str, root: &Path, home: &Path) -> String {
    let mut text = text.to_string();
    let mut roots = vec![
        root.to_string_lossy().into_owned(),
        home.to_string_lossy().into_owned(),
    ];
    for base in [root, home] {
        if let Ok(real) = std::fs::canonicalize(base) {
            roots.push(real.to_string_lossy().into_owned());
        }
    }
    roots.sort_by_key(|r| std::cmp::Reverse(r.len()));
    for raw in roots {
        text = text.replace(
            &raw,
            if raw.contains(home.to_string_lossy().as_ref()) {
                "<HOME>"
            } else {
                "<ROOT>"
            },
        );
    }
    let mut folded = String::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("failed to parse: ") {
        let end = at + "failed to parse: ".len();
        folded.push_str(&rest[..end]);
        rest = &rest[end..];
        match rest.find("; using defaults") {
            Some(tail) => {
                folded.push_str("<ERROR>");
                rest = &rest[tail..];
            }
            None => break,
        }
    }
    folded.push_str(rest);
    folded
}

fn write_tree(base: &Path, files: &Value) {
    for (name, content) in files.as_object().expect("files object") {
        let path = base.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content.as_str().unwrap()).unwrap();
    }
}

#[test]
fn project_matches_the_python_goldens() {
    let cases: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project-cases.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut root = tmp.path().join("root");
    let mut home = tmp.path().join("home");
    for case in &cases {
        let label = case["label"].as_str().unwrap();
        if !case.get("reuse").and_then(Value::as_bool).unwrap_or(false) {
            root = tmp.path().join("root");
            home = tmp.path().join("home");
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(&home);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&home).unwrap();
        }
        if let Some(files) = case.get("home_files") {
            write_tree(&home, files);
        }
        if let Some(files) = case.get("root_files") {
            write_tree(&root, files);
        }
        let argv: Vec<String> = case["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                a.as_str()
                    .unwrap()
                    .replace("{ROOT}", &root.to_string_lossy())
                    .replace("{HOME}", &home.to_string_lossy())
            })
            .collect();
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["spawn-overlay", "project"])
            .args(&argv)
            .current_dir(tmp.path())
            .env("HOME", &home)
            // The frozen argv carries no --repo-root: the oracle runs pinned
            // the root explicitly, so the replay names the case's root the
            // same way or an init lands its config outside it.
            .env("FNO_REPO_ROOT", &root)
            .envs(fno_agents::test_run::self_owner_env())
            .output()
            .unwrap();
        let code = out.status.code();
        let mut streams = vec![normalize(
            &String::from_utf8_lossy(&out.stdout),
            &root,
            &home,
        )];
        if code != Some(0) {
            streams.push(normalize(
                &String::from_utf8_lossy(&out.stderr),
                &root,
                &home,
            ));
        }
        assert_golden(
            "project",
            label,
            &Golden {
                exit: code,
                streams,
            },
            None,
        );
        if let Some(file) = case["file_golden"].as_str() {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            assert_golden(
                "project",
                &format!("{label} file"),
                &Golden {
                    exit: None,
                    streams: vec![normalize(&text, &root, &home)],
                },
                None,
            );
        }
    }
}
