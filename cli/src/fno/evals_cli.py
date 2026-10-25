"""fno doctor evals - forwarding leaf over the native evals dispatch.

The fold commands route to their existing native owners; the run pipeline,
graduate, grade, export and qualify landed in `crates/fno-agents/src/evals/`
as the leg deleted (the dual-implementation protocol). Python owns transport
only. The goldens under crates/fno-agents/tests/golden/evals/ pin the bytes;
the characterization test proves the door still answers them.
"""

from __future__ import annotations

import typer

from fno.tombstones import tombstone_group_cls

evals_app = typer.Typer(
    name="evals",
    help="Golden-task bank (run / report / graduate) + research-brief grading (grade).",
    no_args_is_help=True,
    cls=tombstone_group_cls("evals"),
)


def _forward(command: str, argv: list[str]) -> None:
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
            "fno doctor evals: the fno-agents binary was not found. It ships in the "
            "`pip install fno` wheel and with the plugin; reinstall fno or run "
            "`fno doctor update --rust`, or set FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    result = subprocess.run(
        [str(binary), "evals", command, *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout, nl=False)
    if result.stderr:
        typer.echo(result.stderr, nl=False, err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@evals_app.command(
    "run",
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def run_cmd(ctx: typer.Context) -> None:
    """Run bank tasks in disposable worktrees and grade them mechanically."""
    _forward("run", list(ctx.args))


@evals_app.command(
    "trend",
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def trend_cmd(ctx: typer.Context) -> None:
    """Score the recent window against the prior one; exit 4 when regressed."""
    _forward("trend", list(ctx.args))


@evals_app.command(
    "report",
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def report_cmd(ctx: typer.Context) -> None:
    """Fold evals history: tiers, pass@1, pass^k, flakes, alarm. Exit 4 on alarm."""
    _forward("report", list(ctx.args))


@evals_app.command(
    "macro",
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def macro_cmd(ctx: typer.Context) -> None:
    """Find recurring labelled failure patterns in existing event journals."""
    _forward("macro", list(ctx.args))


@evals_app.command(
    "export",
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def export_cmd(ctx: typer.Context) -> None:
    """Export train-cohort history for prompt tuning. Exit 2 = setup error."""
    _forward("export", list(ctx.args))


@evals_app.command(
    "qualify",
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def qualify_cmd(ctx: typer.Context) -> None:
    """Aggregate held-out qualification results: counts and coverage only."""
    _forward("qualify", list(ctx.args))


@evals_app.command(
    "graduate",
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def graduate_cmd(ctx: typer.Context) -> None:
    """Retag a capability task to the regression tier (a reviewed edit)."""
    _forward("graduate", list(ctx.args))


@evals_app.command(
    "grade",
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def grade_cmd(ctx: typer.Context) -> None:
    """Grade a research brief against a golden doc (three mechanical assertions)."""
    _forward("grade", list(ctx.args))
