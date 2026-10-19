"""``agents.reap.roster_scope``: its default, and what a bad value does.

The knob names which rows the roster-side sweep may retire. The mirror
contract is the same one the sweep-knob seconds ride: a bad value degrades
to the default AND is named in ``DEGRADED``, because a typo must never
widen the sweep and a silent degrade reads exactly like a value nobody set.
"""
from __future__ import annotations

from fno.config import AgentsBlock
from fno.config._sweeps import DEGRADED


def test_each_scope_value_is_honored():
    for value in ("off", "provenanced", "all"):
        block = AgentsBlock(reap={"roster_scope": value})
        assert block.reap.roster_scope == value


def test_a_bad_value_degrades_and_is_named():
    DEGRADED.clear()
    block = AgentsBlock(reap={"roster_scope": "everything"})
    assert block.reap.roster_scope == "provenanced"
    assert DEGRADED["agents.reap.roster_scope"] == "'everything'"
