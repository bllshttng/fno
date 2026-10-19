"""Runtime dispatch proofs for the two-spelling canonicalization.

``test_short_flag_convention.py`` proves the declaration shape (canonical
long visible, legacy long hidden) via static AST scan, and
``tests/unit/test_flag_aliases.py`` proves the merge helper's semantics in
isolation. This test drives the REAL root app, so the sub-app imports,
registers, and parses, and its help hides the deprecated alias - the one
thing neither of the other two can see.
"""
from __future__ import annotations

import re

import pytest
from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()

_ANSI = re.compile(r"\x1b\[[0-9;]*m")


def _clean(text: str) -> str:
    """Strip ANSI styling so substring asserts see contiguous flag names."""
    return _ANSI.sub("", text)


@pytest.mark.parametrize(
    "argv,canonical,legacy",
    [
        (["worker", "external", "--help"], "--pr-number", "--pr"),
    ],
    ids=["worker-external"],
)
def test_legacy_spelling_hidden_from_help(
    argv: list[str], canonical: str, legacy: str
) -> None:
    """Help shows the canonical spelling and hides the deprecated alias.

    The legacy spelling is a prefix of the canonical one, so a plain
    substring check would match the canonical form. Instead: every
    occurrence of the legacy spelling must be part of a canonical
    occurrence (equal counts means no standalone legacy flag in help).
    """
    result = runner.invoke(app, argv)
    assert result.exit_code == 0, result.output
    out = _clean(result.output)
    assert canonical in out, f"{canonical} missing from {argv}"
    assert out.count(legacy) == out.count(canonical), (
        f"deprecated {legacy} visible in {argv}"
    )
