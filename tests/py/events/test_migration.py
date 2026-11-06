"""Tests for scripts/migrate-events-shape.py.

Covers: legacy {timestamp, source, type, data} rows migrate to the
canonical {ts, type, source, data} shape.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "scripts/migrate-events-shape.py"


def _write_events(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as fh:
        for row in rows:
            fh.write(json.dumps(row) + "\n")


def _read_events(path: Path) -> list[dict]:
    """Tolerant reader: skips blank and unparseable lines.

    Tests for the corrupt-preserved invariant rely on the migration script
    leaving bad lines in place; we don't want this helper to choke on the
    fragment when verifying the surrounding migrated rows.
    """
    out: list[dict] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out


@pytest.fixture
def workdir(tmp_path):
    (tmp_path / ".fno").mkdir()
    return tmp_path


def test_migrates_legacy_rows(workdir):
    target = workdir / ".fno/events.jsonl"
    legacy = [
        {
            "timestamp": "2026-05-07T09:30:42Z",
            "source": "target",
            "type": "phase_init",
            "data": {"phase": "register", "nonce": "n", "session_id": "s"},
        },
        {
            "timestamp": "2026-05-07T09:31:00Z",
            "source": "target",
            "type": "phase_transition",
            "data": {
                "gate_bearing": True,
                "gate": "ledger_updated",
                "phase": "register",
                "nonce": "n",
                "session_id": "s",
            },
        },
    ]
    _write_events(target, legacy)
    rc = subprocess.call([sys.executable, str(SCRIPT), "--root", str(workdir)])
    assert rc == 0
    out = _read_events(target)
    assert all({"ts", "type", "source", "data"} <= set(r.keys()) for r in out)
    assert all("timestamp" not in r for r in out)
    assert (workdir / ".fno/events.jsonl.bak").exists()
