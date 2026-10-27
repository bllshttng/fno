"""The Python side of the doctor forward: the foreign leaves only.

The native front door claims the doctor family (report, bundle, plugin-file,
update); this module serves the leaves whose families have not ported, so
``fno doctor test`` (the repo's documented runner) and the CI spellings keep
answering through the wheel. Bare ``doctor``, the family's own leaves, and
any unknown word print the front-door teaching and exit 2. Each row here
dies in the PR whose family ports its leaf.
"""

from __future__ import annotations

import typer

class _TestGroup(typer.core.TyperGroup):
    """The test command is Click-based; add it outside the typer decorator."""

    def __init__(self, *args, **kwargs) -> None:
        super().__init__(*args, **kwargs)
        self.add_command(test_command, name="test")


doctor_app = typer.Typer(
    name="doctor",
    help="Foreign doctor leaves: test, event, evals, lint, harness and friends.",
    invoke_without_command=True,
    no_args_is_help=False,
    add_completion=False,
    hidden=True,
    cls=_TestGroup,
)

@doctor_app.callback(invoke_without_command=True)
def _teach(ctx: typer.Context) -> None:
    """The front door owns the family's own surfaces; name the door instead."""
    if ctx.invoked_subcommand is None:
        _signpost("fno doctor")


def _signpost(spelling: str) -> None:
    typer.echo(
        f"{spelling}: the doctor family is native to the Rust front door; "
        "install it from the source checkout with "
        "`cargo install --locked --path <src>/crates/fno`, then re-run "
        "`fno doctor update`."
    )
    raise typer.Exit(code=2)


# Sub-apps (foreign families) ----------------------------------------------

from fno.codemap_cli import app as codemap_app  # noqa: E402
from fno.evals_cli import evals_app  # noqa: E402
from fno.events.cli import cli as event_app  # noqa: E402
from fno.status_fanout import status_fanout_app  # noqa: E402
from fno.test_cmd import test_command  # noqa: E402
from fno.lint_cli import lint  # noqa: E402
from fno.route_cli import inventory_cmd  # noqa: E402

doctor_app.add_typer(codemap_app, name="codemap")
doctor_app.add_typer(evals_app, name="evals")
doctor_event_app = typer.Typer(name="event", no_args_is_help=True, add_completion=False)
doctor_event_app.registered_callback = event_app.registered_callback
doctor_event_app.registered_commands.extend(event_app.registered_commands)
doctor_event_app.registered_groups.extend(event_app.registered_groups)
doctor_event_app.add_typer(status_fanout_app, name="fanout")
doctor_app.add_typer(doctor_event_app, name="event")
doctor_app.command("lint")(lint)
# The reachability read the deleted doctor_cli mounted (its :150); the leaf
# itself is route_cli's and stays Python.
doctor_app.command("route", hidden=True)(inventory_cmd)


# The family's own leaves: registered so the teaching, not a generic
# unknown-command error, answers them on a Python-only install.


@doctor_app.command(
    "update",
    hidden=True,
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def update_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor update")


@doctor_app.command(
    "bundle",
    hidden=True,
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def bundle_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor bundle")


@doctor_app.command(
    "plugin-file",
    hidden=True,
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def plugin_file_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor plugin-file")


# Transport leaves: thin argv rewriters onto the fno-agents binary ----------


@doctor_app.command("intel", hidden=True)
def intel_command() -> None:
    """Who typed: per-session provenance counters (the binary's flag surface)."""
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno doctor intel: the fno-agents binary was not found; run `fno doctor update --rust`.",
            err=True,
        )
        raise typer.Exit(code=2)
    result = subprocess.run([str(binary), "intel"], check=False)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@doctor_app.command(
    "stalls",
    hidden=True,
    context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
)
def stalls_command(ctx: typer.Context) -> None:
    """Why work stalls: one cause table over the event journals."""
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.paths import event_journals
    from fno.rust_binary import resolve_binary

    args = list(ctx.args)
    binary = resolve_binary()
    if binary is None:
        typer.echo("fno doctor stalls: no fno-agents binary; run `fno doctor update --rust`.", err=True)
        raise typer.Exit(code=2)
    argv = [str(binary), "stalls", *args]
    if not any(a == "--events" or a.startswith("--events=") for a in args):
        for path in event_journals():
            argv += ["--events", str(path)]
    done = subprocess.run(argv, check=False, capture_output=True, text=True)
    if done.stdout:
        typer.echo(done.stdout, nl=False)
    if done.stderr:
        typer.echo(done.stderr, nl=False, err=True)
    raise typer.Exit(code=propagate_returncode(done.returncode))


def _route_harness(argv: list[str]) -> None:
    from fno.agents.rust_runtime import refuse_without_binary, route_to_rust
    from fno.rust_binary import resolve_installed_binary

    binary = resolve_installed_binary() or refuse_without_binary("harness")
    route_to_rust(argv, binary=binary)


@doctor_app.command("harness", hidden=True)
def harness_command(
    harness: str = typer.Argument(...),
    live: bool = typer.Option(False, "--live"),
    json_out: bool = typer.Option(False, "--json", "-J"),
) -> None:
    _route_harness(
        ["harness-probe", "rubric", harness] + ["--live"] * live + ["--json"] * json_out
    )


@doctor_app.command("harness-matrix", hidden=True)
def harness_matrix_command(
    write: bool = typer.Option(False, "--write"),
) -> None:
    _route_harness(["harness-matrix"] + ["--write"] * write)
