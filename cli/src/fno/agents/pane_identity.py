"""`fno agents pane-identity`: cross-check mux panes against registry rows.

A hidden command: operator-facing reads go through `fno agents list`. The
cross-check is native (crates/fno-agents/src/pane_identity.rs); with an
installed binary the runtime router execs it before this function runs, so
this front keeps only its signature for --help and refuses without one.
"""
from __future__ import annotations

from typing import Optional

import typer


def cmd_pane_identity(
    server: Optional[str] = typer.Option(
        None,
        "--server",
        help="Mux server to check. Default: the resolved server.",
    ),
    session_id: Optional[str] = typer.Option(
        None,
        "--session-id",
        hidden=True,
        help="Deprecated alias for --server.",
    ),
    session_legacy: Optional[str] = typer.Option(
        None,
        "--session",
        hidden=True,
        help="Deprecated alias for --server.",
    ),
    as_json: bool = typer.Option(
        False, "--json", "-J", help="Emit the same content as JSON."
    ),
) -> None:
    """Cross-check mux panes against registry rows, in both directions.

    Every registry row with a mux ref must resolve to a pane whose fno_id
    matches the row (a stale ref means the pane was re-homed, e.g. by a
    resume); every pane whose argv carries fno's spawn signature must be
    referenced by a row (a miss is an fno worker no fno surface can address).
    The counts compared print on every run, so a zero-mismatch result is a
    reading and not a silence. A mismatch is a READING, never a repair: this
    command mutates nothing, and it never mints an identity from argv.

    Exit codes: 0 clean, 1 mismatch found, 2 an instrument (mux listing or
    registry) could not be read.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = (server, session_id, session_legacy, as_json)
    refuse_without_binary("pane-identity")
