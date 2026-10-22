"""The macro command delegates to the native fno-agents binary (the fold lives in Rust)."""
from __future__ import annotations


from typer.testing import CliRunner

from fno.evals.cli import evals_app

runner = CliRunner()


def test_macro_refuses_without_the_binary(monkeypatch) -> None:
    from fno import rust_binary

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: None)
    result = runner.invoke(evals_app, ["macro"])
    assert result.exit_code == 2
    assert "fno-agents binary was not found" in result.output
