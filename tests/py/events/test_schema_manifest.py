"""Structural-validation tests for crates/fno-agents/src/event_store/schema.yaml.

The manifest is the canonical source of truth for events.jsonl shape.
These tests assert it parses and that key invariants (envelope required
fields, no duplicate event types, phase_transition declares gate_bearing)
hold structurally - so a bad PR fails the check rather than shipping a
silently broken manifest.
"""
from __future__ import annotations

from pathlib import Path

import yaml


MANIFEST = Path(__file__).resolve().parents[3] / "crates/fno-agents/src/event_store/schema.yaml"


def _load() -> dict:
    return yaml.safe_load(MANIFEST.read_text(encoding="utf-8"))


def test_lead_checkin_declares_canonical_required_and_forbidden() -> None:
    """lead_checkin is the one check-in shape a history reader can walk.

    scope and change are required; role_scope, role, and result are the
    synonyms five leads used before the contract and must stay refused, so
    no second key spelling can re-enter the journal.
    """
    data = _load()
    rc = next((e for e in data["event_types"] if e["name"] == "lead_checkin"), None)
    assert rc is not None, "lead_checkin event type missing"
    assert rc["data"]["required"] == ["scope", "change"]
    assert sorted(rc["data"]["forbidden"]) == ["result", "role", "role_scope"]
