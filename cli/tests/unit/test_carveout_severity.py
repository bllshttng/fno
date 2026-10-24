"""Carveout severity, stamped onto the ledger record.

A carveout filed to satisfy the fidelity gate is "a planned deliverable is
unbuilt" and carries its severity, which the retro classify (Rust) routes to
priority.
"""
from __future__ import annotations


import pytest

from fno.carveout.core import (
    CarveoutError,
    VALID_SEVERITIES,
    add_carveout,
    read_carveouts,
)


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
    assert VALID_SEVERITIES == ("critical", "high", "medium", "low")

