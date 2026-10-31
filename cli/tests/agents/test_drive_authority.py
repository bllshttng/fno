"""Tests for drive-authority detection (Phase 6 Wave 4, ab-8d258ddb).

`is_drive_authority_active()` / `active_drive_sessions()` are the gate-hardening
primitive: they report whether an operator holds an interactive/step/paranoid
drive window on any agent (LD3, LD24/29). Watch windows are read-only and must
NOT count.
"""
from __future__ import annotations

import json
from pathlib import Path

from fno.drive_authority import (
    active_drive_sessions,
    is_drive_authority_active,
)


def _write_state(agents_dir: Path, short_id: str, pty: dict | None) -> None:
    d = agents_dir / short_id
    d.mkdir(parents=True, exist_ok=True)
    state = {"schema_version": 1, "short_id": short_id, "status": "live"}
    if pty is not None:
        state["pty"] = pty
    (d / "state.json").write_text(json.dumps(state))


def _drive(active: bool, mode: str | None, session_id: str | None = "d-1") -> dict:
    return {
        "active": True,
        "drive_active": active,
        "drive_session_id": session_id,
        "drive_mode": mode,
    }


def test_interactive_step_paranoid_open_authority(tmp_path: Path) -> None:
    _write_state(tmp_path, "wkI", _drive(True, "interactive", "d-i"))
    _write_state(tmp_path, "wkS", _drive(True, "step", "d-s"))
    _write_state(tmp_path, "wkP", _drive(True, "paranoid", "d-p"))
    sessions = active_drive_sessions(tmp_path)
    assert {s["short_id"] for s in sessions} == {"wkI", "wkS", "wkP"}
    assert is_drive_authority_active(tmp_path) is True
    # Records carry mode + session_id.
    by_id = {s["short_id"]: s for s in sessions}
    assert by_id["wkS"]["mode"] == "step"
    assert by_id["wkP"]["session_id"] == "d-p"


def test_watch_does_not_open_authority(tmp_path: Path) -> None:
    # Watch never writes the state.json window in production, but even if a
    # window names mode=watch it must NOT count as authority. An unknown mode,
    # an inactive window and a row with no pty are not authority either.
    _write_state(tmp_path, "wkW", _drive(True, "watch"))
    _write_state(tmp_path, "wkX", _drive(True, "bogus-mode"))
    _write_state(tmp_path, "wkOff", _drive(False, "interactive"))
    _write_state(tmp_path, "wkNoPty", None)
    assert active_drive_sessions(tmp_path) == []
    assert is_drive_authority_active(tmp_path) is False


# --- operator-initiated audit tagging (cv-9def52a7) ------------------------


def test_emit_operator_initiated_envelope_and_data(tmp_path: Path) -> None:
    from fno.drive_authority import emit_operator_initiated

    events = tmp_path / ".fno" / "events.jsonl"
    emit_operator_initiated(
        "backlog_done_operator_initiated",
        source="backlog",
        events_path=events,
        task_id="ab-12345678",
    )
    from tests._event_rows import event_rows

    rows = event_rows(events)
    rec = rows[0]
    assert set(rec) == {"ts", "source", "type", "data"}
    assert rec["source"] == "backlog"
    assert rec["type"] == "operator_initiated"
    assert rec["data"] == {
        "action_type": "backlog_done_operator_initiated",
        "task_id": "ab-12345678",
    }


# --- CLI command --------------------------------------------------------------
