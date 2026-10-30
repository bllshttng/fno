"""``fno agents peek``: read-only peer observation; registered here so the file-budget gate keeps ``agents/cli.py`` shrinking."""
from __future__ import annotations

from typing import Optional

import typer

from fno.agents.cli import agents_app


@agents_app.command("peek", hidden=True)
def cmd_peek(
    handle: Optional[str] = typer.Argument(
        None,
        help="Peer handle (alias or bare hex short-id, as in `mail send`). Omit with --all.",
    ),
    lines: int = typer.Option(
        15, "--lines", "-n", help="Show the last N transcript records (default 15; 0 for none)."
    ),
    follow: bool = typer.Option(
        False, "--follow", "-f", help="Stream new records as the peer emits them (read-only)."
    ),
    grep: Optional[str] = typer.Option(
        None,
        "--grep",
        help=(
            "Only records carrying this token, within the --lines window "
            "(raise -n to search deeper); the stderr count line reports a "
            "zero, never bare."
        ),
    ),
    all_sessions: bool = typer.Option(
        False,
        "--all",
        "-A",
        help="Search every registry session with a transcript instead of one handle.",
    ),
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit JSON-Lines rows instead of human lines."
    ),
) -> None:
    """Observe a peer read-only.

    Tails the transcript (claude/codex), preferring normalized status
    events; a pane worker resolves through the registry's mux ref. Never
    writes anything the peer reads. Exit 13 unknown peer, 1 no reader,
    0 observed (or "no activity yet"). The read is native
    (crates/fno-agents/src/peek.rs); with an installed binary the runtime
    router execs it before this function runs, so this front keeps only its
    signature for --help and refuses without one.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = (handle, lines, follow, grep, all_sessions, json_out)
    refuse_without_binary("peek")
