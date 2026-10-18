"""Tier-2 gate_escape emit machinery (x-91b5).

Covers what test_gate_escape_verb.py does not: distinct dedup buckets,
fail-open on an unwritable log (AC1-FR), and placeholder-PR normalization.
The spawn-cap bypass emit lives in the Rust gate, pinned there against
fixtures/gate_escape_spawn_cap_parity.json.
"""
from __future__ import annotations

import json
from pathlib import Path

from fno.events import gate_escape as ge


def _events(p: Path) -> list[dict]:
    from tests._event_rows import event_rows

    return event_rows(Path(p))


def _escapes(p: Path, reason: str = "spawn-cap") -> list[dict]:
    return [
        e
        for e in _events(p)
        if e.get("type") == "gate_escape" and e.get("data", {}).get("reason") == reason
    ]


def test_distinct_dedup_keys_count_separately(tmp_path):
    """A genuinely separate intervention (different bucket) is NOT collapsed."""
    ev = tmp_path / "events.jsonl"
    ge.emit_gate_escape("spawn-cap", dedup_key="day1", events_path=ev)
    ge.emit_gate_escape("spawn-cap", dedup_key="day2", events_path=ev)
    assert len(_escapes(ev)) == 2


def test_fail_open_on_unwritable_log(tmp_path, monkeypatch):
    """AC1-FR: an append failure never raises; a durable failure line is logged."""
    ev = tmp_path / "events.jsonl"
    import fno.events.store_client as store_client_mod

    def _boom(*a, **k):
        raise OSError("disk full")

    monkeypatch.setattr(store_client_mod, "emit_envelope", _boom)
    out = ge.emit_gate_escape("spawn-cap", dedup_key="k", events_path=ev)
    assert out is None
    assert _escapes(ev) == []
    fail_log = tmp_path / "gate_escape_emit_failures.jsonl"
    assert fail_log.exists()
    lines = [json.loads(x) for x in fail_log.read_text().splitlines() if x.strip()]
    assert len(lines) == 1 and lines[0]["reason"] == "spawn-cap"


def test_placeholder_pr_normalized_to_no_pr(tmp_path):
    """A pr<=0 (placeholder) is normalized to 'no PR' in one place, so the
    payload never carries a bogus pr and dedup does not key on it."""
    ev = tmp_path / "events.jsonl"
    ge.emit_gate_escape("flake", pr=0, dedup_key="k", events_path=ev)
    escapes = _escapes(ev, "flake")
    assert len(escapes) == 1
    assert "pr" not in escapes[0]["data"]
