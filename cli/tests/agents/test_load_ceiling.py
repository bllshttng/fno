"""x-c588: the CPU axis is read from one decider, never computed twice.

The decision itself is ``cpu_admission`` in doctor_footprint, pinned against
the shared fixture in test_footprint_verb.py. What lives here is the gate's
own edge of that contract: ``_cpu_axis``'s mapping of a reading or an error
to an Admission, and the platform-tolerance edges. The hold and debounce
behavior lives in test_fleet_load_governor.py.
"""
from __future__ import annotations

import pytest

from fno.agents import spawn_gate
from fno.footprint import Footprint


def _reading(
    fleet: float, measured: float, gap: str | None = None, top: list | None = None
) -> Footprint:
    return Footprint(
        sustained_cpu_cores=0.0,
        descendant_cpu_cores=0.0,
        fleet_cpu_cores=fleet,
        descendant_process_count=0,
        direct_process_count=0,
        transient_call_count=0,
        process_count=0,
        measured_cpu_cores=measured,
        top=top or [],
        unparsed_lines=0,
        attribution_gap=gap,
    )


@pytest.fixture(autouse=True)
def _quiet_edges(monkeypatch):
    """No unit test reads the real machine: the share ceiling is the
    stock default and the capacity is pinned."""
    from fno import doctor_footprint

    monkeypatch.setattr(doctor_footprint, "_admission_config", lambda: 0.5)
    monkeypatch.setattr(spawn_gate, "_load_cpus", lambda: 12)


def test_unreadable_instrument_refuses_with_the_error_text():
    admission = spawn_gate._cpu_axis(
        (None, "footprint unavailable: process table unavailable: timed out after 5.0s")
    )
    assert admission.verdict == "refuse"
    assert admission.axis == "cpu_instrument"
    assert "process table unavailable: timed out after 5.0s" in admission.reason
    assert "--force to bypass" in admission.reason


def test_load_707_admits_when_the_fleet_share_is_under_its_ceiling(monkeypatch):
    """x-c588 regression, the 2026-09-17 20:28 reading: load_15m 707.5 on
    12 cpus with the fleet holding 1.961 of 12.00 cores. The load number
    decides nothing; the share admits and names itself."""

    monkeypatch.setattr(spawn_gate.os, "getloadavg", lambda: (500.0, 600.0, 707.5))
    admission = spawn_gate._cpu_axis((_reading(1.961, 6.9), None))
    assert admission.verdict == "admit"
    assert admission.axis == "fleet_cpu_share"
    assert "max_fleet_cpu_share" in admission.reason


def test_gapped_reading_refuses_inside_the_interval():
    gap = "3 pidless row(s) with no identity route (codex)"
    admission = spawn_gate._cpu_axis((_reading(2.1, 7.2, gap), None))
    assert admission.verdict == "undecidable"
    assert "17.5%" in admission.reason and "60.0%" in admission.reason
    assert gap in admission.reason


def test_hold_names_the_top_holder():
    """The hold names who holds the cores, from the same rows the
    number was summed from. The specimen is the measured 2026-09-11 refusal:
    16 yes rows summing 513.0 %cpu out of one worktree's repro loop."""
    top = [(32.0625, "yes > .fno/worktrees/x-b1ee/repro.out")] * 16
    admission = spawn_gate._cpu_axis((_reading(7.5, 7.5, top=top), None))
    assert admission.verdict == "hold"
    holder = "yes 16 procs 5.13 cores in .fno/worktrees/x-b1ee"
    assert f"top holder {holder}" in admission.reason
    assert admission.top_holder == holder


