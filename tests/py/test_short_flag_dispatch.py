"""Runtime dispatch proofs for the lowercase short flags.

One short-vs-long parity proof per previously-untested risk:

* ``backlog find`` short-vs-long parity (read-only graph path, native binary).

``config accounts add`` is answered by fno-agents; its goldens in
``crates/fno-agents/tests/golden/accounts_add`` hold both spellings.
"""
from __future__ import annotations
from fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest


# --------------------------------------------------------------------------- #
# Parity: backlog find (read-only graph path).
# --------------------------------------------------------------------------- #

@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    g = tmp_path / "graph.json"
    # The native binary resolves the store through FNO_CONFIG's state_dir.
    (tmp_path / "config.toml").write_text(f'state_dir = "{tmp_path}"\n')
    monkeypatch.setenv("FNO_CONFIG", str(tmp_path / "config.toml"))
    return g


def _native_find(*args: str) -> tuple[int, str, str]:
    from _native_door import run_native

    return run_native("backlog", "find", *args)


def test_backlog_find_short_flags_match_long(tmp_graph: Path) -> None:
    """AC4: `backlog find -p X -s Y -d Z -J` is byte-identical to the long form.

    The Phase 2 lowercase table's find pin moved here with the find port:
    the native binary owns the surface, so the -p/-s/-d decls are readable
    only at this door.
    """
    seed_graph(tmp_graph, json.dumps({"entries": [
        {"id": "ab-sf000001", "title": "Short flag rollout", "status": "done",
         "domain": "code", "project": "fno"},
        {"id": "ab-sf000002", "title": "Unrelated thing", "status": "ready",
         "domain": "docs", "project": "other"},
    ]}) + "\n")
    long_code, long_out, long_err = _native_find(
        "rollout", "--project", "fno", "--status", "done", "--domain", "code", "--json",
    )
    short_code, short_out, _short_err = _native_find(
        "rollout", "-p", "fno", "-s", "done", "-d", "code", "-J",
    )
    assert long_code == 0, long_err
    assert short_code == long_code
    assert short_out == long_out
    assert "ab-sf000001" in short_out
