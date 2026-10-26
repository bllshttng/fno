"""Tests for config-write event receipts."""
from __future__ import annotations

import fno.events as events
from fno.config.writer import (
    _emit_write_receipts,
    set_config_value,
)


def _capture_events(monkeypatch):
    captured = []
    monkeypatch.setattr(
        events,
        "append_event",
        lambda event, events_path=None: captured.append((event, events_path)),
    )
    return captured


def test_fresh_set_emits_one_receipt_with_presence_and_real_path(tmp_path, monkeypatch) -> None:
    captured = []
    monkeypatch.setattr(
        events,
        "append_event",
        lambda event, events_path=None: captured.append((event, events_path)),
    )

    set_config_value(
        "config.review.self_review_required",
        "false",
        scope="project",
        repo_root=tmp_path,
    )

    assert len(captured) == 1
    event, journal = captured[0]
    assert event["data"] == {
        "key": "review.self_review_required",
        "scope": "project",
        "root_kind": "project",
        "config_path": str(tmp_path / ".fno" / "config.toml"),
        "present_before": False,
        "present_after": True,
        "new_value": False,
        "attester_session_id": "",
        "attester_witness": "env_only",
    }
    assert journal.name == "events.jsonl"


def test_secret_key_values_are_digest_redacted(tmp_path, monkeypatch) -> None:
    captured = _capture_events(monkeypatch)
    _emit_write_receipts(
        {"api": {"key": "before"}},
        {"api": {"key": "after"}},
        tmp_path / "config.toml",
        "project",
    )

    assert len(captured) == 1
    data = captured[0][0]["data"]
    assert data["redacted"] is True
    assert data["old_value"].startswith("<redacted:sha256:")
    assert data["new_value"].startswith("<redacted:sha256:")
    assert "before" not in data["old_value"]
    assert "after" not in data["new_value"]


def test_append_failure_does_not_fail_config_write(tmp_path, monkeypatch, capsys) -> None:
    def fail_append(*args, **kwargs):
        raise OSError("journal unavailable")

    monkeypatch.setattr(events, "append_event", fail_append)
    set_config_value(
        "config.review.self_review_required", "false", scope="project", repo_root=tmp_path
    )

    assert (tmp_path / ".fno" / "config.toml").exists()
    assert "review.self_review_required" in capsys.readouterr().err
