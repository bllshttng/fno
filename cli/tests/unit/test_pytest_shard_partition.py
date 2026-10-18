"""Tests for the pytest collection shard used by the smoke matrix."""

from __future__ import annotations

import os

import pytest

from tests.conftest import pytest_collection_modifyitems
from tests.conftest import partition_pytest_nodeids


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


def test_AC2_INV_shard_selector_is_consumed_before_test_bodies(monkeypatch) -> None:
    """Nested pytest probes must not inherit the parent collection slice."""
    items = [_Item("tests/unit/test_real.py::test_case")]
    monkeypatch.setenv("FNO_PYTEST_SHARD", "1/1")

    pytest_collection_modifyitems(items)

    assert [item.nodeid for item in items] == ["tests/unit/test_real.py::test_case"]
    assert "FNO_PYTEST_SHARD" not in os.environ


@pytest.mark.parametrize("spec", ["5/4", "four/4"])
def test_AC9_EDGE_malformed_pytest_shard_fails_loudly(spec: str) -> None:
    with pytest.raises(pytest.UsageError, match="FNO_PYTEST_SHARD"):
        partition_pytest_nodeids(["tests/unit/test_real.py::test_case"], spec)


def test_AC6_ERR_empty_pytest_shard_fails_loudly() -> None:
    with pytest.raises(pytest.UsageError, match="selected no tests"):
        partition_pytest_nodeids(["tests/unit/test_real.py::test_case"], "2/2")
