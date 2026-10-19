"""Tests for fno.events.log - event log with atomic append + audit."""
from __future__ import annotations

import multiprocessing
import sqlite3
from pathlib import Path

import pytest


# -- Helpers --

def _make_state_file(tmp_path: Path, session_id: str = "ses-abc123") -> Path:
    """Write a minimal target-state.md with a known session_id."""
    state_file = tmp_path / ".fno" / "target-state.md"
    state_file.parent.mkdir(parents=True, exist_ok=True)
    state_file.write_text(
        f"---\nsession_id: {session_id}\ncampaign_id: camp-001\nstatus: IN_PROGRESS\n---\n# body\n"
    )
    return state_file


def _events_file(tmp_path: Path) -> Path:
    return tmp_path / ".fno" / "events.jsonl"


# -- AC1-HP: emit writes one line per call --

def test_ac1_hp_emit_writes_one_line(tmp_path: Path) -> None:
    """AC1-HP: emit commits exactly one row per call."""
    from fno.events.log import emit_event, read_events

    _make_state_file(tmp_path, "ses-abc123")
    events_file = _events_file(tmp_path)
    events_file.parent.mkdir(parents=True, exist_ok=True)

    nonce = emit_event(
        event_type="phase_transition",
        payload={"phase": "ship", "gate_bearing": False},
        state_path=tmp_path / ".fno" / "target-state.md",
        events_path=events_file,
    )

    events = read_events(events_file)
    assert len(events) == 1
    event = events[0]

    # Required fields: envelope plus legacy identity under data
    assert event["type"] == "phase_transition"
    data = event["data"]
    assert data["session_id"] == "ses-abc123"
    assert data["campaign_id"] == "camp-001"
    assert len(data["nonce"]) == 32  # secrets.token_hex(16) = 32 hex chars
    assert "ts" in event
    assert data["phase"] == "ship"

    # emit_event returns the nonce
    assert nonce == data["nonce"]


# -- AC2-HP: emit is concurrency-safe --

def _worker_emit(args: tuple) -> None:
    """Worker function for concurrency test - importable at module level."""
    events_path_str, state_path_str, idx = args
    from fno.events.log import emit_event
    emit_event(
        event_type="phase_init",
        payload={"phase": "build", "worker": idx},
        state_path=state_path_str,
        events_path=events_path_str,
    )


@pytest.mark.timeout(60)
def test_ac2_hp_emit_concurrency_safe(tmp_path: Path) -> None:
    """AC2-HP: concurrent emits produce non-interleaved lines."""
    _make_state_file(tmp_path, "ses-concurrent")
    events_file = _events_file(tmp_path)
    events_file.parent.mkdir(parents=True, exist_ok=True)

    n_workers = 8
    args_list = [
        (str(events_file), str(tmp_path / ".fno" / "target-state.md"), i)
        for i in range(n_workers)
    ]

    ctx = multiprocessing.get_context("spawn")
    with ctx.Pool(n_workers) as pool:
        pool.map(_worker_emit, args_list)

    from fno.events.log import read_events

    events = read_events(events_file, session_id="ses-concurrent")
    assert len(events) == n_workers, f"Expected {n_workers} rows, got {len(events)}"

    for event in events:
        # Each emission is its own committed transaction (no lost updates)
        assert event["type"] == "phase_init"
        assert event["data"]["session_id"] == "ses-concurrent"


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


# -- AC3-HP: audit returns events for a session --

def test_ac3_hp_audit_returns_session_events(tmp_path: Path) -> None:
    """AC3-HP: audit filters by session_id and returns in order."""
    from fno.events.log import emit_event, read_events

    state_a = tmp_path / ".fno" / "state-a.md"
    state_a.parent.mkdir(parents=True, exist_ok=True)
    state_a.write_text("---\nsession_id: ses-A\ncampaign_id: camp-x\nstatus: IN_PROGRESS\n---\n")

    state_b = tmp_path / ".fno" / "state-b.md"
    state_b.write_text("---\nsession_id: ses-B\ncampaign_id: camp-x\nstatus: IN_PROGRESS\n---\n")

    events_file = _events_file(tmp_path)

    emit_event("phase_init", {"phase": "build"}, state_path=state_a, events_path=events_file)
    emit_event("phase_init", {"phase": "deploy"}, state_path=state_b, events_path=events_file)
    emit_event("gate_written", {"phase": "build"}, state_path=state_a, events_path=events_file)

    events_a = read_events(events_file, session_id="ses-A")
    assert len(events_a) == 2
    assert events_a[0]["type"] == "phase_init"
    assert events_a[1]["type"] == "gate_written"

    events_b = read_events(events_file, session_id="ses-B")
    assert len(events_b) == 1
    assert events_b[0]["type"] == "phase_init"


# -- AC4-HP: audit --strict detects gaps --

def test_ac4_hp_audit_strict_detects_gaps(tmp_path: Path) -> None:
    """AC4-HP: audit --strict returns ok:false when gate_written is missing for a phase."""
    from fno.events.log import emit_event, audit_session

    state_file = _make_state_file(tmp_path, "ses-gap")
    events_file = _events_file(tmp_path)

    # Emit phase_init for ship phase but NOT gate_written
    emit_event("phase_init", {"phase": "ship"}, state_path=state_file, events_path=events_file)

    result = audit_session(events_file, session_id="ses-gap", strict=True)
    assert result["ok"] is False
    assert "gaps" in result
    assert any("ship" in g and "gate_written" in g for g in result["gaps"])


def test_ac4_hp_audit_strict_passes_when_complete(tmp_path: Path) -> None:
    """AC4-HP: strict audit is ok when phase_init + gate_written both present."""
    from fno.events.log import emit_event, audit_session

    state_file = _make_state_file(tmp_path, "ses-complete")
    events_file = _events_file(tmp_path)

    emit_event("phase_init", {"phase": "ship"}, state_path=state_file, events_path=events_file)
    emit_event("gate_written", {"phase": "ship"}, state_path=state_file, events_path=events_file)

    result = audit_session(events_file, session_id="ses-complete", strict=True)
    assert result["ok"] is True


# -- A newer store schema is refused, never read as empty --


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
