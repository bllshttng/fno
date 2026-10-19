"""Tests for the Rust-owned global pause sentinel adapter."""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

runner = CliRunner()


@pytest.fixture
def isolated_home(tmp_path: Path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    yield tmp_path


def test_resume_all_is_a_passthrough_to_the_rust_binary(isolated_home, monkeypatch, tmp_path):
    from fno import loops

    stub = tmp_path / "fno-agents-stub"
    stub.write_text("#!/bin/sh\necho \"got: $@\"\nexit 7\n")
    stub.chmod(0o755)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: stub)

    result = runner.invoke(loops.loops_app, ["resume-all"])
    assert result.exit_code == 7, result.output
    assert "got: loops resume-all" in result.output


def test_bad_rust_result_fails_closed_and_names_binary(isolated_home, monkeypatch, caplog):
    from fno import loops

    def fail(*_args, **_kwargs):
        raise RuntimeError("fno-agents binary /tmp/fno-agents returned bad JSON")

    monkeypatch.setattr(loops, "_rust_loops_call", fail)
    assert loops.loops_paused() is True
    assert "/tmp/fno-agents" in caplog.text


def test_cli_status_formats_epoch_milliseconds_as_iso(isolated_home, monkeypatch):
    from fno import loops

    monkeypatch.setattr(
        loops,
        "_rust_loops_call",
        lambda action, args=None: {
            "paused": True,
            "state": "paused",
            "who": "tester",
            "paused_at": 1_789_261_620_000,
            "expires_at": 1_789_265_220_000,
        },
    )
    result = runner.invoke(loops.loops_app, ["status"])
    assert result.exit_code == 0, result.output
    assert "since 2026-09-13T01:07:00Z" in result.output
    assert "expires 2026-09-13T02:07:00Z" in result.output
    assert "1789261620000" not in result.output


