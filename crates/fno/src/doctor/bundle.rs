//! `fno doctor bundle <build|check|lint>`: the forwarding shape the deleted
//! `cli/src/fno/bundle/cli.py` answered. The bash scripts stay the single
//! source of truth for bundling logic; this leaf only resolves the repo
//! root, forwards, prints the greppable verdict line, and passes the exit
//! code through.

use std::path::PathBuf;

fn script_for(command: &str) -> Option<&'static str> {
    match command {
        "build" => Some("scripts/generate-skill-bundles.sh"),
        "check" => Some("scripts/lint/check-skill-bundles-fresh.sh"),
        "lint" => Some("scripts/lint/no-cross-skill-runtime-calls.sh"),
        _ => None,
    }
}

/// The `resolve_repo_root` precedence (fno/paths.py:96): the
/// `FNO_REPO_ROOT` pin, then `git rev-parse --show-toplevel`, then cwd.
pub(crate) fn resolve_repo_root() -> PathBuf {
    use std::process::Command;
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT") {
        let path = PathBuf::from(pin);
        if let Ok(resolved) = path.canonicalize() {
            return resolved;
        }
        return path;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output()
    {
        if out.status.code() == Some(0) {
            let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !top.is_empty() {
                return PathBuf::from(top);
            }
        }
    }
    cwd
}

fn forward(command: &str, extra: &[String]) -> i32 {
    let script = resolve_repo_root().join(script_for(command).unwrap_or("build"));
    if !script.is_file() {
        // Capability-accurate degrade (US3 / AC3-ERR): bundling operates on
        // the plugin's `skills/` source tree, which a bare `pip install fno`
        // does not ship. Name the missing capability and the install path.
        eprintln!(
            "fno doctor bundle {command}: needs the footnote plugin (skill sources), which \
a bare `pip install fno` does not ship - there is no skills/ tree to bundle.\n\
Install the plugin and run from its checkout:\n  clone the footnote repo, then run \
`claude --plugin-dir /path/to/footnote`\nOr set FNO_REPO_ROOT to an existing plugin \
checkout. (resolved, not on disk: {script})"
        );
        return 2;
    }
    let script = script.as_posix().to_string();
    let mut argv = vec!["bash".to_string(), script];
    argv.extend(extra.iter().cloned());
    match std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 127,
    }
}

/// The terminal verdict line on stdout, so a piped reader sees PASS/FAIL in
/// the content and not only in the exit code (bundle/cli.py:84-95).
fn verdict(command: &str, rc: i32) {
    let outcome = if rc == 0 { "PASS" } else { "FAIL" };
    let suffix = if rc == 0 {
        String::new()
    } else {
        format!(" (rc={rc})")
    };
    println!("fno doctor bundle {command}: {outcome}{suffix}");
}

pub fn run_bundle(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => forward("build", &args[..0.min(args.len())]),
        Some("build") => forward("build", &args[1..]),
        Some("check") => {
            let rc = forward("check", &args[1..]);
            verdict("check", rc);
            rc
        }
        Some("lint") => {
            let rc = forward("lint", &args[1..]);
            verdict("lint", rc);
            rc
        }
        Some(other) => {
            eprintln!(
                "Usage: fno doctor bundle [COMMAND]\nTry 'fno doctor bundle --help' for help.\n\nError: No such command '{other}'."
            );
            2
        }
    }
}
