"""The legacy event vocabulary the removed `backlog inbox` alias left.

The alias is gone but its EVENTS are not. Sessions recorded `inbox_add` rows
before the rename and those rows still have to count, so the reader vocabulary
outlives the command. Dropping it would silently break the capture-pass gate for
any session that predates the rename.
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Generator

import pytest


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Generator[None, None, None]:
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    yield


# --------------------------------------------------------------------------
# AC4-EDGE + AC6-FR: event dual-read
# --------------------------------------------------------------------------

_seq = {"n": 0}


def _write_event(events_path: Path, etype: str, session_id: str) -> None:
    events_path.parent.mkdir(parents=True, exist_ok=True)
    from fno.events.store_client import emit_envelope

    _seq["n"] += 1
    emit_envelope(
        {
            "ts": "2026-07-24T%02d:%02d:00Z" % (_seq["n"] // 60, _seq["n"] % 60),
            "source": "backlog",
            "type": etype,
            "data": {"session_id": session_id, "fu_id": "fu-test"},
        },
        events_path,
    )


def _cap_native(tmp_path: Path, *args: str):
    """The native capture door pinned to the fixture root (cwd, HOME, state)."""
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (the smoke shard deletes it on purpose)")
    env = dict(_os.environ)
    env["HOME"] = str(tmp_path)
    env["FNO_STATE_DIR"] = str(tmp_path / ".fno")
    proc = _sp.run(
        [str(binary), "backlog", "capture", *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=tmp_path,
        timeout=60,
    )
    return proc


def test_capture_pass_counts_mixed_vocabulary(tmp_path: Path) -> None:
    """AC4-EDGE: one legacy inbox_add + one capture_add for the session => 2."""
    events = tmp_path / ".fno" / "events.jsonl"
    _write_event(events, "inbox_add", "SMIX")
    _write_event(events, "capture_add", "SMIX")
    # An unrelated session's event must not count.
    _write_event(events, "capture_add", "OTHER")

    res = _cap_native(tmp_path, "capture-pass", "--session-id", "SMIX")
    assert res.returncode == 0, res.stderr
    payload = json.loads(res.stdout.splitlines()[-1])
    assert payload["entries_written"] == 2
