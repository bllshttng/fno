"""The public protocol schema matches the native event validator."""
from __future__ import annotations

import json
from pathlib import Path

from fno.events.store_client import _native_json


def test_public_schema_matches_validator_allowlist() -> None:
    path = Path(__file__).resolve().parents[3] / "schemas" / "events-protocol-v1.json"
    public = json.loads(path.read_text(encoding="utf-8"))
    native = _native_json(["schema"], "schema")["protocol_family"]
    assert set(public["properties"]) == set(native["envelope_allowed"])
    assert set(public["properties"]["type"]["enum"]) == set(native["types"])
