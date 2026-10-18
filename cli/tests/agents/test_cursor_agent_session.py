from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest


def test_cursor_agent_resume_rejects_empty_and_truncated_ids():
    from fno.agents.harnesses import cursor_agent

    with pytest.raises(cursor_agent.CursorAgentSessionError, match="empty"):
        cursor_agent.resume_argv("")

    with pytest.raises(cursor_agent.CursorAgentSessionError) as caught:
        cursor_agent.resume_argv("74db359a")
    message = str(caught.value)
    assert "8 hex characters" in message
    assert "an fno session handle, not a chat id" in message
    assert "cursor-agent create-chat" in message


def test_cursor_agent_create_chat_reads_one_line_then_kills_process(monkeypatch):
    from fno.agents.harnesses import cursor_agent

    read_fd, write_fd = os.pipe()
    os.write(write_fd, b"fadad56b-8008-45f5-b809-f9fab7074534\n")
    os.close(write_fd)

    class FakeProcess:
        def __init__(self):
            self.stdout = os.fdopen(read_fd, "rb")
            self.stderr = None
            self.killed = False
            self.waited = False

        def kill(self):
            self.killed = True

        def wait(self, timeout):
            self.waited = True
            return -9

    process = FakeProcess()
    monkeypatch.setattr(cursor_agent, "_subprocess_popen", lambda *a, **k: process)

    assert cursor_agent.create_chat("/tmp") == "fadad56b-8008-45f5-b809-f9fab7074534"
    assert process.killed
    assert process.waited


def test_cursor_agent_refuses_native_worktree_passthrough():
    from fno.agents.mux_spawn import DispatchAskError, build_pane_argv

    chat_id = "fadad56b-8008-45f5-b809-f9fab7074534"
    with pytest.raises(DispatchAskError, match="native worktree"):
        build_pane_argv(
            "cursor-agent",
            "",
            Path("/tmp/worktree"),
            False,
            chat_id,
            passthrough=["-w", "native"],
        )


def test_cursor_agent_permission_and_effort_mappings_fail_closed():
    from fno.agents.mux_spawn import DispatchAskError, effort_tokens, permission_pane_tokens

    assert permission_pane_tokens("cursor-agent", "force") == ["--force"]
    with pytest.raises(DispatchAskError, match="unmappable"):
        permission_pane_tokens("cursor-agent", "auto")
    with pytest.raises(DispatchAskError, match="no --effort flag"):
        effort_tokens("cursor-agent", "high")


def test_cursor_agent_worker_server_selector_is_exclusive_to_owner_tree():
    from fno.agents.harnesses import cursor_agent

    rows = [
        (100, 1, "/bin/zsh"),
        (200, 100, "/Users/test/cursor-agent worker-server"),
        (300, 999, "/Users/test/cursor-agent worker-server"),
        (400, 200, "/Users/test/cursor-agent worker-server"),
    ]

    assert cursor_agent.select_owned_worker_server_pids(rows, owner_pid=100) == [200, 400]


def test_cursor_agent_reaper_refuses_unreadable_captured_identity(monkeypatch):
    from fno.agents.harnesses import cursor_agent

    class FakeProcess:
        pid = 731

        def cmdline(self):
            return ["/Users/test/cursor-agent", "worker-server"]

        def terminate(self):
            raise AssertionError("an unreadable identity must not be signaled")

    class FakePsutil:
        class AccessDenied(Exception):
            pass

        class NoSuchProcess(Exception):
            pass

        class ZombieProcess(Exception):
            pass

        class TimeoutExpired(Exception):
            pass

        @staticmethod
        def Process(pid):
            assert pid == 731
            return FakeProcess()

    monkeypatch.setitem(sys.modules, "psutil", FakePsutil)
    monkeypatch.setattr(cursor_agent, "_process_start_token", lambda pid, psutil: None)

    with pytest.raises(RuntimeError, match="could not be confirmed"):
        cursor_agent.reap_detached_worker_servers(
            [cursor_agent.CursorWorkerServerHandle(pid=731, start_time=7)]
        )

