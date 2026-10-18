"""Rank is the operator's pin; an agent session votes instead (AC1)."""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from tests.fixtures.graph_seed import seed_graph
from fno.rust_binary import find_dev_binary, resolve_binary


def _binary() -> str:
    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    return str(binary)


def _seeded_store(tmp_path: Path, entries: list[dict]) -> None:
    (tmp_path / "config.toml").write_text("state_dir = \"%s\"\n" % tmp_path)
    seed_graph(tmp_path / "graph.json", json.dumps({"entries": entries}))


def _env(tmp_path: Path, **extra: str) -> dict:
    env = {**os.environ, "FNO_CONFIG": str(tmp_path / "config.toml"),
           "FNO_TRACKER_BACKEND": "graph"}
    env.update(extra)
    return env


def _rank(tmp_path: Path, *args: str, **env_extra: str):
    return subprocess.run(
        [_binary(), "backlog", "rank", *args],
        capture_output=True, text=True, env=_env(tmp_path, **env_extra),
        cwd=str(tmp_path), timeout=120,
    )


def _rank_of(tmp_path: Path, node_id: str):
    out = subprocess.run(
        [_binary(), "backlog", "get", node_id, "--field", "rank"],
        capture_output=True, text=True, env=_env(tmp_path),
        cwd=str(tmp_path), timeout=60,
    )
    assert out.returncode == 0, out.stderr
    value = out.stdout.strip()
    return None if value == "null" else value


def _agent_env() -> dict:
    return {"FNO_HARNESS_NAME": "claude", "FNO_HARNESS_SESSION_ID": "sess-agent-1"}


LOOSE = [
    {"id": "x-aaa1", "title": "First", "status": "ready", "priority": "p1",
     "project": "fno"},
    {"id": "x-bbb2", "title": "Second", "status": "ready", "priority": "p1",
     "project": "fno", "rank": -4.0},
]


def test_ac1_hp_agent_rank_top_refused(tmp_path):
    """An agent session is refused, writes no rank, and is told how to vote."""
    _seeded_store(tmp_path, LOOSE)

    result = _rank(tmp_path, "x-aaa1", "--top", **_agent_env())

    assert result.returncode != 0
    assert _rank_of(tmp_path, "x-aaa1") is None
    assert "operator-only" in result.stderr
    assert 'fno backlog encounter x-aaa1 --evidence "what it cost you"' in result.stderr
    assert "fno backlog update x-aaa1 --priority" in result.stderr


def test_ac1_hp_partial_harness_stamp_still_refused(tmp_path):
    """A half stamp reads as an agent: the fence fails closed."""
    _seeded_store(tmp_path, LOOSE)

    result = _rank(tmp_path, "x-aaa1", "--top", FNO_HARNESS_SESSION_ID="sess-agent-1")

    assert result.returncode != 0
    assert _rank_of(tmp_path, "x-aaa1") is None
    assert "operator-only" in result.stderr
