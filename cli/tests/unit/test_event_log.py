"""Tests for the native event-store transport."""
from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest


# -- emit_envelope lock retry --

class _Proc:
    def __init__(self, returncode: int, stderr: str = "", stdout: str = "") -> None:
        self.returncode = returncode
        self.stderr = stderr
        self.stdout = stdout


def _patch_sleep(monkeypatch) -> None:
    from fno.events import store_client

    monkeypatch.setattr(store_client.time, "sleep", lambda _s: None)


def test_emit_envelope_retries_a_locked_store(tmp_path: Path, monkeypatch) -> None:
    """A locked store absorbs the bounded retry instead of surfacing as
    unavailable: the first refusals read database-is-locked, the last commits."""
    import json as _json

    from fno.events import store_client

    _patch_sleep(monkeypatch)
    calls = {"n": 0}

    def flaky_run(cmd, **kwargs):
        calls["n"] += 1
        if calls["n"] < 3:
            return _Proc(1, stderr="error: store.db: database is locked")
        receipt = {"store": "db", "event_id": "ev-1", "seq": 1, "inserted": True}
        return _Proc(0, stdout=_json.dumps(receipt))

    monkeypatch.setattr(store_client.subprocess, "run", flaky_run)
    envelope = {"ts": "t", "type": "phase_init", "source": "hook", "data": {}}
    receipt = store_client.emit_envelope(envelope, tmp_path / "events.jsonl")
    assert receipt["inserted"] is True
    assert calls["n"] == 3


def test_emit_envelope_stops_retrying_other_refusals(tmp_path: Path, monkeypatch) -> None:
    """A non-lock refusal is not retried: one attempt, then the named raise."""
    import pytest

    from fno.events import store_client

    _patch_sleep(monkeypatch)
    calls = {"n": 0}

    def refusing_run(cmd, **kwargs):
        calls["n"] += 1
        return _Proc(1, stderr="error: envelope has no source")

    monkeypatch.setattr(store_client.subprocess, "run", refusing_run)
    envelope = {"ts": "t", "type": "phase_init", "source": "hook", "data": {}}
    with pytest.raises(store_client.EventStoreUnavailable, match="no source"):
        store_client.emit_envelope(envelope, tmp_path / "events.jsonl")
    assert calls["n"] == 1


def test_newer_store_schema_is_refused(tmp_path: Path) -> None:
    from fno.events import store_client

    events = tmp_path / "events.jsonl"
    db = store_client.store_db_path(events)
    conn = sqlite3.connect(db)
    conn.execute(
        "CREATE TABLE events (line TEXT, type TEXT, session_id TEXT, ts_ms INTEGER, "
        "reject_reason TEXT, retention_class TEXT)"
    )
    conn.execute("PRAGMA user_version = 99")
    conn.commit()
    conn.close()

    with pytest.raises(store_client.EventStoreUnavailable, match="newer than this build"):
        store_client.read_committed_lines(events)
