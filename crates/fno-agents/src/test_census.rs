//! --census-deferred: stop DISCOVERY_DEFERRED from silently holding green
//! harnesses. A passing-fast entry here is coverage loss no file edit
//! surfaces, so this runs every entry bounded, and fails the moment one
//! passes inside the tranche bound. It reads the set and reports; it never
//! edits it. Ported from test_cmd.py's census half.
use std::path::Path;
use std::time::{Duration, Instant};

use crate::test_smoke_registry as reg;

/// Deferred harness paths to census. DISCOVERY_DEFERRED is the source;
/// CENSUS_DEFERRED_FILE (one path per line, `#` comments allowed) overrides
/// it for a targeted run or a cheap check. Missing paths are returned
/// as-is: bash reports them rc 127, recorded as RED.
fn census_entries() -> Vec<String> {
    let Ok(override_file) = std::env::var("CENSUS_DEFERRED_FILE") else {
        return reg::DISCOVERY_DEFERRED.iter().map(|s| s.to_string()).collect();
    };
    let text = match std::fs::read_to_string(&override_file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("census: cannot read CENSUS_DEFERRED_FILE={override_file:?}: {e}");
            return Vec::new();
        }
    };
    let raw: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    let mut out = Vec::new();
    for line in raw {
        if seen.insert(line.clone()) {
            out.push(line);
        }
    }
    out.sort();
    out
}

/// Run cmd; on exceeding kill_bound_s, kill the whole process group.
/// Returns (returncode, elapsed_seconds, killed). Never the `timeout`
/// binary: that is absent on macOS (a recorded trap) and exits 127,
/// measuring nothing. setsid makes the child a group leader so the kill
/// reaches its grandchildren too, since a shell harness spawns subprocesses
/// a direct SIGKILL would orphan. The ladder is shared with the suite
/// runner.
fn run_bounded(
    argv: &[String],
    env: &std::collections::BTreeMap<String, String>,
    cwd: &Path,
    kill_bound_s: usize,
) -> (i32, f64, bool) {
    use std::process::Stdio;
    let start = Instant::now();
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]).current_dir(cwd).env_clear();
    for (k, v) in env {
        command.env(k, v);
    }
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    // SAFETY: setsid() is async-signal-safe and takes no arguments.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("census: failed to spawn {}: {e}", argv.join(" "));
            return (127, 0.0, false);
        }
    };
    let pgid = child.id() as i32;
    let deadline = start + Duration::from_secs(kill_bound_s as u64);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return (status.code().unwrap_or(1), start.elapsed().as_secs_f64(), false);
            }
            Ok(None) => {}
        }
        if Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        crate::test_run::cleanup_group(pgid);
        return (124, start.elapsed().as_secs_f64(), true);
    }
}

/// CENSUS_KILL_BOUND_S -> kill bound. Non-integer values are named and the
/// default used; below the tranche, a 60-70s-green harness is killed as
/// SLOW, so the tranche is the floor.
fn census_kill_bound(tranche: usize) -> usize {
    let raw = std::env::var("CENSUS_KILL_BOUND_S").ok();
    let bound = match &raw {
        None => reg::CENSUS_KILL_BOUND_S_DEFAULT,
        Some(text) => match text.parse::<usize>() {
            Ok(v) => v,
            Err(_) => {
                eprintln!(
                    "census: ignoring non-integer CENSUS_KILL_BOUND_S={text:?}, using {}",
                    reg::CENSUS_KILL_BOUND_S_DEFAULT
                );
                reg::CENSUS_KILL_BOUND_S_DEFAULT
            }
        },
    };
    if bound < tranche {
        tranche
    } else {
        bound
    }
}

/// Run every DISCOVERY_DEFERRED harness bounded; exit 1 if any is GREEN.
///
/// One tab-separated line per harness on stdout: verdict, rc, seconds, path.
/// Verdicts: GREEN (rc 0 and <= tranche bound), SLOW (over the tranche
/// bound, or killed by the run bound), RED (non-zero rc). Exit 1 if any
/// GREEN, 0 if all RED/SLOW, 2 if the deferred set is empty or
/// unparseable. Costs about 12 minutes on the full set, so run it on a
/// cadence, not on every PR; its finding is never caused by the PR under
/// review.
pub fn run(args: &[String]) -> i32 {
    let _ = args;
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = crate::test_verb::repo_root(&cwd).unwrap_or_else(|| cwd.clone());
    let tranche = reg::CENSUS_TRANCHE_BOUND_S;
    let kill_bound = census_kill_bound(tranche);
    let entries = census_entries();
    if entries.is_empty() {
        eprintln!("census: deferred set is empty or unparseable");
        return 2;
    }
    let env = match crate::test_smoke::smoke_env(&root) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let mut green: Vec<(String, f64)> = Vec::new();
    println!(
        "census: {} deferred entries, tranche={}s kill={}s",
        entries.len(),
        tranche,
        kill_bound
    );
    for rel in &entries {
        let argv = vec!["bash".to_string(), rel.clone()];
        let (rc, secs, killed) = run_bounded(&argv, &env, &root, kill_bound);
        let verdict = if killed {
            "SLOW"
        } else if rc == 0 {
            if secs <= tranche as f64 {
                green.push((rel.clone(), secs));
                "GREEN"
            } else {
                "SLOW"
            }
        } else {
            "RED"
        };
        println!("{verdict}\t{rc}\t{secs:.0}\t{rel}");
    }
    if !green.is_empty() {
        eprintln!(
            "census: GREEN deferred entries are silent coverage loss; delete their lines from DISCOVERY_DEFERRED (discovery then covers them):"
        );
        for (rel, secs) in &green {
            eprintln!("  {rel}  ({secs:.0}s)");
        }
        return 1;
    }
    eprintln!("census: no GREEN entries; {} held as RED/SLOW", entries.len());
    0
}
