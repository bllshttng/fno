"""Tests for the VERB_MOVES forwarding shim and the four deletes beside it.

A move is not a break: the old spelling stays registered for one release,
forwards in-process once the destination root exists, and prints one stderr
line naming the new spelling. Until the destination is minted the OLD
registration serves the call, announced rather than silent - except a hot
leaf in ``permanent_leaves``, which stays quiet forever. The four deleted verbs
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
from fno.verb_moves import Move, VERB_MOVES

runner = CliRunner()


# ---------------------------------------------------------------------------
# The forwarding mechanism, through the real LazyTypeGroup dispatch
# ---------------------------------------------------------------------------


def _app_with_old_registration(with_dest: bool) -> typer.Typer:
    """A scratch root app registering the moved spellings and (optionally)
    their destinations, on the same lazy-group class the real CLI uses."""
    app = typer.Typer(cls=make_lazy_group_cls({}))

    @app.callback()
    def _cb() -> None: ...

    old = typer.Typer(help="old outstanding")

    @old.command("list")
    def _old_list() -> None:
        typer.echo("OLD-PATH")

    app.add_typer(old, name="outstanding", hidden=True)

    if with_dest:
        inbox = typer.Typer(help="inbox")
        outstanding = typer.Typer(help="outstanding")

        @outstanding.command("list")
        def _inbox_outstanding_list() -> None:
            typer.echo("NEW-PATH")

        inbox.add_typer(outstanding, name="outstanding")
        app.add_typer(inbox, name="inbox", hidden=True)
    return app


def test_registered_destination_forwards_byte_identical():
    app = _app_with_old_registration(with_dest=True)
    direct = runner.invoke(app, ["inbox", "outstanding", "list"])
    forwarded = runner.invoke(app, ["outstanding", "list"])
    assert direct.exit_code == 0 and forwarded.exit_code == 0
    # stdout of the old spelling is byte-identical to the new path: callers
    # parse stdout, so the shim must not edit it.
    assert forwarded.stdout == direct.stdout == "NEW-PATH\n"
    err = forwarded.stderr or ""
    assert err.count("is now") == 1
    assert "fno inbox outstanding" in err
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
    import fno.verb_moves as moves

    app = typer.Typer(cls=make_lazy_group_cls({}))

    @app.callback()
    def _cb() -> None: ...

    old_pr = typer.Typer()

    @old_pr.command("create")
    def _old_create() -> None:
        typer.echo("OLD-CREATE-RAN")

    app.add_typer(old_pr, name="pr", hidden=True)
    monkeypatch.setitem(
        moves.VERB_MOVES,
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

    assert VERB_MOVES, "the table must be populated for this test to mean anything"
    root = typer.main.get_command(app)
    ctx = click.Context(root)
    for name in VERB_MOVES:
        cmd = root.get_command(ctx, name)
        # x-6233: `yard` was an eager @app.command, so its release form is a
        # removed registration rather than a hidden lazy row - forwarding is
        # owned by VERB_MOVES, not by the registry. `decide` joined it when
        # the family ported native: every destination the move names is a
        # native verb now, so a python registration would forward into
        # nothing. `review` joined the same way when its ROOT claim moved to
        # the rust front door, which prints the move notice and execs
        # `do review` itself; a python registration would sit behind an
        # intercept that never reaches it. Every OTHER moved spelling keeps a
        # one-release registration, and any registration that exists must be
        # hidden: an advertised moved name teaches a doomed spelling.
        if name in ("decide", "yard", "review"):
            assert cmd is None, f"{name}'s registration was removed with the move"
            continue
        assert cmd is not None, f"moved spelling {name!r} must stay registered"
        assert getattr(cmd, "hidden", False), (
            f"moved spelling {name!r} must be hidden: an advertised moved name teaches a doomed spelling"
        )


# ---------------------------------------------------------------------------
# The four deletes: tombstones that teach, not "No such command"
# ---------------------------------------------------------------------------


def test_deleted_verb_refuses_naming_its_replacement():
    from fno.cli import app

    result = runner.invoke(app, ["upgrade"])
    assert result.exit_code != 0
    combined = (result.output or "") + (result.stderr or "")
    assert "`fno upgrade` was removed" in combined
    assert "`fno doctor update`" in combined, combined
