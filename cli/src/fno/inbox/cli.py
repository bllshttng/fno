"""``fno inbox`` - what is waiting on a human.

Mints the inbox root (unit 2 of the command reorg). approvals
and notify fold in whole - each app is registered here exactly as it was
registered at the top level, so `fno inbox approvals ls` etc. reach the same
commands `fno approvals ls` did. The lead board, `--last-run` included,
answers at the Rust front door. outstanding and its day relay are the Rust
front's nested commands: no Python mount.

 (d-add90c60): the human-authority commands join the human-facing group.
`decisions` and the whole `law` app are the Rust front's nested commands (no
Python mount), and `fno inbox decide` answers natively through the decide
door; the root `fno decide` / `fno law` spellings are COMMAND_MOVES shims.
"""

from __future__ import annotations

import typer

from fno.approvals_cli import approvals_app
from fno.inbox.operator_turns import operator_app
from fno.notify.cli import notify_app

inbox_app = typer.Typer(
    name="inbox",
    help="What is waiting on a human: approvals, notifications, outstanding "
    "carve-outs and questions, the lead board, decisions, law, and the "
    "user conversation queue.",
    no_args_is_help=True,
)

inbox_app.add_typer(approvals_app, name="approvals")
inbox_app.add_typer(notify_app, name="notify")
# `fno inbox law` is the Rust front's nested group; no Python mount.
# The queue command is `user`; the pre-rename spelling stays as a hidden
# alias so existing scripts keep working.
inbox_app.add_typer(operator_app, name="user")
inbox_app.add_typer(operator_app, name="operator", hidden=True)
