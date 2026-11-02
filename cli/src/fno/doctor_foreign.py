"""The wheel's foreign doctor leaves: evals and lint.

The native front door claims the doctor family: the report, bundle,
plugin-file, update and test leaves, the event door, and the transport
leaves (intel, stalls, harness, harness-matrix) whose exec legs died with
the front-door claims. This module serves the two leaves whose families
have not ported, so the CI spellings keep answering through the wheel:
``fno-py doctor lint <check>`` carries the repository checks and
``fno-py doctor evals`` forwards to the native evals dispatch. Bare
``doctor``, the family's own leaves, and the retired transports print the
front-door teaching and exit 2. Each row here dies in the PR whose family
ports its leaf.
"""

from __future__ import annotations

import typer

doctor_app = typer.Typer(
    name="doctor",
    help="Foreign doctor leaves: evals and lint; the family's own surfaces are native.",
    invoke_without_command=True,
    no_args_is_help=False,
    add_completion=False,
    hidden=True,
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


# Foreign leaves --------------------------------------------------------------

from fno.evals_cli import evals_app  # noqa: E402
from fno.lint_cli import lint  # noqa: E402

doctor_app.add_typer(evals_app, name="evals")
doctor_app.command("lint")(lint)


# The family's own leaves and the retired transports: registered so the
# teaching, not a generic unknown-command error, answers them on a
# Python-only install.

_SIGNPOST_SETTINGS = {"ignore_unknown_options": True, "allow_extra_args": True}


def event_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor event")


def update_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor update")


def bundle_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor bundle")


def plugin_file_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor plugin-file")


def test_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor test")


def intel_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor intel")


def stalls_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor stalls")


def harness_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor harness")


def harness_matrix_signpost(ctx: typer.Context) -> None:
    _signpost("fno doctor harness-matrix")


for _word, _fn in [
    ("event", event_signpost),
    ("update", update_signpost),
    ("bundle", bundle_signpost),
    ("plugin-file", plugin_file_signpost),
    ("test", test_signpost),
    ("intel", intel_signpost),
    ("stalls", stalls_signpost),
    ("harness", harness_signpost),
    ("harness-matrix", harness_matrix_signpost),
]:
    doctor_app.command(_word, hidden=True, context_settings=_SIGNPOST_SETTINGS)(_fn)
