"""Native event validation and the compiled schema's source consistency."""
from __future__ import annotations

import pytest

from fno.events.store_client import ValidationError, _native_json, build_event


def test_invalid_mission_status_is_refused() -> None:
    with pytest.raises(ValidationError, match="unknown status"):
        build_event("mission_complete", "megatron", {"mission_id": "mt-001", "status": "sideways"})


def test_invalid_integrity_warning_kind_is_refused() -> None:
    with pytest.raises(ValidationError, match="kind"):
        build_event("integrity_warning", "target", {
            "kind": "bogus_kind_value", "phase": "register",
            "session_id": "sess-77", "artifact_path": "/tmp/x.md",
        }, builder="integrity_warning")


def test_every_declared_source_is_legal_for_the_envelope() -> None:
    schema = _native_json(["schema"], "schema")
    allowed = set(schema["allowed_sources"])
    violations = [(name, source) for name, spec in schema["event_types"].items()
                  for source in spec["sources"] if source not in allowed]
    assert violations == []
