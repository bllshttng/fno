"""Tests for canonical JSON Schema files (Task 1.1).

Verifies that schemas/events-v3.json and status-v1.json parse as valid
JSON Schema documents with the correct structure.
"""
from __future__ import annotations

import json
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCHEMAS_DIR = REPO_ROOT / "schemas"
EVENTS_V3 = SCHEMAS_DIR / "events-v3.json"


# ---------------------------------------------------------------------------
# AC1-HP: Canonical schemas exist and parse
# ---------------------------------------------------------------------------


def test_events_v3_is_single_envelope() -> None:
    """The W7 oneOf split-brain is retired: one unified envelope, no oneOf."""
    schema = json.loads(EVENTS_V3.read_text())
    assert "oneOf" not in schema, "events-v3.json must be a single envelope"
    required = set(schema.get("required", []))
    assert {"ts", "type", "source", "data"} == required
