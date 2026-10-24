//! `fno config accounts test <id> [--smoke]`: check one account can launch.
//! The record must load, its harness binary must be on PATH, and an
//! `oauth_dir` record's credentials_source must exist. `--smoke` also runs
//! `<harness> --help` under the account's launch env.

use crate::accounts::add::split_flag;
use crate::accounts::env::{self, Sources};
use crate::accounts::records;
use crate::accounts::show::pwd;
use crate::loop_dispatch::which_binary;
use std::time::{Duration, Instant};

const HELP: &str = "Usage: fno config accounts test [OPTIONS] PROVIDER_ID

  Validate account config: record lookup + binary on PATH + credentials_source exists.

  With --smoke, also attempt a real CLI invocation (not fully implemented in Phase 02).

  --smoke  Attempt a real CLI invocation (costs quota)";

/// The Python smoke run's `subprocess.run(timeout=10)`.
const SMOKE_TIMEOUT_S: u64 = 10;

#[derive(Default)]
struct Args {
    id: Option<String>,
    smoke: bool,
}

fn parse(args: &[String]) -> Result<Option<Args>, String> {
    let mut out = Args::default();
    for arg in args {
        let (flag, inline) = split_flag(arg);
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--smoke" if inline.is_none() => out.smoke = true,
            f if f.starts_with('-') && f.len() > 1 => {
                return Err(format!("error: no such option: {f}"))
            }
            _ if out.id.is_none() => out.id = Some(arg.clone()),
            _ => return Err(format!("error: unexpected extra argument ({arg})")),
        }
    }
    Ok(Some(out))
}

/// `<harness> --help` under the launch env, the parent env beneath it.
fn smoke(harness: &str, launch: &std::collections::BTreeMap<String, String>) -> Result<(), String> {
    let mut cmd = std::process::Command::new(harness);
    cmd.arg("--help").envs(launch);
    let started = Instant::now();
    let out = match crate::bounded_cmd::output_with_timeout_result(cmd, SMOKE_TIMEOUT_S) {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "smoke test error: [Errno 2] No such file or directory: '{harness}'"
            ))
        }
        Err(e) => return Err(format!("smoke test error: {e}")),
    };
    if out.status.success() {
        return Ok(());
    }
    if started.elapsed() >= Duration::from_secs(SMOKE_TIMEOUT_S) {
        return Err(format!(
            "smoke test error: Command '['{harness}', '--help']' timed out after {SMOKE_TIMEOUT_S} seconds"
        ));
    }
    // Python reports a signal death as the negative signal number.
    let code = out.status.code().map_or_else(
        || {
            use std::os::unix::process::ExitStatusExt;
            -out.status.signal().unwrap_or(0)
        },
        |c| c,
    );
    Err(format!("smoke test: '{harness} --help' exited {code}"))
}

pub fn run(args: &[String]) -> i32 {
    let args = match parse(args) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{HELP}");
            return 0;
        }
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let Some(id) = args.id else {
        eprintln!("error: missing argument 'PROVIDER_ID'");
        return 2;
    };
    let cwd = pwd();
    let accounts = match records::load(Some(cwd.as_path()), &cwd) {
        Ok(accounts) => accounts,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let Some(record) = accounts.by_id(&id) else {
        eprintln!("error: account '{id}' not found");
        return 1;
    };
    if which_binary(&record.harness).is_none() {
        eprintln!("error: CLI binary '{}' is not on PATH", record.harness);
        return 1;
    }
    // The loader already refuses an oauth_dir record with no source.
    if record.auth == "oauth_dir" {
        if let Some(source) = record.credentials_source.as_ref().filter(|s| !s.exists()) {
            eprintln!(
                "error: credentials_source '{}' does not exist",
                source.display()
            );
            return 1;
        }
    }
    if args.smoke {
        let launch = match env::launch_env(&id, Some(cwd.as_path()), None, &cwd, &Sources::system())
        {
            Ok(launch) => launch,
            Err(e) => {
                eprintln!("smoke: dispatch_env failed for '{id}': {}", e.message);
                return 1;
            }
        };
        if let Err(msg) = smoke(&record.harness, &launch) {
            eprintln!("{msg}");
            return 1;
        }
    }
    println!("Account '{id}' looks OK.");
    0
}
