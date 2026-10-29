"""The CPU probe the Rust gate reads: its knobs stay pinned.

The probe transport tests (the cause_main console-script parity, the
two-runtime budget race) died with the crossing: the Rust gate now reads
the census in-process, and the census owns the admission math (pinned by
the Rust tests). What stays is the config knob the gate shares.
"""
from __future__ import annotations

import pytest


def test_non_finite_never_disarms_a_machine_guard():
    """`nan` loses every comparison and `inf` wins every one.

    Either way the ceiling stops refusing while still reading as
    configured, which is worse than a value that is merely wrong. All
    sibling knobs shared the hole, so all are checked.
    """
    from fno.config import AgentsBlock

    fields = {
        "min_free_gb": 4.0,
        "max_load_per_cpu": 8.0,
        "max_fleet_cpu_share": 0.5,
    }
    for name, default in fields.items():
        for bad in ("nan", "inf", "-inf", "NaN", float("nan"), float("inf")):
            got = getattr(AgentsBlock(**{name: bad}), name)
            assert got == default, f"{name}={bad!r} coerced to {got}, not {default}"
