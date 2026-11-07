//! `fno doctor lint shellout-drift`: no repo-root shell-out without a degrade
//! path. Ported from `cli/src/fno/lint_shellout_drift.py` (deleted). The
//! signal is the dominant idiom the Python leg caught: a module that
//! subprocess-exec bash/sh AND calls the shared repo-root resolver, naming a
//! `scripts/` or `hooks/` `.sh` path. Recorded departure: the detection is a
//! line scan, not an ast walk, so a list built on a line the scanner's
//! three idioms cannot see may be missed; the allowlist + degrade proof are
//! verbatim.

use super::Invocation;
use std::collections::BTreeMap;
use std::path::Path;

const SCAN_REL: &str = "cli/src/fno";
const ALLOWLIST_REL: &str = "scripts/lint/.clone-only-scripts.txt";
const DEGRADE_TIMEOUT_SECS: u64 = 30;

/// The Python leg's subprocess timeout, ported: spawn, poll, kill at the
/// deadline and read that as a hung probe. None is a timeout, never a pass.
fn bounded_output(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> Option<std::io::Result<std::process::Output>> {
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return Some(Err(e)),
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Some(child.wait_with_output()),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Some(Err(e)),
        }
    }
}
const SCAN_EXCLUDE: &[&str] = &["cli/src/fno/evals/runner.py"];

pub fn run(inv: &Invocation) -> i32 {
    match run_report(inv) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("shellout-drift: GUARD ERROR (fail-closed, check is red): {message}");
            3
        }
    }
}

fn run_report(inv: &Invocation) -> Result<i32, String> {
    let root = &inv.root;
    let scan_root = root.join(SCAN_REL);
    let allowlist_path = root.join(ALLOWLIST_REL);
    let entries = parse_allowlist(&allowlist_path)?;
    let allowed: Vec<&String> = entries.keys().collect();
    let violations = scan_tree(&scan_root, root, &allowed)?;
    let degrade_failures = if inv.no_degrade {
        Vec::new()
    } else {
        degrade_proof(&entries)
    };
    let mut lines: Vec<String> = Vec::new();
    for (file, lineno, relpath) in &violations {
        lines.push(format!(
            "{file}:{lineno}: repo-root shell-out to '{relpath}' is not on the \
             CLONE_ONLY_SCRIPTS allowlist."
        ));
        lines.push(
            "  Remedy 1 (preferred): eliminate the shell-out - fold the logic into \
             fno-agents (Rust) or internalize it into the package (Python)."
                .to_string(),
        );
        lines.push(format!(
            "  Remedy 2: if the command is intentionally clone-only, add '{relpath}' to \
             {ALLOWLIST_REL} with a degrade-proof command, and make the command exit \
             non-zero with an actionable message on a bare install."
        ));
    }
    for (relpath, command, reason) in &degrade_failures {
        lines.push(format!(
            "shellout-drift: allowlist entry '{relpath}' (command: fno {command}) failed \
             the degrade proof in a no-script env: {reason}."
        ));
        lines.push(
            "  An allowlisted clone-only command MUST degrade with a non-empty \
             actionable message (exit != 0 and != 127, no traceback)."
                .to_string(),
        );
    }
    if !violations.is_empty() || !degrade_failures.is_empty() {
        lines.insert(
            0,
            format!(
                "shellout-drift: FAIL - {} un-allowlisted repo-root shell-out(s), \
                 {} degrade-proof failure(s).",
                violations.len(),
                degrade_failures.len()
            ),
        );
        for line in &lines {
            eprintln!("{line}");
        }
        return Ok(1);
    }
    println!(
        "shellout-drift: ok - all repo-root shell-outs are on the CLONE_ONLY_SCRIPTS \
         allowlist and every allowlisted command degrades gracefully."
    );
    Ok(0)
}

/// Parse `relpath :: command args` lines; a malformed line is fail-closed.
pub(super) fn parse_allowlist(path: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| format!("allowlist not found: {}", path.display()))?;
    let mut entries = BTreeMap::new();
    for (no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((rel_part, cmd_part)) = line.split_once("::") else {
            return Err(format!(
                "{}:{}: malformed allowlist line (missing ' :: ' separator)",
                path.display(),
                no + 1
            ));
        };
        let relpath = rel_part.trim().to_string();
        let command: Vec<String> = cmd_part.split_whitespace().map(str::to_string).collect();
        if relpath.is_empty() || command.is_empty() {
            return Err(format!(
                "{}:{}: malformed allowlist line (empty path or command)",
                path.display(),
                no + 1
            ));
        }
        entries.insert(relpath, command);
    }
    Ok(entries)
}

/// The three line idioms: bash-exec call, shared resolver call, and a
/// scripts/hooks .sh path. A module must carry all three families (bash-exec
/// plus resolver) to flag; collect_relpaths flags every .sh literal in such
/// a module, not only those in argument position.
fn scan_tree(
    scan_root: &Path,
    root: &Path,
    allowed: &[&String],
) -> Result<Vec<(String, usize, String)>, String> {
    if !scan_root.is_dir() {
        return Err(format!("scan root not found: {}", scan_root.display()));
    }
    let bash_call = regex::Regex::new(
        r#"(?:subprocess\.)?(?:Popen|run|call|check_output|check_call)\([^)]*\[?\s*["'](bash|sh)["']"#,
    )
    .unwrap();
    let bash_bound =
        regex::Regex::new(r#"^\s*(?:\w+:\s*list\[str\]\s*=\s*)?\w+\s*=\s*\[\s*["'](bash|sh)["']"#)
            .unwrap();
    let resolver =
        regex::Regex::new(r"(^|[^A-Za-z0-9_])(?:resolve_repo_root|resolve_plugin_script)\s*\(")
            .unwrap();
    let sh_literal = regex::Regex::new(r#"["']((?:scripts|hooks)/[^"']*\.sh)["']"#).unwrap();
    let resolver_arg = regex::Regex::new(
        r#"resolve_plugin_script\(\s*(?:["'](?:scripts|hooks)/[^"']*\.sh["']|\w+)\s*\)"#,
    )
    .unwrap();
    let mut violations: Vec<(String, usize, String)> = Vec::new();
    let mut files = super::walkdir_py_sh(scan_root);
    files.sort();
    for path in files {
        let rel = super::state_roots::rel_path(root, &path);
        if SCAN_EXCLUDE.contains(&rel.as_str()) {
            continue;
        }
        let body = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", rel))?;
        let has_resolver = resolver.is_match(&body);
        if !has_resolver {
            continue;
        }
        let mut bash_exec = bash_call.is_match(&body);
        for line in body.lines() {
            if bash_bound.is_match(line) {
                bash_exec = true;
            }
        }
        if !bash_exec {
            continue;
        }
        let mut seen: Vec<String> = Vec::new();
        for (no, line) in body.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                continue;
            }
            let mut hit: Option<String> = None;
            for caps in sh_literal.captures_iter(line) {
                let v = caps[1].to_string();
                if !seen.contains(&v) {
                    seen.push(v.clone());
                    hit = Some(v);
                }
            }
            if let Some(caps) = resolver_arg.captures(line) {
                if let Some(m) = caps.get(1) {
                    let v = m
                        .as_str()
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string();
                    if v.starts_with("scripts/") || v.starts_with("hooks/") {
                        let v = v;
                        if v.ends_with(".sh") && !seen.contains(&v) {
                            seen.push(v.clone());
                            hit = Some(v);
                        }
                    }
                }
            }
            if let Some(relpath) = hit {
                if allowed.iter().any(|a| a.as_str() == relpath) {
                    continue;
                }
                violations.push((rel.clone(), no + 1, relpath));
            }
        }
    }
    Ok(violations)
}

/// The degrade proof: run each allowlisted command in a no-script env and
/// require a graceful fail (exit != 0 and != 127, non-empty stderr, no
/// traceback). The command base is the `fno` front beside this binary or on
/// PATH.
fn degrade_proof(entries: &BTreeMap<String, Vec<String>>) -> Vec<(String, String, String)> {
    let mut failures: Vec<(String, String, String)> = Vec::new();
    let Some(cmd_base) = resolve_fno() else {
        failures.push((
            "(all)".to_string(),
            String::new(),
            "could not locate the `fno` executable for the degrade proof (not beside \
             this binary and not on PATH)"
                .to_string(),
        ));
        return failures;
    };
    let empty = std::env::temp_dir().join(format!("fno-shellout-drift-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&empty);
    for (relpath, command) in entries {
        let envs: Vec<(String, String)> = vec![
            (
                "FNO_REPO_ROOT".to_string(),
                empty.to_string_lossy().into_owned(),
            ),
            ("FNO_HOME".to_string(), empty.to_string_lossy().into_owned()),
            ("HOME".to_string(), empty.to_string_lossy().into_owned()),
            (
                "USERPROFILE".to_string(),
                empty.to_string_lossy().into_owned(),
            ),
        ];
        let mut cmd = std::process::Command::new(&cmd_base);
        cmd.args(command)
            .current_dir(&empty)
            .env_remove("CLAUDE_PLUGIN_ROOT")
            .envs(envs);
        let outcome = bounded_output(
            &mut cmd,
            std::time::Duration::from_secs(DEGRADE_TIMEOUT_SECS),
        );
        match outcome {
            None => failures.push((
                relpath.clone(),
                command.join(" "),
                "timed out (hung instead of degrading)".into(),
            )),
            Some(Err(e)) => failures.push((
                relpath.clone(),
                command.join(" "),
                format!("could not run command: {e}"),
            )),
            Some(Ok(out)) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stderr),
                    String::from_utf8_lossy(&out.stdout)
                );
                let code = out.status.code().unwrap_or(-1);
                if code == 0 {
                    failures.push((
                        relpath.clone(),
                        command.join(" "),
                        "command exited 0 (did not fail when its script was absent)".into(),
                    ));
                } else if code == 127 {
                    failures.push((
                        relpath.clone(),
                        command.join(" "),
                        "command exited 127 (shell command-not-found, not a graceful degrade)"
                            .into(),
                    ));
                } else if stderr.is_empty() {
                    failures.push((
                        relpath.clone(),
                        command.join(" "),
                        format!("command exited {code} but printed nothing to stderr (silent)"),
                    ));
                } else if combined.contains("Traceback (most recent call last)") {
                    failures.push((
                        relpath.clone(),
                        command.join(" "),
                        format!(
                            "command exited {code} with a Python traceback (panic, not a degrade)"
                        ),
                    ));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&empty);
    failures
}

/// The `fno` front beside this binary, else `fno` on PATH, or None.
fn resolve_fno() -> Option<std::path::PathBuf> {
    let sibling = std::env::current_exe().ok()?.parent()?.join("fno");
    if sibling.is_file() {
        return Some(sibling);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("fno"))
        .find(|candidate| candidate.is_file())
}
