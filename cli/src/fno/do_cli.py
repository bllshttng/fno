"""``fno do`` - delivery-work command group.

The child apps are the same objects used by their one-release top-level
compatibility spellings.  Nesting them here changes only routing: each command
keeps its existing parser, output, and implementation.  The delivery group's
body lives in the Rust delivery front now, so its mount here is a forward
leaf in the claims-forward shape: option signatures unchanged, transport only.
"""

from __future__ import annotations

from pathlib import Path

import typer

from fno.loops import loops_app
from fno.phase import phase_app
from fno.plan import plan_app
from fno.pr_watch.cli import cli as pr_watch_app
from fno.provenance.cli import think_app
from fno.research import research_command
from fno.resume.cli import cli as resume_app
from fno.state.cli import cli as state_app
from fno.stub_manifest import stub_manifest_app
from fno.target_cli import target_app


do_app = typer.Typer(
    name="do",
    help="Delivery work: plans, targets, reviews, PRs, state, and supporting operations.",
    no_args_is_help=True,
)

# The pr namespace's 29 spellings answer natively now (the fno front door
# claims `do pr` and the fno-agents pr dispatcher routes them); the wheel
# keeps only the two Python mounts under the same group.
do_pr_app = typer.Typer(
    name="pr",
    help="PR lifecycle operations (native commands; watch and stub-manifest stay here).",
    no_args_is_help=True,
    add_completion=False,
)
do_pr_app.add_typer(pr_watch_app, name="watch")
do_pr_app.add_typer(stub_manifest_app, name="stub-manifest")


delivery_app = typer.Typer(
    name="delivery",
    help="Evaluate generic delivery evidence.",
    no_args_is_help=True,
)


@delivery_app.command("evaluate")
def delivery_evaluate(
    plan_path: Path = typer.Option(..., "--plan-path"),
    events: Path | None = typer.Option(None, "--events"),
    json_output: bool = typer.Option(False, "--json", "-J"),
) -> None:
    """Evaluate one plan's generic delivery evidence against its journal.

    The evaluation body lives in the Rust delivery front now
    (``fno-agents delivery-evaluate``); Python owns transport only, the same
    split as the mail hold forward.
    """
    from fno.paths import project_events_json

    events_path = str(events) if events is not None else str(project_events_json())
    argv = ["--plan-path", str(plan_path), "--events", events_path]
    if json_output:
        argv.append("--json")
    _forward_delivery(argv)


def _forward_delivery(argv: list[str]) -> None:
    """Forward one delivery command to the bundled fno-agents binary."""
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        typer.echo(
            "fno delivery evaluate: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    # Captured and re-emitted through typer: the leaf's output IS the
    # operator surface, and a CliRunner-hosted caller (the tests) must see it.
    result = subprocess.run(
        [str(binary), "delivery-evaluate", *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


do_app.add_typer(delivery_app, name="delivery")
do_app.add_typer(loops_app, name="loops")
do_app.add_typer(phase_app, name="phase")
do_app.add_typer(plan_app, name="plan")
do_app.add_typer(do_pr_app, name="pr")
do_app.command("research")(research_command)
do_app.add_typer(resume_app, name="resume")
do_app.add_typer(state_app, name="state")
do_app.add_typer(target_app, name="target")
do_app.add_typer(think_app, name="think")
