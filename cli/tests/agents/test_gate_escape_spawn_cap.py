"""Tier-2 gate_escape spawn-cap auto-emit + parity (x-91b5).

Covers the shared emit machinery: fail-open (AC1-FR), dedup (AC2-INV) and
the fail-loud argument checks. The test-context guard lives in the Rust gate
and is pinned there against the shared fixture.
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


def test_dedup_collapses_burst(tmp_path):
    """AC2-INV: three emits sharing (reason, dedup_key) count once."""
    ev = tmp_path / "events.jsonl"
    for _ in range(3):
        ge.emit_gate_escape("spawn-cap", dedup_key="sess:day", events_path=ev)
    assert len(_escapes(ev)) == 1


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


def test_bad_reason_raises_no_event(tmp_path):
    """A bad reason fails closed (ValidationError propagates); emits nothing."""
    import pytest

    from fno.events import ValidationError

    ev = tmp_path / "events.jsonl"
    with pytest.raises(ValidationError):
        ge.emit_gate_escape("flek", dedup_key="k", events_path=ev)
    assert not ev.exists()


def test_pr_and_dedup_key_together_raises(tmp_path):
    """The dedup contract is fail-loud: passing both pr AND dedup_key is a
    caller bug (already_emitted would OR-match and miscount), so it raises
    rather than silently emitting."""
    import pytest

    ev = tmp_path / "events.jsonl"
    with pytest.raises(ValueError):
        ge.emit_gate_escape("flake", pr=7, dedup_key="k", events_path=ev)
    assert not ev.exists()
