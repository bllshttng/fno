"""The strict graph reader distinguishes populated, empty, and unreadable stores."""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.graph.store import GraphUnreadableError, read_graph_strict


def test_unreadable_store_names_its_database_path(tmp_path: Path):
    graph = tmp_path / "graph.json"
    database = graph.with_suffix(".db")
    database.write_bytes(b"not sqlite")
    with pytest.raises(GraphUnreadableError) as exc:
        read_graph_strict(graph)
    assert str(database) in str(exc.value)


def test_absent_store_returns_empty(tmp_path: Path):
    assert read_graph_strict(tmp_path / "does-not-exist.json") == []
