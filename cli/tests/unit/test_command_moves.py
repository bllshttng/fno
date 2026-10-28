"""Tests for the COMMAND_MOVES forwarding shim and the four deletes beside it.

A move is not a break: the old spelling stays registered for one release,
forwards in-process once the destination root exists, and prints one stderr
line naming the new spelling. Until the destination is minted the OLD
registration serves the call, announced rather than silent - except a hot
leaf in ``permanent_leaves``, which stays quiet forever. The four deleted commands
(executor, posture, tokens, upgrade) are tombstones, not moves: the
capability is gone, so the refusal names where it went instead of forwarding.

The synthetic-app tests pin the forwarding mechanism end to end through the
real ``LazyTypeGroup.resolve_command``; the real-app tests pin the wiring on
the actual registry.
"""
from __future__ import annotations

import click
import typer
from typer.testing import CliRunner

from fno._lazy_group import make_lazy_group_cls
from fno.command_moves import Move, COMMAND_MOVES

runner = CliRunner()


# ---------------------------------------------------------------------------
# The forwarding mechanism, through the real LazyTypeGroup dispatch
# ---------------------------------------------------------------------------


def _app_with_old_registration(with_dest: bool) -> typer.Typer:
    """A scratch root app registering the moved spellings and (optionally)
    their destinations, on the same lazy-group class the real CLI uses. The
    specimen is ``claim``, a deprecated move that survives the table; the
    outstanding family's move died with its native port."""
    app = typer.Typer(cls=make_lazy_group_cls({}))

    @app.callback()
    def _cb() -> None: ...

    old = typer.Typer(help="old claim")

    @old.command("list")
    def _old_list() -> None:
        typer.echo("OLD-PATH")

    app.add_typer(old, name="claim", hidden=True)

    if with_dest:
        agents = typer.Typer(help="agents")
        claim = typer.Typer(help="claim")

        @claim.command("list")
        def _agents_claim_list() -> None:
            typer.echo("NEW-PATH")

        agents.add_typer(claim, name="claim")
        app.add_typer(agents, name="agents", hidden=True)
    return app


def test_registered_destination_forwards_byte_identical():
    app = _app_with_old_registration(with_dest=True)
    direct = runner.invoke(app, ["agents", "claim", "list"])
    forwarded = runner.invoke(app, ["claim", "list"])
    assert direct.exit_code == 0 and forwarded.exit_code == 0
    # stdout of the old spelling is byte-identical to the new path: callers
    # parse stdout, so the shim must not edit it.
    assert forwarded.stdout == direct.stdout == "NEW-PATH\n"
    err = forwarded.stderr or ""
    assert err.count("is now") == 1
    assert "fno agents claim" in err
    assert "is now" not in (direct.stderr or "")


def test_hot_leaf_forwards_silently_through_the_destination():
    app = typer.Typer(cls=make_lazy_group_cls({}))

    @app.callback()
    def _cb() -> None: ...

    old_pr = typer.Typer(help="old pr")

    @old_pr.command("merge")
    def _old_merge(number: str) -> None:
        typer.echo(f"OLD-MERGE {number}")

    @old_pr.command("create")
    def _old_create() -> None:
        typer.echo("OLD-CREATE")

    app.add_typer(old_pr, name="pr", hidden=True)

    do = typer.Typer(help="do")
    pr = typer.Typer(help="pr")

    @pr.command("merge")
    def _new_merge(number: str) -> None:
        typer.echo(f"NEW-MERGE {number}")

    @pr.command("create")
    def _new_create() -> None:
        typer.echo("NEW-CREATE")

    do.add_typer(pr, name="pr")
    app.add_typer(do, name="do", hidden=True)

    merged = runner.invoke(app, ["pr", "merge", "993"])
    assert merged.exit_code == 0, merged.output
    assert merged.stdout == "NEW-MERGE 993\n"
    assert "is now" not in (merged.stderr or "")

    created = runner.invoke(app, ["pr", "create"])
    assert created.exit_code == 0, created.output
    assert created.stdout == "NEW-CREATE\n"
    err = created.stderr or ""
    assert "fno pr create is now fno do pr create" in err


def test_post_expiry_rejects_cold_leaf_when_destination_is_missing(monkeypatch):
    import fno.command_moves as moves

    app = typer.Typer(cls=make_lazy_group_cls({}))

    @app.callback()
    def _cb() -> None: ...

    old_pr = typer.Typer()

    @old_pr.command("create")
    def _old_create() -> None:
        typer.echo("OLD-CREATE-RAN")

    app.add_typer(old_pr, name="pr", hidden=True)
    monkeypatch.setitem(
        moves.COMMAND_MOVES,
        "pr",
        Move(kind="leaf-alias", to="do pr", permanent_leaves=frozenset({"status"})),
    )

    result = runner.invoke(app, ["pr", "create"])
    assert result.exit_code != 0
    assert "OLD-CREATE-RAN" not in result.output
    assert "was removed; use fno do pr create" in result.output


def test_every_moved_spelling_is_hidden():
    import typer.main

    from fno.cli import app

    assert COMMAND_MOVES, "the table must be populated for this test to mean anything"
    root = typer.main.get_command(app)
    ctx = click.Context(root)
    for name in COMMAND_MOVES:
        cmd = root.get_command(ctx, name)
        # `yard` was an eager @app.command, so its release form is a
        # removed registration rather than a hidden lazy row - forwarding is
        # owned by COMMAND_MOVES, not by the registry. `decide` joined it when
        # the family ported native: every destination the move names is a
        # native command now, so a python registration would forward into
        # nothing. `review` joined the same way when its ROOT claim moved to
        # the rust front door, which prints the move notice and execs
        # `do review` itself; a python registration would sit behind an
        # intercept that never reaches it. `pr` joined for the same reason
        # when its root claim moved native (the front door execs the
        # dispatcher with the argv intact). `retro` joined when its family
        # ported native: the move is a leaf-alias that refuses with the new
        # spelling. `restart` joined when the front door claimed both
        # spellings and execs `fno-agents restart` itself. `project` joined
        # the same way: the front door claims both spellings, prints the
        # rename notice, and execs `spawn-overlay project` itself. `skill-diff`
        # joined when its family ported: the front door claims both
        # spellings and execs the native ops, so a python registration
        # would forward into nothing. `mcp` joined the same way: the send
        # family ported native and the front door claims both spellings.
        # `setup` joined when every setup command ported native: the front
        # door claims the root spelling and answers it. `route` joined when
        # the route family ported native: the front classifier claims the
        # root spelling (route, config route, config routing, doctor route)
        # and execs the slot resolver itself, so a python registration would
        # sit behind an intercept that never reaches it. `research` and `think`
        # joined when the research family ported native. `target`, `state`,
        # and `phase` joined when the target family ported native: the front
        # door claims the bare spelling, prints the move notice, and execs
        # the native doors itself. `workspace` and `worktree` joined when
        # the worktree family did. `loops` and `autonomy` joined when the
        # fleet family ported: the front's fleet route claims both spellings
        # of each (root and group form), prints the move notice, and execs
        # the fno-agents doors (`loops`, `census yard`, `spawn-overlay
        # autonomy`), so a python registration would sit behind an intercept
        # that never reaches it. `observer` joined the same way: the front
        # door claims both spellings and execs the native ops. `lead` joined
        # when the lead family ported: the front door claims `lead <command>`
        # and prints the move notice itself. Every OTHER moved
        # spelling keeps a one-release registration, and any registration
        # that exists must be hidden: an advertised moved name teaches a
        # doomed spelling.
        if name in (
            "decide", "yard", "review", "pr", "retro", "restart", "project", "skill-diff",
            "mcp", "setup", "route", "research", "think", "target", "state",
            "phase", "workspace", "worktree", "autonomy", "loops", "observer",
            "lead",
        ):
            assert cmd is None, f"{name}'s registration was removed with the move"
            continue
        assert cmd is not None, f"moved spelling {name!r} must stay registered"
        assert getattr(cmd, "hidden", False), (
            f"moved spelling {name!r} must be hidden: an advertised moved name teaches a doomed spelling"
        )


# ---------------------------------------------------------------------------
# The four deletes: tombstones that teach, not "No such command"
# ---------------------------------------------------------------------------


def test_deleted_command_refuses_naming_its_replacement():
    from fno.cli import app

    result = runner.invoke(app, ["upgrade"])
    assert result.exit_code != 0
    combined = (result.output or "") + (result.stderr or "")
    assert "`fno upgrade` was removed" in combined
    assert "`fno doctor update`" in combined, combined
