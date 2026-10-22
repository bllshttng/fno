"""Tests for the post-2026-05 inbox drain dispatcher (3 handlers)."""
from __future__ import annotations

from pathlib import Path

import pytest


@pytest.fixture
def inbox_root(tmp_path, monkeypatch):
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    return tmp_path


@pytest.fixture
def repo_root(tmp_path, monkeypatch):
    """Pin the .fno/ output tree to a tmp dir.

    Also redirects the inbox-drain auto-memory writer (otherwise it would
    write under ~/.claude/projects/ based on tmp_path's encoded form,
    leaving orphan dirs across the user's home after every test run).
    """
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("FNO_AUTO_MEMORY_DIR", str(tmp_path / "auto-memory"))
    return tmp_path


def test_question_drops_wake_signal_leaves_unread(inbox_root, repo_root):
    from fno.inbox.drain import drain_inbox
    from fno.inbox.store import write_new_thread, Kind, read_unread_threads

    handle = write_new_thread(
        "alice", "bob", Kind.QUESTION.value, "should we roll back the queue?",
    )

    results = drain_inbox(repo_root, "alice")
    assert len(results) == 1
    assert results[0].action == "wake_signal_dropped"
    assert results[0].kind == "question"

    # Wake signal landed in the repo's space wake-signals dir
    from fno.wake.signal import signals_dir
    sigs = list(signals_dir(repo_root).glob("wake-*.json"))
    assert len(sigs) == 1

    # Thread stays unread
    assert read_unread_threads("alice")[0].path == handle.path


def test_fyi_persist_memory_writes_memory_file(inbox_root, repo_root, tmp_path):
    from fno.inbox.drain import drain_inbox
    from fno.inbox.store import write_new_thread, Kind

    write_new_thread(
        "alice", "bob", Kind.FYI.value, "useful lesson body",
        persist_to_memory=True,
    )
    results = drain_inbox(repo_root, "alice")
    assert len(results) == 1
    assert results[0].action == "memory_written"
    memory_path = Path(results[0].memory_path)
    assert memory_path.exists()
    text = memory_path.read_text()
    assert "source_inbox_thread:" in text
    assert "useful lesson body" in text


def test_unknown_kind_in_thread_file_returns_unknown_kind(inbox_root, repo_root):
    """A thread file with a malformed/legacy kind should not crash drain."""
    from fno.inbox.drain import drain_inbox
    from fno.inbox.store import inbox_dir_for

    inbox = inbox_dir_for("alice")
    inbox.mkdir(parents=True)
    (inbox / "2026-05-08-bogus.md").write_text(
        "---\n"
        "thread_id: msg-aaa111\n"
        "from: bob\n"
        "to: alice\n"
        "kind: legacy-kind-value\n"
        "created: 2026-05-08T10:00:00Z\n"
        "---\n\n"
        "## msg-aaa111 · 2026-05-08T10:00:00Z · from:bob\n\n"
        "body text\n",
        encoding="utf-8",
    )

    results = drain_inbox(repo_root, "alice")
    assert len(results) == 1
    assert results[0].action == "unknown_kind"
    assert results[0].error is not None


def test_heads_up_create_node_marks_read(inbox_root, repo_root, monkeypatch):
    """Mock triage_thread to return create_node, mock subprocess for `fno new`."""
    from fno.inbox import drain as drain_mod
    from fno.inbox.store import (
        write_new_thread, Kind, read_unread_threads,
    )
    from fno.inbox.triage import TriagePlan

    write_new_thread(
        "alice", "bob", Kind.HEADS_UP.value, "add region column",
    )

    def fake_triage_thread(h, **_kwargs):
        return TriagePlan(
            action="create_node",
            title="add region column",
            priority="p2",
            body="proposed work item",
            follow_up_question=None,
        )

    class _FakeRun:
        def __init__(self, returncode=0, stdout="", stderr=""):
            self.returncode = returncode
            self.stdout = stdout
            self.stderr = stderr

    def fake_run(cmd, *args, **kwargs):
        # The dedupe classifier resolves the same stubbed binary; answer its
        # shape classify locally (one tagless body) so the stub never
        # reaches a real exec.
        if cmd[1:3] == ["mail-envelope", "--classify"]:
            return _FakeRun(stdout='[{"ids": []}]')
        # The native create door: drain resolves fno-agents and argv[0] is
        # its path, so the fake matches on the command words, not the binary.
        if cmd[1:3] == ["backlog", "new"]:
            return _FakeRun(stdout='{"id": "ab-abc123", "title": "t"}\n')
        raise AssertionError(f"unexpected subprocess: {cmd}")

    monkeypatch.setattr(drain_mod, "triage_thread", fake_triage_thread)
    monkeypatch.setattr(drain_mod.subprocess, "run", fake_run)
    # The door resolver: a stubbed path keeps the fake subprocess matching
    # (argv[0] is the resolved binary) without a filesystem probe.
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: Path("/fake/fno-agents"))

    results = drain_mod.drain_inbox(repo_root, "alice")
    assert len(results) == 1
    assert results[0].action == "created_node"
    assert results[0].node_id == "ab-abc123"

    assert read_unread_threads("alice") == []


def test_send_kind_drains_with_fyi_semantics(inbox_root, repo_root):
    """codex #459 P1: kind=send threads (from `fno agents mail send`) must drain
    (fyi semantics: surface + mark read), not loop forever as unknown_kind."""
    from fno.inbox.drain import drain_inbox
    from fno.inbox.store import write_new_thread, Kind, read_unread_threads

    write_new_thread("alice", "bus-peer", Kind.SEND.value, "fyi built the thing")
    results = drain_inbox(repo_root, "alice")
    assert len(results) == 1
    assert results[0].action != "unknown_kind"
    assert results[0].action == "dismissed"
    # Thread is consumed (marked read), not stranded unread.
    assert read_unread_threads("alice") == []
