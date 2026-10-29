"""Sender-side stamping for the control lane (tool-boundary delivery).

A control body that demoted durable must reach the recipient at its next
TOOL boundary, not wait for the prompt boundary a busy worker never
reaches. The durable write stamps a per-recipient pending flag the
PreToolUse hook gates on; the drain itself is the fno-agents
``mail-control-drain`` command (see crates/fno-agents/src/mail_control_drain.rs).
"""
from __future__ import annotations

import pytest

from fno.paths_testing import use_tmpdir

MARKERS = ("CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID", "GEMINI_SESSION_ID")
MY_SID = "ffffabcd1234"  # canonical_handle -> ffffabcd (first-eight)
MY_HANDLE = "ffffabcd"


@pytest.fixture
def env(tmp_path, monkeypatch):
    use_tmpdir(monkeypatch, tmp_path)
    for m in MARKERS:
        monkeypatch.delenv(m, raising=False)
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", MY_SID)
    return tmp_path


def _flag_path(form):
    from fno import paths

    return paths.bus_dir() / "control-pending" / f"{form}.flag"


def test_the_control_lane_stamps_pending_flags(env):
    from fno.inbox.store import write_new_thread

    _ordinary_write_leaves_no_flag(env)

    msg_id = write_new_thread(
        MY_HANDLE,
        sender="lead",
        kind="send",
        body="control: freeze - hold",
    )
    assert msg_id
    assert _flag_path(MY_HANDLE).exists()


def _ordinary_write_leaves_no_flag(env):
    from fno.inbox.store import write_new_thread

    write_new_thread(MY_HANDLE, sender="alice", kind="send", body="plain status")
    assert not _flag_path(MY_HANDLE).exists()
