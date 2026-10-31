"""Addressing: full sender provenance and collision-safe reply addresses.

The envelope carries the full session id as its reply address, so concurrent
sessions do not collide regardless of harness id format.

Kept in one module rather than split across the three legacy mail-test files,
because these behaviors form one contract.
"""
from __future__ import annotations


import pytest

from fno.harness_identity import (
    is_unsafe_short_address,
)

# Two real-shaped UUIDv7 ids from one clock bucket: identical head-8, distinct
# tails. This is the collision, written down.
V7_A = "01a025f8-09af-74b2-a461-998a18e3bfca"


@pytest.mark.parametrize(
    "token,harness,unsafe",
    [
        ("01a025f8", "codex", True),
        ("01a025f8", "claude", False),  # v4 head-8 is 32 random bits
    ],
)
def test_unsafe_short_address_keys_on_shape_and_harness(token, harness, unsafe):
    assert is_unsafe_short_address(token, harness) is unsafe


