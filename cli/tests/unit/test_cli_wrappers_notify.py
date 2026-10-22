"""Unit tests for `fno notify` - in-package OS notification helper (US2).

Formerly a wrapper that sourced scripts/lib/notify.sh; the dispatch is now
internalized in fno.notify._impl, so the command runs from the installed package
with no repo-root path on disk (AC2-HP) and degrades loudly when no OS
notification tool is present (AC2-FR).
"""
from __future__ import annotations

from types import SimpleNamespace

from typer.testing import CliRunner

from fno.cli import app
from fno.notify import _impl

runner = CliRunner()


def test_notify_degrades_loudly_when_no_tool(monkeypatch):
    """AC2-FR: with neither osascript nor notify-send, exit non-zero with a
    one-line message - never a silent no-op, never a traceback."""
    # Force the no-tool path: non-Darwin platform and notify-send absent.
    monkeypatch.setattr(_impl.platform, "system", lambda: "Linux")
    monkeypatch.setattr(_impl.shutil, "which", lambda _name: None)

    result = runner.invoke(app, ["notify", "title", "message"])
    assert result.exit_code != 0
    assert "no OS notification tool available" in result.output


def test_notify_impl_darwin_dispatches_osascript(monkeypatch, tmp_path):
    """Success-path parity: on macOS the helper invokes osascript and returns 0
    even if osascript itself fails (best-effort, matching the former bash)."""
    calls: list = []

    def _stub_run(cmd, **kwargs):
        calls.append(list(cmd))

        class _R:
            returncode = 1  # tool failed; helper must still return 0

        return _R()

    monkeypatch.setattr(_impl.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(_impl.subprocess, "run", _stub_run)
    # This test asserts that a dispatch HAPPENS, so it opts out of the
    # hermetic suppression the whole suite runs under - `fno.hermetic` stamps
    # FNO_TEST_HERMETIC=1 to keep every other test off the operator's screen.
    # The unsuppressed run also emits the operator_notice journal row (x-5f06),
    # so the journal must be pinned inside the sandbox.
    monkeypatch.setenv("FNO_TEST_HERMETIC", "0")
    monkeypatch.setenv("FNO_EVENTS_PATH", str(tmp_path / "events.jsonl"))
    code, err = _impl.send_notification("T", "M")
    assert code == 0
    assert err == ""
    # The journal leg runs first (status sinks depend on it), so the fake's
    # first recorded call is the journal append, not the toast; the toast
    # must still be dispatched.
    assert any(c[0] == "osascript" for c in calls)


def _headless(monkeypatch):
    """A host with neither notifier: Linux, no notify-send, dispatch suppressed."""
    monkeypatch.setattr(_impl.platform, "system", lambda: "Linux")
    monkeypatch.setattr(_impl.shutil, "which", lambda _name: None)
    monkeypatch.setenv("FNO_TEST_HERMETIC", "0")


def _sink_settings(monkeypatch, events):
    """load_settings returning one enabled sink routing exactly `events`."""

    class _S:
        status_sinks = [
            SimpleNamespace(name="s", enabled=True, events=list(events)),
        ]

    monkeypatch.setattr("fno.config.load_settings", lambda: _S())


def test_headless_with_sink_routes_operator_notice_delivers(monkeypatch, tmp_path):
    """The no-local-notifier host delivers through the sink lane (x-5f06).

    The journal row lands carrying the pointer, and the return reads 0: the
    channel count is honest in the direction the old contract got wrong.
    """
    _headless(monkeypatch)
    _sink_settings(monkeypatch, ["operator_notice"])
    monkeypatch.setenv("FNO_EVENTS_PATH", str(tmp_path / "events.jsonl"))

    code, err = _impl.send_notification("T", "3 open questions.", "fno inbox outstanding")

    assert (code, err) == (0, "")
    from tests._event_rows import event_rows

    rows = event_rows(tmp_path / "events.jsonl")
    notices = [r for r in rows if r["type"] == "operator_notice"]
    assert len(notices) == 1
    assert notices[0]["data"] == {
        "title": "T",
        "body": "3 open questions.",
        "pointer": "fno inbox outstanding",
    }
