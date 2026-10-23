"""A row stored under the legacy dispatch key still seeds a spawn.

A defer/undefer can leave a row with only the old key; the seam and the door
read ``dispatch_command``, so ``find_node_row`` must surface the old key there.
"""
from __future__ import annotations

import pytest

from fno.agents import node_dispatch


@pytest.mark.parametrize(
    ("stored", "expected"),
    [
        ({"id": "n-1", "dispatch_verb": "target"}, "target"),  # verb-wording: allow
        ({"id": "n-1", "dispatch_command": "think", "dispatch_verb": "target"}, "think"),  # verb-wording: allow
        ({"id": "n-1"}, None),
    ],
)
def test_legacy_key_surfaces_as_dispatch_command(monkeypatch, stored, expected):
    monkeypatch.setattr(node_dispatch, "_find_node_row", lambda _node: dict(stored))
    assert node_dispatch.find_node_row("n-1").get("dispatch_command") == expected
