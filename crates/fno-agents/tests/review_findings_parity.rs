//! parity-stage: characterization
//! parity-oracle: fno.review.findings
//!
//! Golden parity for the `review_findings` port (the finding classifier,
//! the bounded record, and the per-round disposition comment), frozen
//! against the live Python leg before its deletion. The case table lives in
//! `golden/review_findings/cases.json`, written by
//! `review_findings_golden_driver.py` alongside the per-case fixtures and
//! the `gh_stub.sh`; this file is its lockstep consumer. The driver and
//! this file share two normalizers, applied to the same places on both
//! sides:
//!
//! - every ` --paginate` token drops from the argv log (the native comments
//!   read walks every page; the Python leg read one), and
//! - every `gh proxy: <sentence>` segment cuts from the streams (the
//!   Python leg's gh wrapper noise, which the native leg never has).
//!
//! Two wrapper-shaped cases ("post pr read fails", "post non github origin")
//! had a reason TAIL built from that machine's wrapper noise, so the capture
//! cut it to nothing and the table pins only the sentence's `err_prefix`;
//! the prefix rules whenever the table declares one, exact `.err` otherwise.

mod common;

use common::golden_dir;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Every prefix that can leak machine identity into the config walk or the
/// review round. The capture driver scrubbed the same set.
const SCRUB_PREFIXES: [&str; 10] = [
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

fn slug(label: &str) -> String {
    let mut out = String::new();
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

fn git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git spawns")
        .success();
    assert!(ok, "git {args:?} failed in {}", repo.display());
}

fn strip_paginate(text: &str) -> String {
    text.replace(" --paginate", "")
}

/// Cut `gh proxy: <sentence>` segments, newline included. The Rust leg has
/// none of this noise; the cut keeps the two sides' normalizers in
/// lockstep, as the driver's doc comment states.
fn strip_wrapper_noise(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("gh proxy: ") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        match after.find('\n') {
            Some(j) => rest = &after[j + 1..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

fn fs_read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

struct Runtime {
    dir: PathBuf,
    repo: PathBuf,
    log: PathBuf,
    body: PathBuf,
}

fn build_runtime(case: &Value, base: &Path, key: &str, stub: &Path) -> Runtime {
    let dir = base.join(key);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home/.fno")).unwrap();
    std::fs::write(dir.join("home/.fno/config.toml"), "").unwrap();
    std::fs::write(dir.join("home/.fno/.path-migration-done"), "").unwrap();
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let origin = case["origin"].as_str().unwrap_or("github");
    match origin {
        "github" => git(
            &repo,
            &["remote", "add", "origin", "https://github.com/o/r.git"],
        ),
        "gitlab" => git(
            &repo,
            &["remote", "add", "origin", "https://gitlab.com/o/r.git"],
        ),
        _ => {}
    }
    if case["detached"].as_bool().unwrap_or(false) {
        git(
            &repo,
            &[
                "-c",
                "user.email=t@example.test",
                "-c",
                "user.name=t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "base",
            ],
        );
        git(&repo, &["checkout", "-q", "--detach"]);
    }
    let bindir = dir.join("bin");
    std::fs::create_dir(&bindir).unwrap();
    let gh = bindir.join("gh");
    std::fs::copy(stub, &gh).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&gh).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&gh, perms).unwrap();
    }
    Runtime {
        log: dir.join("gh.log"),
        body: dir.join("gh-body.txt"),
        dir,
        repo,
    }
}

/// Pin the child's env exactly as the capture driver did: scrub the
/// machine-identity prefixes, pin HOME/GIT config, FNO_CONFIG at the case's
/// own config.toml when the case carries one, the case's env stamp, and the
/// fake gh's log/body/case paths.
fn pinned_command(
    case: &Value,
    case_dir: &Path,
    runtime: &Runtime,
    bin: &Path,
    action: &str,
) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("review-level").arg("findings").arg(action);
    if let Some(args) = case["args"].as_array() {
        for arg in args {
            cmd.arg(arg.as_str().unwrap());
        }
    }
    if action == "record" {
        // The capture driver appended the fixture path itself (Python's
        // classify takes only --findings-file), so record cases record
        // empty argv; the native door takes the same implicit file.
        cmd.arg("--findings-file").arg("findings.json");
    }
    for (k, _) in std::env::vars_os() {
        let text = k.to_string_lossy().to_string();
        if SCRUB_PREFIXES.iter().any(|p| text.starts_with(p)) {
            cmd.env_remove(&k);
        }
    }
    cmd.env("HOME", runtime.dir.join("home"));
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
    let config = case_dir.join("config.toml");
    if config.exists() {
        cmd.env("FNO_CONFIG", &config);
    }
    if let Some(env) = case["env"].as_object() {
        for (k, v) in env {
            cmd.env(k, v.as_str().unwrap_or_default());
        }
    }
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            runtime.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    cmd.env("FAKE_GH_LOG", &runtime.log);
    cmd.env("FAKE_GH_BODY", &runtime.body);
    cmd.env("FAKE_GH_CASE", case_dir);
    // Every client spawn a test file makes is test-owned: the keeper reaper
    // can attribute (and reap) anything the door leaves behind.
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.current_dir(&runtime.repo);
    cmd
}

fn compare_case(bin: &Path, base: &Path, case: &Value, problems: &mut Vec<String>) {
    let label = case["label"].as_str().unwrap();
    let key = slug(label);
    let action = case["action"].as_str().unwrap();
    let golden = golden_dir("review_findings");
    let case_dir = golden.join("cases").join(&key);
    let stub = golden.join("gh_stub.sh");
    let runtime = build_runtime(case, base, &key, &stub);
    // The findings file rides the committed case fixtures.
    std::fs::copy(
        case_dir.join("findings.json"),
        runtime.repo.join("findings.json"),
    )
    .unwrap();
    let mut cmd = pinned_command(case, &case_dir, &runtime, bin, action);
    let out = cmd.output().expect("fno-agents binary spawns");
    let stdout = strip_wrapper_noise(&String::from_utf8_lossy(&out.stdout));
    let stderr = strip_wrapper_noise(&String::from_utf8_lossy(&out.stderr));

    // Every diff this case shows is COLLECTED, not panicked: one CI round
    // reports the whole table, not its first row.
    let mut bad = |msg: String| problems.push(format!("[{label}] {msg}"));

    // exit
    let exit_path = golden.join(format!("{key}.exit"));
    let want_exit: i32 = match fs_read(&exit_path) {
        Some(text) => match text.trim().parse() {
            Ok(parsed) => parsed,
            Err(_) => {
                bad(format!("golden exit does not parse: {text:?}"));
                return;
            }
        },
        None => {
            bad(format!("missing golden {}", exit_path.display()));
            return;
        }
    };
    if out.status.code() != Some(want_exit) {
        bad(format!(
            "exit differs: golden {want_exit}, rust {:?}",
            out.status.code()
        ));
    }
    // stdout
    let out_path = golden.join(format!("{key}.out"));
    match fs_read(&out_path) {
        Some(want) => {
            if stdout != want {
                bad(format!(
                    "stdout differs\ngolden={want:?}\nrust  ={stdout:?}"
                ));
            }
        }
        None => bad(format!("missing golden {}", out_path.display())),
    }
    // stderr: the table's declared prefix RULES when it declares one (two
    // wrapper-shaped cases had machine-noise reason tails the capture cut to
    // nothing), else exact when frozen, else free.
    if let Some(prefix) = case["err_prefix"].as_str() {
        if !stderr.starts_with(prefix) {
            bad(format!(
                "stderr lacks the frozen prefix\nprefix={prefix:?}\nrust={stderr:?}"
            ));
        }
    } else {
        let err_path = golden.join(format!("{key}.err"));
        if let Some(want) = fs_read(&err_path) {
            if stderr != want {
                bad(format!(
                    "stderr differs\ngolden={want:?}\nrust  ={stderr:?}"
                ));
            }
        }
    }
    // argv log: compared with the paginate token dropped; a case with no
    // golden log must produce none.
    let own_log = fs_read(&runtime.log).map(|t| strip_paginate(&strip_wrapper_noise(&t)));
    let log_golden = fs_read(&golden.join(format!("{key}.log")));
    match (log_golden, own_log) {
        (Some(want), Some(got)) => {
            if want != got {
                bad(format!("log differs\ngolden={want:?}\nrust  ={got:?}"));
            }
        }
        (Some(want), None) => bad(format!(
            "a golden log exists but the leg ran no gh: golden={want:?}"
        )),
        (None, Some(got)) => bad(format!("no golden log but the leg ran gh: {got:?}")),
        (None, None) => {}
    }
    // POST body
    let own_body = fs_read(&runtime.body);
    let body_golden = fs_read(&golden.join(format!("{key}.body")));
    match (body_golden, own_body) {
        (Some(want), Some(got)) => {
            if want != got {
                bad(format!(
                    "POST body differs\ngolden={want:?}\nrust  ={got:?}"
                ));
            }
        }
        (Some(want), None) => bad(format!(
            "a golden body exists but the leg posted nothing: golden={want:?}"
        )),
        (None, Some(got)) => bad(format!("no golden body but the leg posted: {got:?}")),
        (None, None) => {}
    }
}

fn cases() -> Vec<Value> {
    let path = golden_dir("review_findings").join("cases.json");
    serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cases.json unreadable: {e}")),
    )
    .expect("cases.json parses")
}

/// The record golden table.
#[test]
fn review_findings_record_golden_parity() {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_fno-agents"));
    let base = std::env::temp_dir().join(format!("review-findings-record-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let mut ran = 0;
    let mut problems: Vec<String> = Vec::new();
    for case in cases() {
        if case["action"].as_str() == Some("record") {
            compare_case(&bin, &base, &case, &mut problems);
            ran += 1;
        }
    }
    assert!(ran > 0, "the record table ran no case: cases.json lost it");
    assert!(
        problems.is_empty(),
        "{} record case(s) differ from the goldens:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// The comment golden table, page walk included.
#[test]
fn review_findings_post_golden_parity() {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_fno-agents"));
    let base = std::env::temp_dir().join(format!("review-findings-post-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let mut ran = 0;
    let mut problems: Vec<String> = Vec::new();
    for case in cases() {
        if case["action"].as_str() == Some("post") {
            compare_case(&bin, &base, &case, &mut problems);
            ran += 1;
        }
    }
    assert!(ran > 0, "the post table ran no case: cases.json lost it");
    assert!(
        problems.is_empty(),
        "{} post case(s) differ from the goldens:\n{}",
        problems.len(),
        problems.join("\n")
    );
}
