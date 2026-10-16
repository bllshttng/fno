"""CLI-layer tests for `fno do pr verify` and `fno do pr rebase` after the
in-package port (ab-d4c98550).

The verify body is native now, so the CLI-layer contract is the forward: the
same verb spelling hands `pr-verify` to the binary and propagates its exit
code. The audits themselves are characterized by the Rust golden replay over
the fixture the deleted Python leg froze.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


# ---------------------------------------------------------------------------
# fno do pr verify
# ---------------------------------------------------------------------------


def test_verify_help_renders():
    result = runner.invoke(app, ["do", "pr", "verify", "--help"])
    assert result.exit_code == 0


@pytest.fixture
def _capture_forward(monkeypatch):
    """Capture the forward instead of spawning a binary."""
    calls = {}

    def _forward(verb, args):
        calls["forward"] = (verb, args)

    monkeypatch.setattr("fno.pr.cli._forward_to_binary", _forward)
    return calls


def test_verify_kind_forwards_to_the_binary(_capture_forward):
    """--kind merged/reviews forward with the same flags."""
    r1 = runner.invoke(app, ["do", "pr", "verify", "--kind", "merged", "--pr-number", "42", "--state-file", "x.md"])
    assert r1.exit_code == 0, r1.exception
    assert _capture_forward["forward"] == (
        "pr-verify",
        ["--kind", "merged", "--pr-number", "42", "--state-file", "x.md"],
    )

    r2 = runner.invoke(app, ["do", "pr", "verify", "--kind", "reviews", "--pr-number", "7", "--state-file", "y.md"])
    assert r2.exit_code == 0, r2.exception
    assert _capture_forward["forward"] == (
        "pr-verify",
        ["--kind", "reviews", "--pr-number", "7", "--state-file", "y.md"],
    )


def test_verify_exit_code_propagates(monkeypatch):
    import typer

    def _forward(verb, args):
        raise typer.Exit(code=1)

    monkeypatch.setattr("fno.pr.cli._forward_to_binary", _forward)
    result = runner.invoke(
        app, ["do", "pr", "verify", "--kind", "merged", "--pr-number", "1", "--state-file", "x.md"]
    )
    assert result.exit_code == 1


def test_verify_invalid_kind_rejected():
    """--kind bogus exits rc=2 and names valid values."""
    result = runner.invoke(
        app, ["do", "pr", "verify", "--kind", "bogus", "--pr-number", "1", "--state-file", "x.md"]
    )
    assert result.exit_code == 2
    assert "merged" in result.output and "reviews" in result.output


def test_verify_missing_pr_number_yields_exit_2():
    result = runner.invoke(app, ["do", "pr", "verify", "--kind", "merged", "--state-file", "x.md"])
    assert result.exit_code == 2


def test_verify_missing_state_file_yields_exit_2():
    result = runner.invoke(app, ["do", "pr", "verify", "--kind", "merged", "--pr-number", "1"])
    assert result.exit_code == 2


# ---------------------------------------------------------------------------
# fno do pr rebase: contract is characterized against real git repos in
# test_pr_rebase.py; only the help surface is asserted here.
# ---------------------------------------------------------------------------


def test_rebase_help_renders():
    result = runner.invoke(app, ["do", "pr", "rebase", "--help"])
    assert result.exit_code == 0
