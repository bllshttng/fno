"""Tests for the managed block readers.

The fence reads as `none`, `both` or `malformed`, and only a well-formed pair
yields a version or a block.

Run: fno doctor test -- -k managed_block
"""
from __future__ import annotations

from fno.setup.managed_block import (
    BLOCK_VERSION,
    extract_block,
    marker_state,
    render_block,
    stamped_version,
)

_USER = "# My House Rules\n\nDo the thing. Do not break the build.\n"


def test_readers_accept_one_ordered_pair_and_refuse_a_lone_marker() -> None:
    block = render_block()
    host = _USER + "\n" + block + "\n"
    assert marker_state(_USER) == "none"
    assert stamped_version(_USER) is None
    assert marker_state(host) == "both"
    assert stamped_version(host) == BLOCK_VERSION
    assert extract_block(host) == block

    broken = _USER + "\n<!-- fno:begin v=1 -->\nstuff\n"  # no end marker
    assert marker_state(broken) == "malformed"
    assert extract_block(broken) is None
