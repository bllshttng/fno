"""The notify_watch tick phase (x-87fb): spawn, parse, emit, degrade.

The arm's sampler logic is Rust and tested there; these tests pin the seam:
the receipt becomes one tick row, and every failure shape - binary absent,
non-zero exit, unparseable stdout - lands as ``notify_failed`` without
raising out of the tick.
"""
import stat

import pytest

from fno.pr_watch import cli as pr_watch_cli


@pytest.fixture()
def rows(monkeypatch):
    """Capture the tick rows and the phase's view of the world."""
    captured = []
    monkeypatch.setattr(pr_watch_cli, "_emit_tick_row", lambda *a, **kw: captured.append((a, kw)))
    monkeypatch.setattr(pr_watch_cli, "_catchup_roots", lambda: [])
    return captured


def _fake_binary(tmp_path, script):
    binary = tmp_path / "fake-fno-agents"
    binary.write_text(script)
    binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
    return binary


def test_receipt_becomes_one_tick_row(monkeypatch, tmp_path, rows):
    monkeypatch.setattr(
        "fno.rust_binary.resolve_binary",
        lambda: _fake_binary(
            tmp_path,
            "#!/bin/sh\necho '{\"acted\": 2, \"skip_reason\": null, \"detail\": \"operator_question:sent\"}'\n",
        ),
    )
    pr_watch_cli._run_notify_watch_phase()
    assert len(rows) == 1
    args, kwargs = rows[0]
    assert args == ("notify_watch",)
    assert kwargs["interval_s"] == 300
    assert kwargs["acted"] == 2
    assert kwargs["skip_reason"] is None
    assert kwargs["detail"] == "operator_question:sent"


def test_absent_binary_is_notify_failed(monkeypatch, tmp_path, rows):
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: None)
    pr_watch_cli._run_notify_watch_phase()
    _, kwargs = rows[0]
    assert kwargs["skip_reason"] == "notify_failed"
    assert "absent" in kwargs["detail"]


def test_bound_overrun_reads_starved_not_failed(monkeypatch, tmp_path, rows):
    """The subprocess bound is the phase slice minus its reserve, so
    an overrun is a budget cut, not a broken arm. The row must read
    ``starved`` - "notify_failed" rides FAILURE_SKIPS and rendered a healthy
    loop as FAIL for 43 minutes."""
    import subprocess

    def _overrun(*_a, **_kw):
        raise subprocess.TimeoutExpired(cmd="fno-agents", timeout=28.0)

    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: tmp_path / "absent")
    monkeypatch.setattr("subprocess.run", _overrun)
    pr_watch_cli._run_notify_watch_phase()
    assert len(rows) == 1
    _, kwargs = rows[0]
    assert kwargs["skip_reason"] == "starved"
    assert "28s bound" in kwargs["detail"]
