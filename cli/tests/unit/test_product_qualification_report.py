"""Tests for the qualification report path (AC3)."""
from __future__ import annotations

import subprocess
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.evals.cli import evals_app

runner = CliRunner()

REPO_ROOT = Path(__file__).resolve().parents[3]
MANIFEST = REPO_ROOT / "evals" / "fixtures" / "product-delivery" / "qualification.json"


def _forwarder_fakes(monkeypatch: pytest.MonkeyPatch, tmp_path: Path, *, returncode: int = 0) -> dict:
    from fno import rust_binary

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: tmp_path / "fno-agents")
    captured: dict = {}

    def fake_run(argv, check=False, **kw):
        captured["argv"] = argv
        return subprocess.CompletedProcess(argv, returncode)

    monkeypatch.setattr(subprocess, "run", fake_run)
    return captured


def test_report_forwarder_passes_qualification_through(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    captured = _forwarder_fakes(monkeypatch, tmp_path)
    res = runner.invoke(evals_app, ["report", "--qualification", str(MANIFEST)])
    assert res.exit_code == 0
    argv = captured["argv"]
    assert argv[argv.index("--qualification") + 1] == str(MANIFEST)

