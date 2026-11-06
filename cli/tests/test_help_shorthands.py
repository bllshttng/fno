"""`fno help` shorthand legend (Phase 3, US3).

The legend is the discoverability half of the short-flag convention: the
global UPPERCASE register and the per-command ``-p`` meanings live behind
``fno help shorthands``, and the bare ``fno help`` output points at it.

The substring assertions below check only that the legend SAYS something, and
that let three dead rows sit here: ``fno agents ask -p provider`` (``-p`` is a
loud tombstone off ``spawn``), ``fno gate verify`` (no such command anywhere),
and ``fno backlog done -p pr-number (-l link, -m note)`` (that flag surface
belongs to the deprecated root ``fno done``). A substring of prose passes
whether or not the flag is real, so the row-level checks at the bottom read
the live command tree instead.
"""
from __future__ import annotations

import re

from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


# Two renderers are in play: plain click prints `-F, --force`, rich-formatted
# typer prints `--pr-number  -p`. Match a short only where a long sits beside
# it, in either order, so prose hyphens in the docstring are never mistaken for
# a flag.


def _resolves(path: str) -> None:
    """Assert a legend row resolves somewhere real: the Python tree, or the
    native dispatcher for a command the port moved into the binary (the Python
    leg is deleted, so the in-process app refuses it with "No such command")."""
    result = runner.invoke(app, [*path.split(), "--help"])
    if result.exit_code == 0:
        return
    from tests._native_door import run_native

    # The binary's mail group has no `agents` word above it.
    code, _out, _err = run_native(*path.removeprefix("agents ").split(), "--help")
    assert code == 0, f"legend names `fno {path}`, which does not resolve"


def test_legend_names_no_dead_command() -> None:
    """Every `fno <path>` a legend row names still resolves."""
    from fno.cli import SHORTHAND_LEGEND

    rows = [
        line.strip()[len("fno ") :].split("  ")[0].strip()
        for line in SHORTHAND_LEGEND.splitlines()
        if line.startswith("  fno ")
    ]
    assert len(rows) >= 5, "legend rows stopped parsing; fix the parse, not the legend"
    for cell in rows:
        cell = re.sub(r"\s*\(.*\)\s*$", "", cell).strip()
        parts = cell.split()
        # A row may name a family: `backlog add/idea/update/intake`.
        leaves = parts[-1].split("/") if "/" in parts[-1] else [parts[-1]]
        for leaf in leaves:
            _resolves(" ".join(parts[:-1] + [leaf]))
