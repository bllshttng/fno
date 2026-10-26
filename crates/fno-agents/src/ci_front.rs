//! The native `fno do ci` namespace: per-commit CI state with per-check
//! names. The front door claims `do ci` lexically and execs this binary with
//! the argv intact, the same handover `do pr` uses. Every target form (PR
//! number, `main`, hex sha) reduces to one commit sha and one workflow-run
//! read, so a scratch script never hand-rolls the listing again. The PR
//! merge verdict stays `do pr status`; this command answers the narrower
//! question the census keeps filing: what ran at THIS sha, and what is red.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One spelling's row: (spelling, usage tail), the pr_front help shape.
const HELP_ROWS: &[(&str, &str)] = &[(
    "status <sha|main|pr> [--watch] [--interval 60] [--timeout 15m]",
    "One CI verdict at one commit: a hex sha, `main` (its head sha), or a PR number (its head sha; all-digits reads as a PR number). Prints a JSON line {target, kind, sha, verdict, settled, green, checks} with one checks row per workflow {name, status, conclusion, created_at}. Per workflow the newest run decides while it is in flight (pending); the newest completed run decides once it lands: fail or cancel reads red, success reads green; no rows at that sha reads pending, never green. exit 0 green, 1 red, 2 pending (with --watch: still unsettled at --timeout), 3 unresolvable target, 4 read failed. --watch re-reads every --interval seconds until settled or the bound, one stderr line per poll; it never cancels runs (do pr wait owns that).",
)];

fn print_help() {
    println!("CI utilities (per-commit workflow-run state)");
    println!();
    println!("Usage: fno do ci <spelling> [flags...]");
    println!();
    for (spelling, wording) in HELP_ROWS {
        println!("  {spelling}");
        println!("      {wording}");
        println!();
    }
}

/// One dispatchable target word.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Main,
    Pr(u64),
    Sha(String),
}

/// Classify one positional target. All-digits reads as a PR number (the
/// pr_operand precedent), so a short all-digit sha needs its full form or a
/// letter in it; `main` is reserved; anything else hex-ish 7..=40 is a sha.
pub(crate) fn classify_target(raw: &str) -> Option<Target> {
    if raw == "main" {
        return Some(Target::Main);
    }
    if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit()) {
        return raw.parse::<u64>().ok().map(Target::Pr);
    }
    let hexish = (7..=40).contains(&raw.len())
        && raw.chars().all(|c| c.is_ascii_hexdigit())
        && raw.chars().any(|c| c.is_ascii_alphabetic());
    hexish.then(|| Target::Sha(raw.to_ascii_lowercase()))
}

/// `--name value` / `--name=value` off the tail, the pr_front helper shape.
fn option_value(tail: &[String], name: &str) -> Option<String> {
    for (index, arg) in tail.iter().enumerate() {
        if arg == name {
            return tail.get(index + 1).cloned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            return Some(value.to_string());
        }
    }
    None
}

/// Resolve the target to its commit sha and the target kind the payload
/// names. A resolution read that fails is a read failure, not a usage error:
/// the argv was well formed to get here.
fn resolve_sha(cwd: &Path, target: &Target) -> Result<(String, &'static str), String> {
    match target {
        Target::Sha(sha) => Ok((sha.clone(), "sha")),
        Target::Main => {
            let raw = crate::pr_push::gh_api("gh", cwd, "repos/{owner}/{repo}/branches/main", &[])?;
            let parsed: Value = serde_json::from_str(&raw)
                .map_err(|e| format!("branches/main returned unparseable json: {e}"))?;
            let sha = parsed
                .get("head")
                .and_then(|h| h.get("sha"))
                .and_then(Value::as_str)
                .ok_or_else(|| "branches/main returned no head sha".to_string())?
                .to_string();
            Ok((sha, "branch"))
        }
        Target::Pr(number) => {
            let raw = crate::pr_push::gh_api(
                "gh",
                cwd,
                &format!("repos/{{owner}}/{{repo}}/pulls/{number}"),
                &[],
            )?;
            let parsed: Value = serde_json::from_str(&raw)
                .map_err(|e| format!("pulls/{number} returned unparseable json: {e}"))?;
            let sha = parsed
                .get("head")
                .and_then(|h| h.get("sha"))
                .and_then(Value::as_str)
                .ok_or_else(|| format!("pulls/{number} returned no head sha"))?
                .to_string();
            Ok((sha, "pr"))
        }
    }
}

/// Every workflow run at one sha, newest page only: a burst past 100 rows
/// outruns the per-sha read the way the branch page does in main_ci.
fn runs_at_sha(cwd: &Path, sha: &str) -> Result<Vec<Value>, String> {
    let raw = crate::pr_push::gh_api(
        "gh",
        cwd,
        &format!("repos/{{owner}}/{{repo}}/actions/runs?head_sha={sha}&per_page=100"),
        &[],
    )?;
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("actions/runs returned unparseable json: {e}"))?;
    Ok(parsed
        .get("workflow_runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// The verdict at one fold: (verdict, green, checks rows). Per workflow the
/// newest run decides while in flight; the newest completed run decides once
/// it lands, through the shared bucket vocabulary that never reads an
/// unknown conclusion green. No rows reads pending, never green: a sha with
/// no CI must never answer a green watcher.
pub(crate) fn verdict_from_folds(
    folds: &[(&str, &Value, Option<&Value>)],
) -> (String, bool, Vec<Value>) {
    let mut red = false;
    let mut pending = false;
    let mut checks = Vec::new();
    // The fold's newest-completed slot stays unused here on purpose: once
    // the newest run has landed it IS the newest completed one, so the
    // newest pointer alone decides, in flight or landed.
    for (name, newest, _newest_completed) in folds {
        let newest_status = newest.get("status").and_then(Value::as_str).unwrap_or("");
        let row = if newest_status != "completed" {
            pending = true;
            json!({
                "name": name,
                "status": newest_status,
                "conclusion": Value::Null,
                "created_at": newest.get("created_at").and_then(Value::as_str).unwrap_or(""),
            })
        } else {
            let conclusion = newest
                .get("conclusion")
                .and_then(Value::as_str)
                .unwrap_or("");
            match crate::pr_push::rest_bucket(newest) {
                "pass" | "skipping" => {}
                "fail" | "cancel" => red = true,
                _ => pending = true,
            }
            json!({
                "name": name,
                "status": "completed",
                "conclusion": conclusion,
                "created_at": newest.get("created_at").and_then(Value::as_str).unwrap_or(""),
            })
        };
        checks.push(row);
    }
    let verdict = if red {
        "red"
    } else if pending || folds.is_empty() {
        "pending"
    } else {
        "green"
    };
    (verdict.to_string(), verdict == "green", checks)
}

/// Fold one page of runs per workflow name and read the verdict off it.
pub(crate) fn verdict_from_runs(runs: &[Value]) -> (String, bool, Vec<Value>) {
    let mut folds: Vec<(&str, &Value, Option<&Value>)> = Vec::new();
    crate::main_ci::fold_page(&mut folds, runs.iter());
    verdict_from_folds(&folds)
}

/// One dispatch of `do ci status [tail...]`. Called from bin/client.rs for
/// argv whose first two words are `do ci`, with `args = argv[2..]`.
pub fn run(args: &[String]) -> i32 {
    let Some(sub) = args.first() else {
        print_help();
        return 0;
    };
    if sub == "--help" || sub == "-h" {
        print_help();
        return 0;
    }
    if sub != "status" {
        print_help();
        return 2;
    }
    let tail = &args[1..];
    if tail.iter().any(|arg| arg == "--help" || arg == "-h") {
        if let Some((spelling, wording)) = HELP_ROWS.first() {
            println!("Usage: fno do ci {spelling}");
            println!();
            println!("    {wording}");
            return 0;
        }
    }
    let Some(target_word) = tail.iter().find(|arg| !arg.starts_with("--")) else {
        eprintln!("fno do ci status: a target is required: <sha|main|pr>");
        return 3;
    };
    let Some(target) = classify_target(target_word) else {
        eprintln!(
            "fno do ci status: cannot read {target_word:?} as a target: \
             all-digits is a PR number, `main` is the branch, else 7..=40 hex \
             with at least one letter is a sha"
        );
        return 3;
    };
    let watch = tail.iter().any(|arg| arg == "--watch");
    let interval = option_value(tail, "--interval")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(60);
    let timeout = option_value(tail, "--timeout")
        .and_then(|v| crate::duration::parse_duration_secs(&v))
        .filter(|n| *n > 0)
        .unwrap_or(15 * 60);

    let cwd = PathBuf::from(
        std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".into()),
    );
    let sha = match resolve_sha(&cwd, &target) {
        Ok((sha, kind)) => (sha, kind),
        Err(error) => {
            eprintln!("fno do ci status: {error}");
            return 4;
        }
    };
    let target_word = target_word.to_string();

    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut poll = 0u64;
    loop {
        poll += 1;
        let runs = match runs_at_sha(&cwd, &sha.0) {
            Ok(runs) => runs,
            Err(error) => {
                eprintln!("fno do ci status: {error}");
                return 4;
            }
        };
        let (verdict, green, checks) = verdict_from_runs(&runs);
        let settled = verdict != "pending";
        if watch && !settled {
            eprintln!(
                "fno do ci status: pending at {} (poll {poll}, {}s left)",
                sha.0.get(..7).unwrap_or(&sha.0),
                deadline.saturating_duration_since(Instant::now()).as_secs()
            );
        }
        if settled || !watch || Instant::now() >= deadline {
            let payload = json!({
                "target": target_word,
                "kind": sha.1,
                "sha": sha.0,
                "verdict": verdict,
                "settled": settled,
                "green": green,
                "checks": checks,
            });
            println!("{}", serde_json::to_string(&payload).unwrap_or_default());
            return match verdict.as_str() {
                "green" => 0,
                "red" => 1,
                _ => 2,
            };
        }
        std::thread::sleep(Duration::from_secs(
            interval.min(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    .max(1),
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_row(name: &str, status: &str, conclusion: &str, created: &str) -> Value {
        json!({
            "name": name,
            "status": status,
            "conclusion": conclusion,
            "created_at": created,
            "workflow_id": 1,
            "event": "push",
            "head_sha": "abc",
        })
    }

    #[test]
    fn target_words_classify_by_the_documented_rule() {
        assert_eq!(classify_target("main"), Some(Target::Main));
        assert_eq!(classify_target("41"), Some(Target::Pr(41)));
        assert_eq!(classify_target("0"), Some(Target::Pr(0)));
        assert_eq!(
            classify_target("deadbeef123"),
            Some(Target::Sha("deadbeef123".into()))
        );
        assert_eq!(
            classify_target("DEADBEEF123"),
            Some(Target::Sha("deadbeef123".into()))
        );
        // All-digits stays a PR number even at sha-ish length.
        assert_eq!(classify_target("1234567"), Some(Target::Pr(1234567)));
        assert_eq!(classify_target("abc"), None);
        assert_eq!(classify_target(""), None);
        assert_eq!(classify_target("mainx"), None);
        assert_eq!(classify_target("dead beef"), None);
    }

    #[test]
    fn verdict_reads_green_only_when_every_workflow_completed_green() {
        let runs = vec![
            run_row("ci", "completed", "success", "2026-10-25T00:00:00Z"),
            run_row("wheels", "completed", "success", "2026-10-25T00:01:00Z"),
        ];
        let (verdict, green, checks) = verdict_from_runs(&runs);
        assert_eq!(verdict, "green");
        assert!(green);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0]["name"], "ci");
        assert_eq!(checks[0]["conclusion"], "success");
    }

    #[test]
    fn verdict_reads_red_on_the_newest_completed_failure_with_names() {
        let runs = vec![
            run_row("ci", "completed", "success", "2026-10-25T00:00:00Z"),
            run_row("wheels", "completed", "failure", "2026-10-25T00:01:00Z"),
        ];
        let (verdict, green, checks) = verdict_from_runs(&runs);
        assert_eq!(verdict, "red");
        assert!(!green);
        let wheels = checks.iter().find(|c| c["name"] == "wheels").unwrap();
        assert_eq!(wheels["conclusion"], "failure");
    }

    #[test]
    fn an_in_flight_newest_run_holds_its_workflow_pending() {
        let runs = vec![
            run_row("ci", "completed", "success", "2026-10-25T00:00:00Z"),
            // A newer run of the same workflow superseded the pass and is live.
            run_row("ci", "in_progress", "", "2026-10-25T00:02:00Z"),
        ];
        let (verdict, green, checks) = verdict_from_runs(&runs);
        assert_eq!(verdict, "pending");
        assert!(!green);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["status"], "in_progress");
        assert_eq!(checks[0]["conclusion"], Value::Null);
    }

    #[test]
    fn a_cancelled_completed_run_reads_red() {
        let runs = vec![run_row(
            "ci",
            "completed",
            "cancelled",
            "2026-10-25T00:00:00Z",
        )];
        let (verdict, green, _) = verdict_from_runs(&runs);
        assert_eq!(verdict, "red");
        assert!(!green);
    }

    #[test]
    fn no_rows_reads_pending_never_green() {
        let (verdict, green, checks) = verdict_from_runs(&[]);
        assert_eq!(verdict, "pending");
        assert!(!green);
        assert!(checks.is_empty());
    }

    #[test]
    fn an_unknown_conclusion_never_reads_green() {
        let runs = vec![run_row(
            "ci",
            "completed",
            "stale_beyond_vocabulary",
            "2026-10-25T00:00:00Z",
        )];
        let (verdict, green, _) = verdict_from_runs(&runs);
        assert_eq!(verdict, "pending");
        assert!(!green);
    }

    #[test]
    fn a_skipped_workflow_reads_neutral_not_pending() {
        let runs = vec![
            run_row("ci", "completed", "success", "2026-10-25T00:00:00Z"),
            run_row("release", "completed", "skipped", "2026-10-25T00:00:00Z"),
        ];
        let (verdict, green, _) = verdict_from_runs(&runs);
        assert_eq!(verdict, "green");
        assert!(green);
    }
}
