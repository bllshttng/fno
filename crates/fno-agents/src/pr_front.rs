//! The native `fno do pr` namespace: one dispatcher for the 29 spellings the
//! Python front door carried, routed to their existing owners in this
//! binary, byte-verbatim where the Python leg forwarded (pr port 7). The
//! front door (crates/fno) claims `do pr` lexically and execs this binary
//! with the argv intact; watch and stub-manifest stay Python mounts. The
//! bare `pr` spelling lands here too, so the muscle-memory form survives
//! the wheel's pr_app deletion.

use serde_json::json;
use std::path::PathBuf;

/// The subcommands that stay Python mounts (the watcher's own surface, outside
/// this port's scope): decide_role leaves them unclaimed, and the
/// dispatcher refuses them by name so a hand-typed `fno-agents do pr watch`
/// cannot shadow the mount.
const PYTHON_MOUNTS: [&str; 2] = ["watch", "stub-manifest"];

/// One spelling's row: (spelling, usage tail). The grouped help lists every
/// registered name with its usage line; a spelling's own --help prints the
/// same row. The wording carries the exit-code contracts the Python --help
/// carried, compacted to one line each.
const HELP_ROWS: &[(&str, &str)] = &[
    ("attestation retract <args>...", "Revoke a passing review_attestation by naming the pair that passed (--reviewer, --attester, --head, --reason/-R); refused when no matching pass exists."),
    ("merge <pr_number> [flags]", "Merge a PR with the fno-canonical guards. Emits a JSON line {pr, outcome, reason, strategy[, cleanup]}; exit 0 merged, 1 failed, 2 skipped|held, 3 merge-landed-cleanup-failed, 127 gh-missing."),
    ("verify --kind merged|reviews --pr-number <n> --state-file <f>", "Verify an external PR gate: merged audits GitHub merge state (one bounded remediation); reviews flips the external-review gate when a reviewer has no qualifying reply. Exit 0 clean/degrade, 1 blocked/flipped, 2 substrate failure."),
    ("status <pr> [--refresh]", "One authoritative CI verdict from statusCheckRollup; prints a JSON line {pr, verdict, settled, green, checks}. exit 0 green, 1 red, 2 pending, 3 unknown, 4 read failed, 127 gh-missing. --refresh bypasses the coalescing cache for one live read (manual use only)."),
    ("wait <pr> [--until settled|red|green|review] [--timeout 15m] [--interval 60]", "The sanctioned watcher through the coalescing cache; exits the status command's own code. Red cancels the head's runs; a red made only of cancelled runs, or left by the attempt before a rerun, waits for the rerun; CONFLICTING exits 5 with a rebase receipt."),
    ("coverage-publish <pr> [--head H] [--repo R]", "Publish the review-coverage verdict for a PR head as the commit status fno/review-coverage. Never recomputes. exit 0 posted, 1 not posted."),
    ("info [<pr>] [--repo R]", "Read PR state, URL, head SHA, refs, and mergeability; the read runs in the Rust op door. Prints JSON; exit 4 when the read cannot answer."),
    ("list [--state open] [--repo R]", "List PR number, state, title, head ref, and URL through REST."),
    ("graphql-exec --purpose discretionary -- <gh argv>", "Run one gh GraphQL read under the reserve accounting. Everything after -- is gh's full argv, command words first; a flag-first argv is refused, because gh answers flags with no command word by printing its root command list."),
    ("logs [<pr>] [--job J] [--lines 40] [--full]", "Why did CI fail: spool the failing job's log to .fno/last-ci.log and print its last 40 lines. exit 0 green, 1 red, 2 pending, 3 no checks, 4 fetch error, 127 gh-missing."),
    ("heal [<pr>] [--apply] [--all] [--playbook]", "Classify a red check by signature, apply the mechanical fix, push once. exit 0 nothing red, 1 escalations remain, 2 a run in flight, 3 wrong branch or dirty tree, 4 read error, 127 binary missing."),
    ("base-check [--base origin/main]", "Refuse a PR whose branch base is > 24h of main history behind origin/main (phantom-deletion guard). exit 0 fresh|bypass|fail-open, 3 stale, 4 unrelated histories. Bypass with FNO_PR_BASE_OK=stale-acknowledged."),
    ("base-lineage-check <pr>", "Refuse a merge into a base branch that no longer leads to the default branch. exit 0 ok|bypassed, 3 stale (retarget the PR), 4 unknown. Bypass with FNO_PR_BASE_LINEAGE_OK=stale-acknowledged."),
    ("merge-result-check <pr>", "Compile the merge result of <pr> against its base: git merge-tree plus the repo-wide ruff + mypy step. exit 0 ok, 3 red, 4 unknown."),
    ("coverage-check <pr> [--recompute]", "The merge guard's coverage predicate for callers that cannot import it. exit 0 covered, 3 uncovered, 4 unanswered."),
    ("coverage-waive <pr> --reason R", "Record the attended operator waiver for ONE PR head, pinned to the live 40-hex head, so a push voids it. exit 0 recorded, 2 no reason, 3 refused provenance, 4 unreadable slug/head, 1 failed write."),
    ("review-hold <args>...", "Register, clear, or read the hold that says a review of this PR's head is RUNNING (check/acquire/release/metadata/round). Answered by the claim command's review-hold sub-token; flags pass through unchanged."),
    ("hold-check <pr> [--repo R]", "Refuse a PR whose bound plan ancestry carries an active or unreadable hold."),
    ("evidence-check [--allow-rebase-equivalent]", "Require a newest exact-HEAD full/passed verification receipt across project, global, and known delivery-root event journals."),
    ("evidence-required [--base origin/main]", "Expose the one local-verification policy to shell-based ship paths."),
    ("next-receipt-generation --candidate-sha <s>", "Derive the next receipt generation from every discovered journal."),
    ("global-receipt-events-path", "Print the durable cross-checkout verification receipt journal."),
    ("sync-canonical --pr-number <n>", "Post-merge canonical-checkout sync; exactly-once per merge SHA via a marker + single-flight lock; fail-open. exit 0 no-op/skipped/synced, non-zero on a failed sync_command or a dirty canonical checkout on touched paths."),
    ("publish-review <args>...", "Post a review verdict to GitHub as config.review.bot_identity (--pr-number N; --verdict defaults to the newest head-pinned attestation; --dry-run/-N). exit 0 posted, 1 skipped|refused|failed, 2 no attestation to default from."),
    ("rebase [--base B] [--continue]", "Rebase the current branch via the conflict-delegation protocol. Two-phase: exit 42 means the caller must invoke the conflict-resolver agent, then call back --continue."),
    ("push [flags]", "The one guarded push: fetch, rebase onto origin/main, optionally preflight, push exactly once to the PR head the upstream names, print one receipt with the remote ref. Exit 0 pushed (ci=settled|bypassed|red-cancel), 1 preflight red, 2 a live run in flight, 3 a refusal, 4 a read error, 127 binary missing."),
    ("create [flags]", "Duplicate-guarded create; exit 3 refuses a twin (--not-duplicate <pr>)."),
    ("ritual [<pr>] [--autonomous]", "Mechanical core of the post-merge ritual as one idempotent sequence with a per-leg receipt line. Hidden: the `fno do pr merged` skill is the attended front door."),
    ("closure-trailer [<node>] [--extra ID]...", "Print the exact `Fixes` closure line for NODE plus its contained_in descendants. Prints nothing (exit 0) when NODE is unresolvable or nothing well-formed remains. Exit 4 means the graph read itself failed."),
    ("bind-created --url U [--repo R] [--node N]", "Bind a raw `gh pr create` result to its one real node."),
];

/// Print the grouped help: one row per registered spelling. exit 0.
fn print_help() {
    println!("PR utilities (merge / verify / rebase via gh + git)");
    println!();
    println!("Usage: fno do pr <spelling> [flags...]");
    println!();
    for (spelling, wording) in HELP_ROWS {
        println!("  {spelling}");
        println!("      {wording}");
        println!();
    }
    println!(
        "  watch, stub-manifest      Python mounts under the same namespace (the watcher's surface)"
    );
}

/// The help row matching a spelling name exactly, for per-spelling --help.
fn spelling_help(sub: &str) -> Option<&'static str> {
    HELP_ROWS
        .iter()
        .find(|(name, _)| name.split_whitespace().next() == Some(sub))
        .map(|(_, wording)| *wording)
}

/// Parse `--purpose X` / `--purpose=X` off the graphql-exec tail; `Err` when
/// absent. The first bare `--` ends flag parsing and is dropped: everything
/// after it is gh's own argv, command words first.
fn parse_purpose(tail: &[String]) -> Result<(String, Vec<String>), String> {
    let mut purpose: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut index = 0;
    while index < tail.len() {
        let arg = &tail[index];
        if arg == "--" {
            rest.extend_from_slice(&tail[index + 1..]);
            break;
        }
        if arg == "--purpose" {
            purpose = tail.get(index + 1).cloned();
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--purpose=") {
            purpose = Some(value.to_string());
            index += 1;
            continue;
        }
        rest.push(arg.clone());
        index += 1;
    }
    match purpose {
        Some(purpose) => Ok((purpose, rest)),
        None => Err("graphql-exec: --purpose is required".to_string()),
    }
}

/// The one worktree-selection preamble the Python callback ran for five
/// spellings: resolve the argv's PR number, resolve its local worktree, and
/// chdir into it so the command reads the same checkout the Python door did.
/// "no local worktree" proceeds (the command's own checkout lists every
/// worktree of the repo); any other resolution failure is a usage refusal
/// (typer.BadParameter's exit 2).
fn select_worktree(tail: &[String]) -> Result<(), i32> {
    let pr = tail
        .iter()
        .find(|arg| !arg.is_empty() && arg.chars().all(|c| c.is_ascii_digit()))
        .map(String::as_str)
        .or_else(|| {
            tail.iter()
                .find_map(|arg| arg.strip_prefix("--pr-number=").filter(|v| !v.is_empty()))
        });
    let Some(pr) = pr else {
        return Ok(());
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match crate::pr_worktree::resolve_pr_with(&cwd, pr, "gh") {
        Ok(worktree) => {
            let _ = std::env::set_current_dir(&worktree);
            Ok(())
        }
        Err(reason) if reason.contains("no local worktree") => Ok(()),
        Err(reason) => {
            eprintln!("fno do pr: {reason}");
            Err(2)
        }
    }
}

/// Dispatch one `do pr <subcommand> [tail...]` argv in process. Called from
/// bin/client.rs's main for argv whose first two words are `do pr`, with
/// `args = argv[2..]`.
pub fn run(args: &[String]) -> i32 {
    let Some(sub) = args.first() else {
        print_help();
        return 0;
    };
    if PYTHON_MOUNTS.contains(&sub.as_str()) {
        eprintln!(
            "fno do pr {sub}: the Python mount answers this spelling; run it \
through the fno front door"
        );
        return 2;
    }
    if sub == "--help" || sub == "-h" {
        print_help();
        return 0;
    }
    let tail = &args[1..];
    if tail.iter().any(|arg| arg == "--help" || arg == "-h") {
        if let Some(wording) = spelling_help(sub) {
            println!("Usage: fno do pr {sub}");
            println!();
            println!("    {wording}");
            return 0;
        }
    }
    if matches!(
        sub.as_str(),
        "verify" | "status" | "base-lineage-check" | "merge-result-check" | "coverage-check"
    ) {
        if let Err(code) = select_worktree(tail) {
            return code;
        }
    }
    dispatch(sub, tail)
}

/// Current directory as a string (the op payloads' cwd field).
fn current_cwd_string() -> String {
    std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// The first all-digits tail token, the positional PR operand.
fn pr_operand(tail: &[String]) -> Option<u64> {
    tail.iter()
        .find(|arg| !arg.is_empty() && arg.chars().all(|c| c.is_ascii_digit()))
        .and_then(|arg| arg.parse().ok())
}

/// `--name value` / `--name=value` off the tail.
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

/// Prepend a token (the owner command's sub-command word).
fn bound_tail(word: &str, tail: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = vec![word.to_string()];
    argv.extend(tail.iter().cloned());
    argv
}

/// The routing table proper. Every arm re-spells onto the owner command the
/// Python leg forwarded to; only info and graphql-exec wire new machinery.
fn dispatch(sub: &str, tail: &[String]) -> i32 {
    match sub {
        "merge" | "hold-check" => {
            crate::authorized_merge::run_authorized_merge(&bound_tail(sub, tail))
        }
        "verify" => crate::pr_verify::run(tail),
        "status" => {
            let payload = json!({
                "op": "status-read",
                "cwd": current_cwd_string(),
                "pr": pr_operand(tail).unwrap_or(0),
                "refresh": tail.iter().any(|a| a == "--refresh" || a == "--no-cache"),
            });
            crate::authorized_merge::run_authorized_merge(&[payload.to_string()])
        }
        "wait" => {
            let payload = json!({
                "op": "status-wait",
                "cwd": current_cwd_string(),
                "pr": pr_operand(tail).unwrap_or(0),
                "until": option_value(tail, "--until")
                    .unwrap_or_else(|| "settled".to_string()),
                "timeout": option_value(tail, "--timeout")
                    .unwrap_or_else(|| "15m".to_string()),
                "interval": option_value(tail, "--interval")
                    .unwrap_or_else(|| "60".to_string()),
            });
            crate::authorized_merge::run_authorized_merge(&[payload.to_string()])
        }
        "coverage-publish" => {
            let pr = pr_operand(tail).unwrap_or_default().to_string();
            let mut argv = vec!["publish".to_string(), pr];
            if let Some(head) = option_value(tail, "--head") {
                argv.extend(["--head".to_string(), head]);
            }
            if let Some(repo) = option_value(tail, "--repo") {
                argv.extend(["--cwd".to_string(), repo]);
            }
            crate::loopcheck::run_review_coverage(&argv)
        }
        "info" => {
            let mut ask = json!({
                "op": "status-pr-info",
                "cwd": current_cwd_string(),
            });
            if let Some(pr) = pr_operand(tail) {
                ask["pr"] = json!(pr);
            }
            if let Some(repo) = option_value(tail, "--repo") {
                ask["repo"] = json!(repo);
            }
            let answer = crate::pr_info::run_op(&ask);
            if let Ok(line) = serde_json::to_string(&answer) {
                println!("{line}");
            }
            if answer.get("error").is_some() {
                4
            } else {
                0
            }
        }
        "list" => crate::pr_list::run_pr_list(tail),
        "graphql-exec" => match parse_purpose(tail) {
            Ok((purpose, rest)) => crate::pr_status::quota::run_graphql_exec_args(&purpose, &rest),
            Err(msg) => {
                eprintln!("{msg}");
                2
            }
        },
        "logs" => {
            let payload = json!({
                "op": "status-logs",
                "cwd": current_cwd_string(),
                "pr": pr_operand(tail).unwrap_or(0),
                "job": option_value(tail, "--job"),
                "lines": option_value(tail, "--lines")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(40),
                "full": tail.iter().any(|a| a == "--full"),
            });
            crate::authorized_merge::run_authorized_merge(&[payload.to_string()])
        }
        "heal" => crate::heal::run_heal(tail),
        "base-check" => {
            if tail.is_empty() {
                crate::pr_base_check::run_stdin()
            } else {
                crate::pr_base_check::run(tail)
            }
        }
        "base-lineage-check" => crate::gate_probes::run_base_lineage_command(tail),
        "merge-result-check" => crate::pr_merge_result::run_merge_result_command(tail),
        "coverage-check" => {
            let pr = pr_operand(tail).unwrap_or_default().to_string();
            let mut argv = vec!["check".to_string(), pr];
            if tail.iter().any(|a| a == "--recompute") {
                argv.push("--recompute".to_string());
            }
            crate::loopcheck::run_review_coverage(&argv)
        }
        "coverage-waive" => {
            let pr = pr_operand(tail).unwrap_or_default().to_string();
            let mut argv = vec!["waive".to_string(), pr];
            if let Some(reason) = option_value(tail, "--reason") {
                argv.extend(["--reason".to_string(), reason]);
            }
            crate::loopcheck::run_review_coverage(&argv)
        }
        "review-hold" => crate::claim_commands::run_claim(&bound_tail("review-hold", tail)),
        "evidence-check" => crate::verify_evidence::run_verify_evidence(tail),
        "evidence-required" => {
            crate::verify_evidence::run_verify_evidence(&bound_tail("required", tail))
        }
        "next-receipt-generation" => {
            crate::verify_evidence::run_verify_evidence(&bound_tail("next-generation", tail))
        }
        "global-receipt-events-path" => {
            crate::verify_evidence::run_verify_evidence(&bound_tail("journal-path", tail))
        }
        "sync-canonical" => sync_canonical(tail),
        "publish-review" => crate::publish_review::run_publish_review(tail),
        "rebase" => crate::pr_rebase::run_rebase(tail),
        "push" => crate::pr_push::run_push(tail),
        "create" => crate::pr_create::run_pr_create_command(tail),
        "ritual" => crate::pr_watch::run(&bound_tail("ritual", tail)),
        "closure-trailer" => {
            crate::org_board::pr_closure::run_command(&bound_tail("pr-closure-trailer", tail))
        }
        "bind-created" => {
            let payload = json!({
                "url": option_value(tail, "--url"),
                "repo": option_value(tail, "--repo"),
                "node": option_value(tail, "--node"),
            });
            crate::org_board::pr_closure::run_command(&bound_tail(
                "pr-closure-bind-created",
                &[payload.to_string()],
            ))
        }
        "attestation" => {
            // `fno do pr attestation retract <args>` re-spells onto the
            // attest door's retract token; the flags pass through unchanged.
            match tail.first() {
                Some(first) if first == "retract" => crate::attest::dispatch(tail),
                _ => {
                    print_help();
                    2
                }
            }
        }
        _ => {
            print_help();
            2
        }
    }
}

/// The post-merge canonical sync through its own door's internals: build the
/// command's payload shape and run the sync leg directly (the stdin door the
/// Python transport used, without the round trip).
fn sync_canonical(tail: &[String]) -> i32 {
    let Some(pr) = option_value(tail, "--pr-number").and_then(|v| v.parse::<u64>().ok()) else {
        eprintln!("fno do pr sync-canonical: --pr-number is required");
        return 2;
    };
    let payload = json!({
        "action": "sync",
        "cwd": current_cwd_string(),
        "pr": pr,
    });
    let (exit, out_lines, err_lines) = crate::sync_canonical::run_sync_payload(&payload);
    for line in out_lines {
        println!("{line}");
    }
    for line in err_lines {
        eprintln!("{line}");
    }
    exit
}

#[cfg(test)]
mod graphql_exec_argv_tests {
    use super::parse_purpose;
    use crate::pr_status::quota::command_args;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn the_separator_never_reaches_gh_argv() {
        // The exact tail pr_status/reviews.rs builds for its reader.
        let tail = argv(&[
            "--purpose",
            "discretionary",
            "--",
            "api",
            "graphql",
            "-f",
            "query=q",
            "--jq",
            ".x",
        ]);
        let (purpose, rest) = parse_purpose(&tail).unwrap();
        assert_eq!(purpose, "discretionary");
        assert_eq!(
            rest,
            argv(&["api", "graphql", "-f", "query=q", "--jq", ".x"])
        );
        assert_eq!(command_args(&rest)[0], "api");
    }

    #[test]
    fn a_purpose_flag_after_the_separator_belongs_to_gh() {
        let tail = argv(&["--purpose=coverage", "--", "api", "--purpose", "x"]);
        let (purpose, rest) = parse_purpose(&tail).unwrap();
        assert_eq!(purpose, "coverage");
        assert_eq!(rest, argv(&["api", "--purpose", "x"]));
    }
}
