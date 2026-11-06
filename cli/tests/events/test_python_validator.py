"""Native event validation and the compiled schema's source consistency."""
from __future__ import annotations


from fno.events.store_client import _native_json


def test_every_declared_source_is_legal_for_the_envelope() -> None:
    schema = _native_json(["schema"], "schema")
    allowed = set(schema["allowed_sources"])
    violations = [(name, source) for name, spec in schema["event_types"].items()
                  for source in spec["sources"] if source not in allowed]
    assert violations == []
