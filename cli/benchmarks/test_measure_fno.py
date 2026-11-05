"""Tests for the fno-in-target measurement harness (Phase 01 gate).

One table per family; each row is a distinct branch of the decision rule or
the parser's strict-type contract.
"""

from __future__ import annotations



from measure_fno_in_target import apply_decision_rule


def test_decision_rule_boundary_rows():
    """0.15 and 0.30 fall into the HIGHER bucket (>= boundaries)."""
    rows = [
        (0.10, "abort_daemon"),
        (0.149, "abort_daemon"),
        (0.15, "reads_only_v1"),
        (0.20, "reads_only_v1"),
        (0.299, "reads_only_v1"),
        (0.30, "full_v1"),
        (0.50, "full_v1"),
    ]
    for ratio, want in rows:
        assert apply_decision_rule(ratio) == want, ratio
