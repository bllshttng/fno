"""fno do pr CLI - in-package gh/git PR operations.

Verbs:
    merge  - merge a PR with the fno-canonical guards (-> _merge.py)
    verify - audit an external PR gate, merged|reviews (-> _verify.py)
    rebase - two-phase rebase with conflict delegation (-> fno-agents pr-rebase)
    push   - the one guarded push: fetch, rebase, preflight, push once (-> fno-agents pr-push)
    logs   - tail the failing CI job, spool the rest (-> _logs.py)
    wait   - poll status through the coalescing cache until settled/green (-> _wait.py)

The four ``scripts/lib/pr-*.sh`` were ported to in-package Python shelling to
gh/git, so these verbs run from a bare ``pip install fno`` with no repo-root
dependency. Each module preserves the bash exit-code / output contract.
"""

from __future__ import annotations

import enum
import json
import os
import sys
from typing import List, Optional

import typer



pr_app = typer.Typer(
    name="pr",
    help="PR utilities (merge / verify / rebase via gh + git)",
    no_args_is_help=True,
    add_completion=False,
)


@pr_app.callback()
def _select_pr_worktree(ctx: typer.Context) -> None:
    if not (command := ctx.invoked_subcommand) or command not in {"verify", "status", "base-lineage-check", "merge-result-check", "coverage-check"}:
        return
    if pr := next((arg.partition("=")[2] if arg.startswith("--pr-number=") else arg for arg in sys.argv[sys.argv.index(command) + 1:] if arg.isdigit() or arg.startswith("--pr-number=")), None):
        from fno.rust_binary import VerbUnavailable, verb_call

        try:
            answer = verb_call("pr-worktree", {"pr": int(pr), "cwd": os.getcwd()})
            worktree = str(answer.get("worktree") or "")
        except VerbUnavailable as exc:
            # "" = no local worktree on the PR branch: nothing to select, and
            # the command's own checkout lists every worktree of the repo.
            if exc.returncode == 3 and "no local worktree" in str(exc):
                return
            raise typer.BadParameter(str(exc)) from exc
        except Exception as exc:
            raise typer.BadParameter(str(exc)) from exc
        # "" = no local worktree on the PR branch: nothing to select, and the
        # command's own checkout lists every worktree of the repo anyway.
        if worktree:
            os.chdir(worktree)


class VerifyKind(str, enum.Enum):
    merged = "merged"
    reviews = "reviews"


class GraphqlPurpose(str, enum.Enum):
    discretionary = "discretionary"


attestation_app = typer.Typer(
    name="attestation",
    help="Retraction for review attestations (the event, not the identity).",
    no_args_is_help=True,
    add_completion=False,
)


@attestation_app.command(
    "retract",
    hidden=True,
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
    help=(
        "Revoke a passing review_attestation by naming the pair that passed: "
        "--reviewer, --attester (the session that emitted the pass), --head, "
        "--reason/-R. Writes a fail verdict carrying retracts_attester and "
        "records the RETRACTING process's own identity. Refuses when no "
        "matching pass exists, so a revocation never lands for a pair that "
        "never passed."
    ),
)
def attestation_retract(ctx: typer.Context) -> None:
    _forward_to_binary("attest", ["retract", *ctx.args])


pr_app.add_typer(attestation_app, name="attestation", hidden=True)


@pr_app.command(
    "merge",
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
    help=(
        "Merge a PR via gh CLI with the fno-canonical guards (<pr_number>). "
        "Emits a JSON line {pr, outcome, reason, strategy[, cleanup]}; exit 0 merged, "
        "1 failed, 2 skipped|held, 3 merge-landed-cleanup-failed, 127 gh-missing."
    ),
)
def merge(ctx: typer.Context) -> None:
    _forward_to_binary("authorized-merge", ["merge", *ctx.args])


@pr_app.command(
    "verify",
    help=(
        "Verify an external PR gate. --kind merged audits GitHub merge state "
        "(with a single bounded remediation); --kind reviews flips the "
        "external-review gate when a reviewer has no qualifying reply. "
        "Exit 0 clean/degrade, 1 blocked/flipped, 2 substrate failure."
    ),
)
def verify(
    kind: VerifyKind = typer.Option(..., help="Gate to verify: merged | reviews"),
    pr_number: int = typer.Option(..., "--pr-number", help="GitHub PR number"),
    state_file: str = typer.Option(..., "--state-file", help="Path to target-state.md"),
) -> None:
    _forward_to_binary(
        "pr-verify",
        ["--kind", kind.value, "--pr-number", str(pr_number), "--state-file", state_file],
    )


@pr_app.command(
    "status",
    help=(
        "One authoritative CI verdict for a PR from statusCheckRollup. Prints a "
        "JSON line {pr, verdict, settled, green, checks}; exit 0 green, 1 red, "
        "2 pending, 3 unknown (no checks), 4 read failed (fetch error or "
        "reader crash; stdout still carries one JSON line), 127 gh-missing. "
        "In-progress checks read as pending, never red. settled is true only "
        "when every latest run carries a real conclusion, so a cancelled run "
        "reads red AND unsettled. checks.total counts the whole rollup: "
        "checks.check_runs and checks.statuses split it, because "
        "`gh api .../check-runs` returns only the first kind. A served answer "
        "carries cached, cached_at and cached_age_seconds beside the head "
        "it was computed at; "
        "--refresh bypasses the cache for one live read."
    ),
)
def status(
    pr_number: int = typer.Argument(..., help="GitHub PR number"),
    refresh: bool = typer.Option(
        False,
        "--refresh",
        "--no-cache",
        help=(
            "Bypass the coalescing cache and read GitHub live. Manual use "
            "only - it defeats the coalescing that keeps a watcher fleet "
            "under the REST secondary limit, so never put it in a poll loop."
        ),
    ),
) -> None:
    _forward_to_binary(
        "authorized-merge",
        [json.dumps({"op": "status-read", "cwd": os.getcwd(), "pr": pr_number, "refresh": refresh})],
    )


@pr_app.command(
    "wait",
    hidden=True,
    help=(
        "The sanctioned watcher: poll `fno do pr status` through the "
        "coalescing cache until the PR settles (or turns green with "
        "--until green), then exit the status verb's own code. --until review "
        "wakes when a new review posts on the PR. N waiters on "
        "one PR cost one network read per cache TTL, a rate-limit backoff is "
        "ridden out rather than hammered, and the gh-call count prints at "
        "exit. --timeout (15m default) exits with the last observed code and "
        "a still-unsettled note. Red cancels the head's runs, exits 1; a red made only of "
        "cancelled runs waits for the rerun instead; CONFLICTING exits 5 with a "
        "rebase receipt. Use this instead of a hand-rolled "
        "`while/sleep/grep` loop - every such loop is an uncoordinated poll "
        "against a quota the whole machine shares."
    ),
)
def wait(
    pr_number: int = typer.Argument(..., help="GitHub PR number"),
    until: str = typer.Option(
        "settled", "--until",
        help="settled (terminal, or red at once), red (fail fast), green (every check), review.",
    ),
    timeout: str = typer.Option("15m", "--timeout", help="Max wait, e.g. 30m / 90s / 1h."),
    interval: str = typer.Option("60", "--interval", help="Poll interval in seconds (minimum 5)."),
) -> None:
    _forward_to_binary(
        "authorized-merge",
        [json.dumps({"op": "status-wait", "cwd": os.getcwd(), "pr": pr_number, "until": until, "timeout": timeout, "interval": interval})],
    )


@pr_app.command(
    "coverage-publish",
    hidden=True,
    help=(
        "Publish the review-coverage verdict for a PR head as the commit "
        "status fno/review-coverage (the context the repo ruleset requires). "
        "Never recomputes: reads the row the emitters already wrote. "
        "Prints a JSON line {pr, posted, note}; exit 0 posted, 1 not posted."
    ),
)
def coverage_publish(
    pr_number: int = typer.Argument(..., help="GitHub PR number"),
    head: Optional[str] = typer.Option(
        None, "--head", help="Head sha to publish for; default: the PR's headRefOid."
    ),
    repo: Optional[str] = typer.Option(
        None, "--repo", help="Directory whose git remote names the repository."
    ),
) -> None:
    args = [str(pr_number)]
    if head:
        args += ["--head", head]
    if repo:
        args += ["--cwd", repo]
    _forward_to_binary("review-coverage", ["publish", *args])


@pr_app.command(
    "info",
    help=(
        "Read PR state, URL, head SHA, refs, and mergeability through one REST request. "
        "Prints JSON; exit 4 when the REST instrument cannot answer."
    ),
)
def info(
    pr_number: Optional[int] = typer.Argument(
        None, help="GitHub PR number; defaults to current branch"
    ),
    repo: Optional[str] = typer.Option(
        None, "--repo", help="GitHub owner/repo; defaults to origin"
    ),
) -> None:
    from fno.pr import _rest

    if pr_number is None:
        pr_number, reason = _rest.resolve_current_pr_number_rest(cwd=os.getcwd(), repo=repo)
        if pr_number is None:
            typer.echo(json.dumps({"pr": None, "error": reason}, separators=(",", ":")))
            raise typer.Exit(code=4)
    payload, reason = _rest.fetch_pr_info_rest(str(pr_number), cwd=os.getcwd(), repo=repo)
    if payload is None:
        typer.echo(json.dumps({"pr": pr_number, "error": reason}, separators=(",", ":")))
        raise typer.Exit(code=4)
    typer.echo(json.dumps(payload, separators=(",", ":")))


@pr_app.command(
    "list",
    help="List PR number, state, title, head ref, and URL through REST.",
)
def list_cmd(
    state: str = typer.Option("open", "--state", help="open, closed, or all"),
    repo: Optional[str] = typer.Option(
        None, "--repo", help="GitHub owner/repo; defaults to origin"
    ),
) -> None:
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno do pr list: the fno-agents binary was not found. It ships in "
            "the `pip install fno` wheel and with the plugin; reinstall fno or "
            "run `fno doctor update --rust`, or set FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)

    argv = [str(binary), "pr-heal", "list", "--state", state]
    if repo:
        argv += ["--repo", repo]
    result = subprocess.run(argv, check=False)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@pr_app.command(
    "graphql-exec",
    hidden=True,
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
    help="Run one gh GraphQL read under the reserve accounting.",
    epilog=(
        "Everything after -- is passed to gh verbatim as its argv, command "
        "words first: fno do pr graphql-exec --purpose discretionary -- api "
        "graphql -f query=... --jq ... . A flag-first argv is refused, "
        "because gh answers flags with no command word by printing its root "
        "command list."
    ),
)
def graphql_exec(
    ctx: typer.Context,
    purpose: GraphqlPurpose = typer.Option(..., "--purpose"),
) -> None:
    from fno.pr import _quota

    result = _quota.execute_graphql(purpose.value, list(ctx.args))
    if result.stdout:
        typer.echo(result.stdout, nl=False)
    if result.stderr:
        typer.echo(result.stderr, err=True)
    raise typer.Exit(code=result.returncode)


@pr_app.command(
    "logs",
    help=(
        "Why did CI fail: spool the failing job's log to .fno/last-ci.log and "
        "print its last 40 lines. Omit <pr> to read the current branch. "
        "--job picks among several failures, --lines resizes the tail, --full "
        "dumps the whole log. Exit 0 green (nothing fetched), 1 red, 2 pending, "
        "3 no checks, 4 fetch error, 127 gh-missing."
    ),
)
def logs(
    pr_number: Optional[int] = typer.Argument(
        None, help="GitHub PR number; omitted -> the PR for the current branch."
    ),
    job: Optional[str] = typer.Option(
        None, "--job", help="Failing check to tail (exact name, else substring)."
    ),
    lines: int = typer.Option(40, "--lines", help="Tail length."),
    full: bool = typer.Option(False, "--full", help="Print the whole log, not a tail."),
) -> None:
    # pr 0 asks the door to resolve the current branch's PR.
    _forward_to_binary(
        "authorized-merge",
        [json.dumps({"op": "status-logs", "cwd": os.getcwd(), "pr": pr_number or 0, "job": job, "lines": lines, "full": full})],
    )


def _forward_to_binary(verb: str, args: list[str]) -> None:
    """Forward to the bundled fno-agents binary, binary-direct. The shared
    body of the do-pr binary verbs (heal, push, rebase)."""
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            f"fno do pr {verb.replace('pr-', '')}: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    result = subprocess.run([str(binary), verb, *args], check=False)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@pr_app.command(
    "heal",
    hidden=True,
    help=(
        "Classify a red check by signature, apply the mechanical fix, push "
        "once. Dry run unless --apply. --playbook prints the signature table; "
        "--all reports every red open PR; --all --apply drives them (each "
        "from its own worktree, behind four refusals). Exit 0 nothing red, "
        "1 escalations remain, 2 a run is in flight, 3 wrong branch or dirty "
        "tree, 4 read error, 127 binary missing."
    ),
)
def heal(
    pr_number: Optional[int] = typer.Argument(None, help="GitHub PR number."),
    apply: bool = typer.Option(False, "--apply", help="Fix, commit, push once."),
    all_prs: bool = typer.Option(False, "--all", "-A", help="Report every red open PR."),
    playbook: bool = typer.Option(False, "--playbook", help="Print the signature table."),
) -> None:
    argv = []
    if pr_number is not None:
        argv.append(str(pr_number))
    if apply:
        argv.append("--apply")
    if all_prs:
        argv.append("--all")
    if playbook:
        argv.append("--playbook")
    _forward_to_binary("pr-heal", argv)


@pr_app.command(
    "base-check",
    help=(
        "Refuse a PR whose branch base is > 24h of main history behind "
        "origin/main (phantom-deletion guard). Exit 0 fresh|bypass|fail-open, "
        "3 stale (points at the git merge origin/main fix), 4 unrelated histories. Bypass "
        "with FNO_PR_BASE_OK=stale-acknowledged (emits gate_escape)."
    ),
)
def base_check(
    base: str = typer.Option(
        "origin/main", "--base", help="Base ref to compare the branch against"
    ),
) -> None:
    _forward_to_binary("pr-base-check", ["--base", base])


@pr_app.command(
    "base-lineage-check",
    help=(
        "Refuse a merge into a base branch that no longer leads to the default "
        "branch (<pr_number>). Fires when a MERGED PR already carried that base, "
        "or when the base is already an ancestor of the default branch. Exit 0 "
        "ok|bypassed, 3 stale (retarget the PR), 4 unknown (a probe failed). "
        "Bypass with FNO_PR_BASE_LINEAGE_OK=stale-acknowledged (emits gate_escape)."
    ),
)
def base_lineage_check(
    pr_number: int = typer.Argument(..., help="GitHub PR number"),
) -> None:
    _forward_to_binary("pr-base-lineage-check", [str(pr_number)])


@pr_app.command(
    "merge-result-check",
    hidden=True,
    help=(
        "Compile the merge result of <pr_number> against its base: git merge-tree plus the"
        " repo-wide ruff + mypy step on that tree. Exit 0 ok, 3 red (rebase, fix, push,"
        " retry), 4 unknown (a probe failed)."
    ),
)
def merge_result_check(pr_number: int = typer.Argument(..., help="GitHub PR number")) -> None:
    _forward_to_binary("pr-merge-result-check", [str(pr_number)])


@pr_app.command(
    "coverage-check",
    hidden=True,
    help=(
        "The merge guard's coverage predicate, for callers that cannot import "
        "it (the stdlib-only git-protection hook shells this verb on a bare "
        "gh pr merge). Exit 0 covered, 3 uncovered (the guard's refusal on "
        "stderr), 4 unanswered (a named instrument failure)."
    ),
)
def coverage_check(
    pr_number: int = typer.Argument(..., help="GitHub PR number"),
    recompute: bool = typer.Option(
        False,
        "--recompute",
        help="Recompute once when no row describes this head.",
    ),
) -> None:
    args = [str(pr_number)]
    if recompute:
        args.append("--recompute")
    _forward_to_binary("review-coverage", ["check", *args])


@pr_app.command(
    "coverage-waive",
    hidden=True,
    help=(
        "Record the attended operator waiver for ONE PR head (<pr_number> "
        "--reason). Operator provenance decides, never a GitHub login: an "
        "attended terminal records, a harness-identified session is refused. "
        "The waiver is pinned to the live 40-hex head, so a push voids it. "
        "Exit 0 recorded (stdout: the head-pinned receipt), 2 no reason, "
        "3 refused provenance, 4 unreadable slug/head, 1 failed write."
    ),
)
def coverage_waive(
    pr_number: int = typer.Argument(..., help="GitHub PR number"),
    reason: str = typer.Option(
        ...,
        "--reason",
        "-R",
        help="Why coverage is waived for this exact head (required, recorded).",
    ),
) -> None:
    _forward_to_binary(
        "review-coverage", ["waive", str(pr_number), "--reason", reason]
    )


def _parse_flags(flags_json: Optional[str]) -> Optional[List[str]]:
    """The --flags-json payload as a list, or None when absent/unreadable."""
    if flags_json is None:
        return None
    try:
        parsed = json.loads(flags_json)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, list) and all(isinstance(flag, str) for flag in parsed) else None
@pr_app.command(
    "review-hold",
    hidden=True,
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
    help=(
        "Register, clear, or read the hold that says a review of this PR's head "
        "is RUNNING. Merge readiness only knows what verdicts EXIST for a head, "
        "so a review still writing its fixes is invisible to it. Actions: "
        "check <pr> (exit 0 clear, 3 held, 4 unanswered), acquire, release, "
        "metadata, round. Answered by the fno-agents claim verb's review-hold "
        "sub-token; the flags pass through unchanged."
    ),
)
def review_hold(ctx: typer.Context) -> None:
    _forward_to_binary("claim", ["review-hold", *ctx.args])

@pr_app.command("hold-check", hidden=True)
def hold_check(
    pr_number: int = typer.Argument(..., help="GitHub PR number"),
    repo: Optional[str] = typer.Option(None, "--repo", help="Repository working directory."),
) -> None:
    """Refuse a PR whose bound plan ancestry carries an active or unreadable hold."""
    args = [str(pr_number)]
    if repo:
        args += ["--repo", repo]
    _forward_to_binary("authorized-merge", ["hold-check", *args])


@pr_app.command(
    "evidence-check",
    help=(
        "Require a newest exact-HEAD full/passed verification receipt across "
        "project, global, and known delivery-root event journals. "
        "--allow-rebase-equivalent also accepts a full/passed receipt for an "
        "earlier commit whose verbatim patch ids match HEAD (a rebase with no "
        "code change; never a rescue of a failed/pending receipt for HEAD "
        "itself). Answered by fno-agents verify-evidence check."
    ),
)
def evidence_check(
    allow_rebase_equivalent: bool = typer.Option(
        False,
        "--allow-rebase-equivalent",
        help="Accept a receipt for an earlier commit whose patches match HEAD.",
    ),
) -> None:
    args = []
    if allow_rebase_equivalent:
        args.append("--allow-rebase-equivalent")
    _forward_to_binary("verify-evidence", args)

@pr_app.command("evidence-required", hidden=True)
def evidence_required(
    base: str = typer.Option("origin/main", "--base"),
) -> None:
    """Expose the one local-verification policy to shell-based ship paths."""
    _forward_to_binary("verify-evidence", ["required", "--base", base])

@pr_app.command("next-receipt-generation", hidden=True)
def next_receipt_generation(
    candidate_sha: str = typer.Option(..., "--candidate-sha"),
) -> None:
    """Derive the next receipt generation from every discovered journal."""
    _forward_to_binary("verify-evidence", ["next-generation", "--candidate-sha", candidate_sha])

@pr_app.command("global-receipt-events-path", hidden=True)
def global_receipt_events_path() -> None:
    """Print the durable cross-checkout verification receipt journal."""
    _forward_to_binary("verify-evidence", ["journal-path"])


@pr_app.command(
    "sync-canonical",
    help=(
        "Post-merge canonical-checkout sync. Runs "
        "config.post_merge.sync_command in the CANONICAL checkout after a PR "
        "merges (opt-in; unset command = no-op). Exactly-once per merge SHA via "
        "a marker + single-flight lock; fail-open. Exit 0 no-op/skipped/synced, "
        "non-zero on a failed sync_command or a canonical checkout dirty on "
        "paths this merge touches (marker withheld -> retries)."
    ),
)
def sync_canonical(
    pr_number: int = typer.Option(..., "--pr-number", help="GitHub PR number of the merged PR"),
) -> None:
    from fno.rust_binary import VerbUnavailable, verb_call

    try:
        answer = verb_call(
            "sync-canonical",
            {"action": "sync", "cwd": os.getcwd(), "pr": pr_number},
            timeout=900.0,
        )
    except VerbUnavailable as exc:
        # The marker stays withheld, so the next reconcile retries.
        typer.echo(
            f"post-merge sync: native verb unavailable ({exc}); skipping", err=True
        )
        raise typer.Exit(code=1)
    for line in answer.get("stdout") or []:
        typer.echo(line)
    for line in answer.get("stderr") or []:
        typer.echo(line, err=True)
    raise typer.Exit(code=int(answer.get("exit", 1)))


@pr_app.command(
    "publish-review",
    hidden=True,
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
    help=(
        "Post a review verdict to GitHub as config.review.bot_identity "
        "(--pr-number N; --verdict defaults to the newest head-pinned "
        "attestation for HEAD; --dry-run/-N makes no POST). One bot-review: "
        "receipt line; exit 0 posted, 1 skipped|refused|failed, 2 no "
        "attestation to default from. The identity-independent review-round "
        "leg also runs on this call "
        "(crates/fno-agents/src/publish_review.rs::publish_round) but this "
        "door prints only the bot-review receipt."
    ),
)
def publish_review(ctx: typer.Context) -> None:
    _forward_to_binary("publish-review", list(ctx.args))


@pr_app.command(
    "rebase",
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
    help=(
        "Rebase the current branch via the conflict-delegation protocol "
        "(--base=<branch>, --continue). Two-phase: exit 42 means the caller "
        "must invoke the conflict-resolver agent, then call back --continue."
    ),
)
def rebase(ctx: typer.Context) -> None:
    _forward_to_binary("pr-rebase", list(ctx.args))


@pr_app.command(
    "push",
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
    help=(
        "The one guarded push: fetch, rebase onto origin/main, optionally preflight, "
        "push exactly once, print one receipt. Refuses while a CI run on the remote "
        "head is still in flight, unless the head's checks already carry a failure: "
        "then it cancels that stale run, records the bypass row, and pushes with no "
        "flag. A head with no red row is never cancelled. --no-preflight is an "
        "accepted no-op; --force-ci-cancel still overrides. Exit 0 pushed "
        "(ci=settled|bypassed|red-cancel), 1 preflight red, 2 a live run in flight, "
        "3 a refusal the caller must fix (protected branch, dirty tree, conflict), 4 a read error, 127 binary missing."
    ),
)
def push(ctx: typer.Context) -> None:
    _forward_to_binary("pr-push", list(ctx.args))


@pr_app.command(
    "create", hidden=True,
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
    help="Duplicate-guarded create; exit 3 refuses a twin (--not-duplicate <pr>).",
)
def create(ctx: typer.Context) -> None:
    _forward_to_binary("pr-create", list(ctx.args))


@pr_app.command(
    "ritual",
    hidden=True,
    help=(
        "Mechanical core of the post-merge ritual. Runs the CLI-only "
        "steps as one idempotent sequence, printing a per-leg receipt line "
        "(step=<name> status=<ok|skipped|failed> detail=...). Non-zero if any "
        "leg failed; no leg is swallowed. The judgment residue (deferral triage "
        "+ parking-lot prose) is done inline by an attended caller, or spawned "
        "as one headless one-shot under --autonomous (never bg). Hidden: the "
        "`fno do pr merged` skill is the attended front door."
    ),
)
def ritual(
    pr_number: Optional[int] = typer.Argument(
        None, help="Merged PR number; omitted -> most recently merged PR for this repo."
    ),
    autonomous: bool = typer.Option(
        False,
        "--autonomous",
        help="No operator present: spawn the judgment leg as one headless one-shot "
        "when its inputs are non-empty. Mirrors the POST_MERGE_NONINTERACTIVE=1 env.",
    ),
) -> None:
    args: list[str] = []
    if pr_number is not None:
        args.append(str(pr_number))
    if autonomous or os.environ.get("POST_MERGE_NONINTERACTIVE", "") == "1":
        args.append("--autonomous")
    _forward_to_binary("pr-watch", ["ritual", *args])


@pr_app.command(
    "closure-trailer",
    help=(
        "Print the exact `Fixes` closure line for NODE plus its "
        "contained_in descendants. Compose it into a PR body before "
        "`gh pr create` so every node the PR ships gets bound at merge, not "
        "just the one stamped by --pr-number. Prints nothing (exit 0) when "
        "NODE is unresolvable or nothing well-formed remains, so a caller "
        "can append the output to a body unconditionally. Exit 4 means the "
        "graph read itself failed - a dead reader is not an unresolvable "
        "node, and a caller must not append empty output on it. Omit NODE "
        "to resolve it from the current branch instead: exactly one real "
        "graph node must be named, and any failure exits NONZERO with the "
        "reason - a producer that cannot verify must not read as empty."
    ),
)
def closure_trailer(
    node: Optional[str] = typer.Argument(None, help="Node id to render the trailer for; omit to resolve from the current branch."),
    extra: List[str] = typer.Option(
        [],
        "--extra",
        help="Additional genuinely-shipped node ids beyond NODE and its "
        "contained_in descendants (repeatable).",
    ),
) -> None:
    argv = [node] if node is not None else []
    for item in extra:
        argv.extend(["--extra", item])
    _forward_to_binary("pr-closure-trailer", argv)


@pr_app.command("bind-created", hidden=True)
def bind_created(
    url: str = typer.Option(..., "--url", help="Created PR URL."),
    repo: Optional[str] = typer.Option(None, "--repo", help="Repository worktree."),
    node: Optional[str] = typer.Option(
        None,
        "--node",
        help="Authoritative node id (the manifest's). Leads; branch is the fallback.",
    ),
) -> None:
    """Bind a raw ``gh pr create`` result to its one real node."""
    # One JSON argv element (the logs-verb door): the leg reads its payload
    # from argv, never stdin - the passthrough below inherits the caller's
    # stdin, and a TTY there would hang a stdin read.
    payload = json.dumps({"url": url, "repo": repo, "node": node})
    _forward_to_binary("pr-closure-bind-created", [payload])
