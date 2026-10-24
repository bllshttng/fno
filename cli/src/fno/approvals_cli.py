"""CLI surface for approvals: fno inbox approvals ls | show | decide (forwarder leaves).

All three commands answer natively (crates/fno-agents/src/approvals.rs); this
front keeps the group so `--help` lists the verbs and `fno approvals` still
reaches the same native lane through the one Rust classify.
"""
from __future__ import annotations

import typer

approvals_app = typer.Typer(
    name="approvals",
    help="Inspect and decide pending approvals for consequential effects.",
    no_args_is_help=True,
)

_FORWARD = {"allow_extra_args": True, "ignore_unknown_options": True}


def _forward_native(verb: str, ctx: typer.Context) -> None:
    """Exec the Rust front with the same argv, propagating its exit code."""
    from fno.cli import _run_rust_front

    _run_rust_front(["inbox", "approvals", verb, *ctx.args])


@approvals_app.command("ls", context_settings=_FORWARD)
def list_requests(ctx: typer.Context) -> None:
    """List approval requests (--pending, --json). Reads only; never executes an effect."""
    _forward_native("ls", ctx)


@approvals_app.command("show", context_settings=_FORWARD)
def show_request(ctx: typer.Context) -> None:
    """Show every bound field of one request: show DIGEST [--json]. Reads only."""
    _forward_native("show", ctx)


@approvals_app.command("decide", context_settings=_FORWARD)
def decide_request(ctx: typer.Context) -> None:
    """Decide one exact request: decide DIGEST --as PRINCIPAL (--approve | --decline).

    The principal is authorized against config.approvals.authorized_principals.
    Approving records a decision and executes nothing.
    """
    _forward_native("decide", ctx)
