"""fno do resume receipt - forwarding leaf over the native resume-receipt door.

The verbs are hidden infrastructure: handoff writes at a phase boundary, a
successor session validates before any write, and `show` prints parse-checked
evidence. The port moved the leaf's logic (the receipt model, the revalidate
gate, the byte shapes, output and exit codes) into
`crates/fno-agents/src/resume_receipt/`; Python owns transport only, per the
dual-implementation protocol. The goldens under
`crates/fno-agents/tests/golden/resume_receipt/` pin the bytes; the
characterization test in the same crate proves the door still answers them.
"""

from __future__ import annotations

from typing import Optional

import typer

from fno.tombstones import tombstone_group_cls

cli = typer.Typer(
    name="resume",
    help=(
        "Durable typed resume receipts (evidence, never write authority). "
        "To revive an agent SESSION by id, use `fno agents resume <session-id>` "
        "(or `fno agents adopt <session-id>` to register it without resuming)."
    ),
    no_args_is_help=True,
    cls=tombstone_group_cls("resume receipt"),
)

receipt_app = typer.Typer(
    name="receipt",
    help="Write / validate / show a durable resume receipt",
    no_args_is_help=True,
    cls=tombstone_group_cls("resume receipt"),
)
cli.add_typer(receipt_app, name="receipt")


def _forward(verb: str, argv: list[str]) -> None:
    """Forward to the bundled fno-agents binary, binary-direct.

    Captured and re-emitted through typer: the leaf's output IS the operator
    surface, and a CliRunner-hosted caller (the tests) must see it. Exit
    codes, stdout bytes, and stderr lines are the door's own.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno do resume receipt: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    # Captured and re-emitted through typer (a CliRunner caller must see it).
    result = subprocess.run(
        [str(binary), "resume-receipt", verb, *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


def _session_argv(session_id: Optional[str], session_legacy: Optional[str]) -> list[str]:
    """Forward whichever session spelling the caller used, verbatim.

    The deprecation warning and the both-passed refusal are the door's
    answers; the leaf transports the flags untouched so neither is lost.
    """
    argv: list[str] = []
    if session_id is not None:
        argv.extend(("--session-id", session_id))
    if session_legacy is not None:
        argv.extend(("--session", session_legacy))
    return argv


def _list_argv(name: str, raw: Optional[str]) -> list[str]:
    argv: list[str] = []
    if raw is not None:
        argv.extend((name, raw))
    return argv


@receipt_app.command("write")
def write_cmd(
    node: str = typer.Option(..., "--node", help="Backlog node id"),
    session_id: Optional[str] = typer.Option(None, "--session-id", help="Writing session id"),
    session_legacy: Optional[str] = typer.Option(
        None, "--session", hidden=True, help="[DEPRECATED] alias for --session-id."
    ),
    phase: str = typer.Option(..., "--phase", help="Phase/boundary (do, review, ship, wave, ...)"),
    generation: int = typer.Option(..., "--generation", help="Handoff generation (>=1)"),
    repo: str = typer.Option(..., "--repo", help="Repository name"),
    worktree: str = typer.Option(..., "--worktree", help="Absolute worktree path"),
    branch: str = typer.Option(..., "--branch", help="git branch"),
    head: str = typer.Option(..., "--head", help="git HEAD sha (the candidate)"),
    next_command: str = typer.Option(..., "--next-command", "--next-verb", help="Exactly one next action command"),  # verb-wording: allow (old flag, one release)
    next_target: Optional[str] = typer.Option(None, "--next-target", help="Next action target (node/task)"),
    completed_tasks: Optional[str] = typer.Option(None, "--completed-tasks", help="Newline-separated completed task ids"),
    remaining_tasks: Optional[str] = typer.Option(None, "--remaining-tasks", help="Newline-separated remaining task ids"),
    open_findings: Optional[str] = typer.Option(None, "--open-findings", help="Newline-separated open finding ids"),
    known_reds: Optional[str] = typer.Option(None, "--known-reds", help="Newline-separated known-red signals"),
    watchers: Optional[str] = typer.Option(None, "--watchers", help="Newline-separated watcher ids"),
    idempotency_keys: Optional[str] = typer.Option(None, "--idempotency-keys", help="Newline-separated external-effect keys"),
    written_at: Optional[str] = typer.Option(None, "--written-at", help="Override UTC timestamp (default: now)"),
) -> None:
    """Write an immutable versioned resume receipt (producer).

    Snapshots the supplied state into a receipt file named by identity. Refuses
    to overwrite a completed receipt for the same identity.
    """
    argv = [
        "--node", node,
        "--phase", phase,
        "--generation", str(generation),
        "--repo", repo,
        "--worktree", worktree,
        "--branch", branch,
        "--head", head,
        "--next-command", next_command,
    ]
    argv.extend(_session_argv(session_id, session_legacy))
    if next_target is not None:
        argv.extend(("--next-target", next_target))
    argv.extend(_list_argv("--completed-tasks", completed_tasks))
    argv.extend(_list_argv("--remaining-tasks", remaining_tasks))
    argv.extend(_list_argv("--open-findings", open_findings))
    argv.extend(_list_argv("--known-reds", known_reds))
    argv.extend(_list_argv("--watchers", watchers))
    argv.extend(_list_argv("--idempotency-keys", idempotency_keys))
    if written_at is not None:
        argv.extend(("--written-at", written_at))
    _forward("write", argv)


@receipt_app.command("validate")
def validate_cmd(
    node: str = typer.Option(..., "--node", help="Backlog node id to revalidate"),
    session_id: Optional[str] = typer.Option(None, "--session-id", help="Successor (own) session id; default: receipt session"),
    session_legacy: Optional[str] = typer.Option(
        None, "--session", hidden=True, help="[DEPRECATED] alias for --session-id."
    ),
    worktree: Optional[str] = typer.Option(None, "--worktree", help="Override worktree to probe (default: receipt worktree)"),
    events_file: Optional[str] = typer.Option(None, "--events", help="Override events.jsonl path (default: <worktree>/.fno/events.jsonl)"),
    harness: Optional[str] = typer.Option(None, "--harness", help="Owning harness for generation scoping"),
    claims_root: Optional[str] = typer.Option(None, "--claims-root", help="Override claims root (default: ~/.fno)"),
) -> None:
    """Revalidate the latest receipt for a node against live state (consumer).

    Gathers live HEAD/branch, worktree existence, node-claim holder, and the
    node's journal events; runs the read-only revalidate gate; prints a JSON
    verdict. Never acquires or releases claims - on failure the caller parks
    and the predecessor's state is preserved.
    """
    argv = ["--node", node]
    argv.extend(_session_argv(session_id, session_legacy))
    if worktree is not None:
        argv.extend(("--worktree", worktree))
    if events_file is not None:
        argv.extend(("--events", events_file))
    if harness is not None:
        argv.extend(("--harness", harness))
    if claims_root is not None:
        argv.extend(("--claims-root", claims_root))
    _forward("validate", argv)


@receipt_app.command("show")
def show_cmd(
    node: str = typer.Option(..., "--node", help="Backlog node id"),
    receipt_file: Optional[str] = typer.Option(None, "--path", help="Explicit receipt file (default: latest for node)"),
) -> None:
    """Load + print a receipt (parse/integrity checked). Fails on malformed."""
    argv = ["--node", node]
    if receipt_file is not None:
        argv.extend(("--path", receipt_file))
    _forward("show", argv)
