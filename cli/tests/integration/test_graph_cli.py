"""The p0 birth gate, measured through the native backlog door."""
from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

from fno.rust_binary import backlog_rows
from tests.goldens._door import native_backlog


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    g = tmp_path / "graph.json"
    # The empty store makes the layout ladder answer this root spelling.
    sqlite3.connect(tmp_path / "graph.db").close()
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    return g


def test_new_p0_requires_breaking_acknowledgment(tmp_graph):
    """Every CLI birth path applies the p0 acknowledgment before mutation."""
    refused = native_backlog("backlog", "new", "Not actually broken", "--priority", "p0")
    assert refused.exit_code != 0
    assert "p0 blocks everything else, usually a bug" in refused.output + refused.stderr
    assert backlog_rows(path=tmp_graph) == []

    accepted = native_backlog(
        "backlog", "new", "Broken service", "--priority", "p0", "--blocks-everything"
    )
    assert accepted.exit_code == 0, accepted.output + accepted.stderr
    assert backlog_rows(path=tmp_graph)[0]["blocks_everything"] is True
