"""Integration: a queue-armed merge lands later, and the remote ref still goes.

``finalize`` can hand a PR to GitHub's queue, and when the queue lands the
merge no fno process is in the loop. The watcher tick is the detector, so it
pays the post-merge cleanup through the ``merge-remote-delete`` op on the
authorized-merge door. The fork-safety and idempotence contracts live in
``crates/fno-agents/src/pr_verify.rs`` (``remote_delete_cleanup``); what this
file proves is the seam those unit tests cannot see: which op the finisher
calls, and how the cleanup word it answers is rendered onto the event surface.
"""
from __future__ import annotations

from pathlib import Path


def _auto_merge(delete_branch: bool):
    from fno.config import AutoMergeBlock

    return AutoMergeBlock(enabled=True, delete_branch_on_merge=delete_branch)


def _arm_door(monkeypatch, *, cleanup: str) -> None:
    def _door(command, payload, timeout=None):
        assert command == "authorized-merge", command
        assert payload["op"] == "merge-remote-delete", payload
        assert payload["pr"] == 1042
        return {"cleanup": cleanup}

    monkeypatch.setattr("fno.rust_binary.command_call", _door)


def test_a_failed_cleanup_is_emitted_once(tmp_path, monkeypatch):
    from fno.pr_watch._dispatch import _finish_queue_merge

    _arm_door(monkeypatch, cleanup="ref delete refused: denied\n")
    monkeypatch.setattr(
        "fno.config.load_settings_for_repo",
        lambda _p: type("S", (), {"auto_merge": _auto_merge(True)})(),
    )
    emitted: list[tuple] = []
    _finish_queue_merge(Path(tmp_path), 1042, lambda kind, data: emitted.append((kind, data)))

    assert emitted == [
        ("pr_watch_cleanup_failed", {"pr": 1042, "cleanup": "ref delete refused: denied\n"})
    ]


def test_a_clean_cleanup_emits_no_failure_event(tmp_path, monkeypatch):
    from fno.pr_watch._dispatch import _finish_queue_merge

    _arm_door(monkeypatch, cleanup="")
    monkeypatch.setattr(
        "fno.config.load_settings_for_repo",
        lambda _p: type("S", (), {"auto_merge": _auto_merge(True)})(),
    )
    emitted: list[tuple] = []
    _finish_queue_merge(Path(tmp_path), 1042, lambda kind, data: emitted.append((kind, data)))
    assert emitted == []


def test_a_disabled_delete_branch_gates_the_door_call(tmp_path, monkeypatch):
    from fno.pr_watch._dispatch import _finish_queue_merge

    def _door(command, payload, timeout=None):
        raise AssertionError(f"cleanup must not reach the door: {command} {payload!r}")

    monkeypatch.setattr("fno.rust_binary.command_call", _door)
    monkeypatch.setattr(
        "fno.config.load_settings_for_repo",
        lambda _p: type("S", (), {"auto_merge": _auto_merge(False)})(),
    )
    _finish_queue_merge(Path(tmp_path), 1042, lambda kind, data: None)
