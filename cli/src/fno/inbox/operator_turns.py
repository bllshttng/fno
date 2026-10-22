"""``fno inbox user`` (old spelling ``fno inbox operator`` still works) - the user conversation queue.

A lead records from the direction it is pushed: worker mail arrives as a
discrete event with an id and a queue, and it gets recorded, while operator
conversation is a stream with no event boundary and no receipt, so it does
not. This sub-app gives the operator turn the shape mail already has: an id,
a queue, and an ack, with no capture-time write path and no hook.

The queue is derived from the whole transcript by the Rust reader
(``fno-agents compaction operator-turns``): undispositioned turns are the
prose user turns minus the ids already acked. A per-session scan cursor
(``<session>.scan.json``, a deletable cache) is that reader's cache, so
later reads parse only newly appended bytes and no turn is lost to
distance from EOF. Recording still runs through the existing capture commands
first; ``ack`` names what the turn produced - one ack command where a
``--from-turn`` flag would have been threaded through three surfaces.

Session/transcript/ledger resolution (in order: explicit env pins, then
the ambient identity): ``FNO_OPERATOR_SESSION_ID``,
``FNO_OPERATOR_HARNESS``, ``FNO_OPERATOR_TRANSCRIPT``,
``FNO_OPERATOR_CAPTURE_DIR``. The pins are the tools/tests/hook seam - a
Stop hook runs outside the harness process, so it pins nothing and lets
the ambient identity resolve.

Machine envelopes and markers (task-notification, teammate delivery,
interrupt markers, bash echo, compaction preamble) are refused and counted
by name inside the Rust reader; every read names what it skipped, because
a filter that drops a real operator turn silently is worse than the noise
it removes.

Known hole, named on purpose: ``fno agents mail send --raw`` strips the
envelope, so raw mail still reads as operator here. For that residual,
over-counting is the safe direction: a false queue entry costs one ack, a
missed operator turn costs the failure this queue exists to close.
"""

from __future__ import annotations

import typer

operator_app = typer.Typer(
    name="user",
    help="Queue of this session's undispositioned user turns, derived "
    "from the transcript by the Rust reader and acked to a per-session "
    "ledger under ~/.fno/operator-capture/ (the <session>.scan.json scan "
    "cursor is a cache; deleting it only forces a full rescan). Machine "
    "envelopes are refused and counted by name in that reader. Residual "
    "hole: raw mail (send --raw) reads as a user turn; over-counting is "
    "the safe direction there.",
    no_args_is_help=True,
)


@operator_app.command("list")
def list_forward(
    limit: int = typer.Option(None, "--limit", "-L", min=1, help="Max turns to show."),
    json_output: bool = typer.Option(False, "--json", "-J", help="Emit a JSON array."),
) -> None:
    """Undispositioned user turns, oldest first."""
    extra = (["--limit", str(limit)] if limit is not None else []) + (["--json"] if json_output else [])
    _forward(["operator-turns", "--render", "list", *extra])


@operator_app.command("ack")
def ack_forward(
    turn_id: str = typer.Argument(..., help="The user turn id to dispose."),
    outcome: str = typer.Option(
        ...,
        "--outcome",
        help="nothing | law:<decision-id> | capture:<fu-id> | node:<node-id> | answer:<the answer>",
    ),
    why: str = typer.Option(None, "--why", help="One-line reason, kept in the ledger."),
) -> None:
    """Dispose one user turn, naming what it produced."""
    _forward(["ack", turn_id, "--outcome", outcome, *(["--why", why] if why else [])])


@operator_app.command("status")
def status_forward(
    json_output: bool = typer.Option(False, "--json", "-J", help="Emit the depth payload."),
) -> None:
    """Queue depth for this session - the number the capture hook reads."""
    _forward(["operator-turns", "--render", "status", *(["--json"] if json_output else [])])


def _forward(tail: list[str]) -> None:
    """Exec the native action; the binary owns the output and the exit code."""
    import subprocess

    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo("error: the fno-agents binary was not found", err=True)
        raise typer.Exit(code=127)
    argv = [str(binary), "compaction", *tail]
    raise typer.Exit(code=subprocess.run(argv).returncode)
