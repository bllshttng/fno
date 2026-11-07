"""Tests for the pytest collection shard used by the smoke matrix."""
from __future__ import annotations


import pytest

from conftest import pytest_collection_modifyitems
from conftest import partition_pytest_nodeids


class _Item:
    def __init__(self, nodeid: str) -> None:
        self.nodeid = nodeid

    def add_marker(self, marker: object) -> None:
        del marker

    def get_closest_marker(self, name: str) -> None:
        del name
        return None


def test_AC2_HP_shard_selects_one_disjoint_slice(monkeypatch) -> None:
    nodeids = [f"tests/unit/test_real_{index}.py::test_case" for index in range(8)]
    items = [_Item(nodeid) for nodeid in nodeids]
    monkeypatch.setenv("FNO_PYTEST_SHARD", "2/4")

    pytest_collection_modifyitems(items)

    assert [item.nodeid for item in items] == [nodeids[1], nodeids[5]]


def test_AC9_EDGE_malformed_pytest_shard_fails_loudly() -> None:
    with pytest.raises(pytest.UsageError, match="FNO_PYTEST_SHARD"):
        partition_pytest_nodeids(["tests/unit/test_real.py::test_case"], "5/4")
