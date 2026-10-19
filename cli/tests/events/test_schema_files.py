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
STATUS_V1 = SCHEMAS_DIR / "status-v1.json"

PYTHON_SOURCES = [
    "target", "megawalk", "megatron", "fno-loop",
    "hook", "subagent", "migration", "test", "backlog",
]

AGENT_STATUS_VALUES = [
    "spawning", "ready", "idle", "busy", "live",
    "restarting", "orphaned", "failed", "exited", "permanent_dead",
]


# ---------------------------------------------------------------------------
# AC1-HP: Canonical schemas exist and parse
# ---------------------------------------------------------------------------


def test_events_v3_is_single_envelope() -> None:
    """The W7 oneOf split-brain is retired: one unified envelope, no oneOf."""
    schema = json.loads(EVENTS_V3.read_text())
    assert "oneOf" not in schema, "events-v3.json must be a single envelope"
    required = set(schema.get("required", []))
    assert {"ts", "type", "source", "data"} == required


def test_events_v3_source_enum_contains_sources() -> None:
    """The unified source anyOf must enumerate the fixed-string sources."""
    schema = json.loads(EVENTS_V3.read_text())
    branches = schema["properties"]["source"]["anyOf"]
    enum_vals = next((b["enum"] for b in branches if "enum" in b), [])
    for src in [*PYTHON_SOURCES, "daemon", "active-backlog"]:
        assert src in enum_vals, f"source enum missing {src!r}"


def test_status_v1_status_enum() -> None:
    """status-v1.json status property must enumerate all AgentStatus values."""
    schema = json.loads(STATUS_V1.read_text())
    status_enum = schema["properties"]["status"]["enum"]
    for val in AGENT_STATUS_VALUES:
        assert val in status_enum, f"status enum missing {val!r}"
