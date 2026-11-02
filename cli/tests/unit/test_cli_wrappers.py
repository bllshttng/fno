"""Cross-wrapper smoke tests: all 8 new fno subcommands respond to --help.

Task 02.2 of plan 2026-05-11-fno-cli-promotion-wrappers.
"""
from __future__ import annotations

from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()
_ENV = {"COLUMNS": "240", "NO_COLOR": "1", "TERM": "dumb"}


# ── fno backlog get: the native exec shim ──
#
# The graph-mode read ladder is the native binary's; the Python surface execs
# it. Several ids is the batch shape whose whole job is the argv it hands the
# binary, so that argv is what these pin.

_FAKE_GET_BIN = "/fake/bin/fno-agents"


class _StubGetResult:
    def __init__(self, returncode: int = 0):
        self.returncode = returncode


def _patch_get_binary(monkeypatch, binary=_FAKE_GET_BIN):
    import fno.rust_binary
    from pathlib import Path

    monkeypatch.setattr(
        fno.rust_binary, "resolve_binary", lambda: Path(binary) if binary else None
    )






