"""The CPU axis reads one decider: the native census payload.

The decision itself computes in fno-agents (census --cause; pinned by the
Rust tests). What lives here is the gate's own edge of that contract: the
payload-to-Admission mapping and the unreadable-instrument refusal. The
admission math (shares, gap intervals, top-holder naming) is the census's
contract now.
"""
from __future__ import annotations

import pytest

from fno.agents import spawn_gate


@pytest.fixture(autouse=True)
def _quiet_edges(monkeypatch):
    """No unit test reads the real machine: the capacity is pinned."""
    monkeypatch.setattr(spawn_gate, "_load_cpus", lambda: 12)


def test_unreadable_instrument_refuses_with_the_error_text():
    admission = spawn_gate._cpu_axis(
        (None, "footprint unavailable: process table unavailable: timed out after 5.0s")
    )
    assert admission.verdict == "refuse"
    assert admission.axis == "cpu_instrument"
    assert "process table unavailable: timed out after 5.0s" in admission.reason
    assert "--force to bypass" in admission.reason


def test_payload_flows_through_verbatim():
    """The gate prints the census's verdict and reason verbatim: one
    decider means the Python side never rewrites the sentence."""
    payload = {
        "admission": {
            "verdict": "admit",
            "reason": "fleet CPU share admits: 0.10 < max_fleet_cpu_share 0.50",
            "share_low": 0.1,
            "share_high": 0.1,
            "bound": "exact",
            "fleet_cores": 1.2,
            "machine_cores": 6.0,
            "capacity_cores": 12.0,
            "ceiling": 0.5,
            "gap": None,
        }
    }
    admission = spawn_gate._cpu_axis((payload, None))
    assert admission.verdict == "admit"
    assert admission.axis == "fleet_cpu"
    assert admission.reason == (
        "fleet CPU share admits: 0.10 < max_fleet_cpu_share 0.50"
    )


def test_hold_carries_the_top_holder_and_gap():
    payload = {
        "admission": {
            "verdict": "hold",
            "reason": "top holder yes 16 procs 5.13 cores in .fno/worktrees/x-b1ee",
            "share_low": 0.5,
            "share_high": 1.0,
            "bound": "upper",
            "fleet_cores": 7.5,
            "machine_cores": 7.5,
            "capacity_cores": 12.0,
            "ceiling": 0.5,
            "gap": "3 pidless row(s) with no identity route (codex)",
            "top_holder": "yes 16 procs 5.13 cores in .fno/worktrees/x-b1ee",
        }
    }
    admission = spawn_gate._cpu_axis((payload, None))
    assert admission.verdict == "hold"
    assert admission.bound == "upper"
    assert admission.gap is not None
    assert "top holder" in admission.reason
