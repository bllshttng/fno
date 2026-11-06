"""The native `fno backlog decompose` door.

The Python decompose command moved into the Rust dispatcher; these tests
exercise the door the way an operator does: one binary invocation per case,
the fixture root pinned (cwd, HOME, state dir) so no subprocess write
escapes to operator state.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

GROUPS = json.dumps(
    [
        {"slug": "one", "title": "Group One", "waves": "1"},
        {"slug": "two", "title": "Group Two", "waves": "2", "blocked_by_groups": ["one"]},
    ]
)

EPIC_ID = "ab-epic0001"


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    yield


def _door(tmp_path: Path, *args: str):
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (the smoke shard deletes it on purpose)")
    env = dict(os.environ)
    env["HOME"] = str(tmp_path)
    env["FNO_STATE_DIR"] = str(tmp_path / ".fno")
    env["FNO_TRACKER_BACKEND"] = "graph"
    return subprocess.run(
        [str(binary), "backlog", "decompose", *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=tmp_path,
        timeout=60,
    )


def test_decompose_door_refuses_a_planless_epic_and_names_contain(tmp_path: Path) -> None:
    """The plan-less-epic refusal names the folding command, not a dead end.

    An epic with no plan_path cannot address group fragments; the remedy is
    `fno backlog contain`, moved here from the wheel plan_base pin when the
    module went native.
    """
    from tests.fixtures.graph_seed import seed_graph

    (tmp_path / ".fno" / "db").mkdir(parents=True, exist_ok=True)
    planless = {
        "id": EPIC_ID,
        "parent": None,
        "title": "Epic",
        "type": "epic",
        "priority": "p1",
        "domain": "code",
        "blocked_by": [],
        "cwd": "",
        "created_at": "2026-01-01T00:00:00+00:00",
    }
    seed_graph(tmp_path / ".fno" / "db" / "graph.json", json.dumps({"entries": [planless]}) + "\n")
    res = _door(tmp_path, EPIC_ID, "--groups", GROUPS)
    assert res.returncode == 1, res.stdout + res.stderr
    assert "fno backlog contain" in res.stdout + res.stderr
