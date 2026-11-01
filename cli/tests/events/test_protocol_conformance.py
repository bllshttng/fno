"""Conformance test for the a2a status-breakpoint golden fixtures (x-dbaf, US5).

The fixtures under schemas/fixtures/events-protocol-v1/ are the static artifacts
third-party adapter authors build against. This test gives them teeth: every
valid/ fixture must pass the canonical validator, every invalid/ fixture must
fail it (proving the additionalProperties:false + outcome-enum rules bite), and
the duplicate-emission fixture demonstrates the (run, task, type) dedup key.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.events.store_client import ValidationError, validate

REPO_ROOT = Path(__file__).resolve().parents[3]
FIXTURES = REPO_ROOT / "schemas" / "fixtures" / "events-protocol-v1"
VALID = sorted((FIXTURES / "valid").glob("*.json"))
INVALID = sorted((FIXTURES / "invalid").glob("*.json"))


@pytest.mark.parametrize("path", VALID, ids=lambda p: p.name)
def test_valid_fixture_conforms(path: Path) -> None:
    validate(json.loads(path.read_text(encoding="utf-8")))


@pytest.mark.parametrize("path", INVALID, ids=lambda p: p.name)
def test_invalid_fixture_rejected(path: Path) -> None:
    with pytest.raises(ValidationError):
        validate(json.loads(path.read_text(encoding="utf-8")))


# -- no leaked internal ids in the public fixtures --

def test_fixtures_use_placeholder_ids_only() -> None:
    import re

    # This repo's internal ids use the x-/ab- prefixes (e.g. x-dbaf, ab-ca822421);
    # public examples must use the prj- placeholder instead.
    leak = re.compile(r"\b(?:x|ab)-[0-9a-f]{4,}\b")
    for path in VALID + INVALID:
        text = path.read_text(encoding="utf-8")
        assert not leak.search(text), f"possible internal id leak in {path.name}"
