"""Carveout severity, stamped by provenance and routed to priority (x-cbab AC6).

A carveout filed to satisfy the fidelity gate is, by construction, "a planned
deliverable is unbuilt" and lands high/p1 - not p3. The routing already existed
(``retro.classify.severity_to_priority`` + ``_resolve_priority`` reads
``item.severity``); only the field was missing. This test pins the plumbing end
to end: the record carries severity, harvest reads it, classify routes it.
"""
from __future__ import annotations


import pytest

from fno.carveout.core import (
    CarveoutError,
    VALID_SEVERITIES,
    add_carveout,
    read_carveouts,
)
from fno.retro.classify import _resolve_priority, severity_to_priority
from fno.retro.types import KIND_CARVEOUT, RawItem


def test_add_carveout_stamps_severity_onto_the_record(tmp_path):
    cv, _ = add_carveout(
        tmp_path, kind="deferred", description="unbuilt deliverable",
        severity="high", storage_root=tmp_path,
    )
    assert cv.severity == "high"
    rec = read_carveouts(tmp_path)[0]
    assert rec["severity"] == "high"


def test_invalid_severity_is_refused(tmp_path):
    with pytest.raises(CarveoutError):
        add_carveout(
            tmp_path, kind="deferred", description="x",
            severity="bogus", storage_root=tmp_path,
        )


def test_explicit_priority_still_wins_over_severity():
    """A hand-filed --priority overrides severity (precision beats provenance
    when the filer was more specific)."""
    item = RawItem(
        kind=KIND_CARVEOUT, text="x", severity="high", priority="p2"
    )
    assert _resolve_priority(item) == "p2"


def test_all_severities_route():
    # critical lands p1, not p0: p0 means a breaking bug ONLY (d-36de723f),
    # and an external review severity badge is not a broken service.
    assert severity_to_priority("critical") == "p1"
    assert severity_to_priority("high") == "p1"
    assert severity_to_priority("medium") == "p2"
    assert severity_to_priority("low") == "p3"
    assert severity_to_priority(None) == "p3"
    assert VALID_SEVERITIES == ("critical", "high", "medium", "low")
