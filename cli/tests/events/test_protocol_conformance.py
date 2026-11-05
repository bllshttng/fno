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


def test_every_valid_fixture_conforms() -> None:
    assert VALID, f"no valid fixtures under {FIXTURES}"
    for path in VALID:
        validate(json.loads(path.read_text(encoding="utf-8")))


def test_every_invalid_fixture_is_rejected() -> None:
    assert INVALID, f"no invalid fixtures under {FIXTURES}"
    for path in INVALID:
        with pytest.raises(ValidationError):
            validate(json.loads(path.read_text(encoding="utf-8")))
