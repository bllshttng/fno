"""Tests for the a2a status-breakpoint protocol family.

The envelope rules are judged natively and pinned by the public fixtures in
test_protocol_conformance.py. These cover the Python builder and the public
schema subset staying in sync with the Python envelope allowlist.
"""
from __future__ import annotations

import json
from pathlib import Path

from fno.events import (
    PROTOCOL_ENVELOPE_ALLOWED,
    PROTOCOL_FAMILY_TYPES,
    _build,
)

REPO_ROOT = Path(__file__).resolve().parents[3]
PUBLIC_SCHEMA = REPO_ROOT / "schemas" / "events-protocol-v1.json"


# -- AC1-EDGE: non-session producer omits from/model entirely --

def test_build_omits_none_envelope_fields() -> None:
    event = _build(
        "task_started",
        "test",
        {"title": "t"},
        envelope={"v": 1, "run": "r1", "from": None, "model": None, "node": "prj-0001"},
    )
    assert "from" not in event  # omitted, not empty string
    assert "model" not in event
    assert event["node"] == "prj-0001"
    assert event["run"] == "r1"


# -- public schema subset: in sync with the envelope allowlist --

def test_public_schema_matches_validator_allowlist() -> None:
    schema = json.loads(PUBLIC_SCHEMA.read_text(encoding="utf-8"))
    # every declared property is an allowed envelope field, and vice versa
    assert set(schema["properties"]) == PROTOCOL_ENVELOPE_ALLOWED
    assert set(schema["properties"]["type"]["enum"]) == PROTOCOL_FAMILY_TYPES
