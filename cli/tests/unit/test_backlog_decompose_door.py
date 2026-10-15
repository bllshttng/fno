"""The native `fno backlog decompose` door.

The Python decompose verb moved into the Rust dispatcher; these tests
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


def _seed_epic(home: Path) -> None:
    """One epic with a plan_path, seeded through the live write path."""
    from tests.fixtures.graph_seed import seed_graph

    (home / ".fno" / "db").mkdir(parents=True, exist_ok=True)
    epic = {
        "id": EPIC_ID,
        "parent": None,
        "title": "Epic",
        "type": "epic",
        "priority": "p1",
        "domain": "code",
        "blocked_by": [],
        "plan_path": "plans/epic.md",
        "cwd": "",
        "created_at": "2026-01-01T00:00:00+00:00",
    }
    seed_graph(home / ".fno" / "db" / "graph.json", json.dumps({"entries": [epic]}) + "\n")


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


def test_decompose_door_help_exits_zero(tmp_path: Path) -> None:
    res = _door(tmp_path, "--help")
    assert res.returncode == 0, res.stderr
    assert "backlog decompose" in res.stdout
    assert "--groups" in res.stdout


def test_decompose_door_mints_group_children(tmp_path: Path) -> None:
    _seed_epic(tmp_path)
    res = _door(tmp_path, EPIC_ID, "--groups", GROUPS, "--json")
    assert res.returncode == 0, res.stdout + res.stderr
    payload = json.loads(res.stdout)
    assert [row["action"] for row in payload["groups"]] == ["created", "created"]
    assert payload["orphaned"] == []
    assert payload["packaging"] == "separate"


def test_decompose_door_re_run_updates_in_place(tmp_path: Path) -> None:
    _seed_epic(tmp_path)
    first = _door(tmp_path, EPIC_ID, "--groups", GROUPS, "--json")
    assert first.returncode == 0, first.stdout + first.stderr
    ids = {row["slug"]: row["id"] for row in json.loads(first.stdout)["groups"]}
    again = _door(tmp_path, EPIC_ID, "--groups", GROUPS, "--json")
    assert again.returncode == 0, again.stdout + again.stderr
    payload = json.loads(again.stdout)
    assert [row["action"] for row in payload["groups"]] == ["updated", "updated"]
    assert {row["slug"]: row["id"] for row in payload["groups"]} == ids


def test_decompose_door_refuses_a_group_cycle(tmp_path: Path) -> None:
    _seed_epic(tmp_path)
    cycle = json.dumps(
        [
            {"slug": "a", "title": "A", "blocked_by_groups": ["b"]},
            {"slug": "b", "title": "B", "blocked_by_groups": ["a"]},
        ]
    )
    res = _door(tmp_path, EPIC_ID, "--groups", cycle)
    assert res.returncode == 2, res.stdout + res.stderr
    assert "cycle" in (res.stdout + res.stderr).lower()


def test_decompose_door_refuses_cap_overflow(tmp_path: Path) -> None:
    _seed_epic(tmp_path)
    res = _door(tmp_path, EPIC_ID, "--groups", GROUPS, "--max-prs", "1")
    assert res.returncode == 1, res.stdout + res.stderr
    assert "exceed the cap" in (res.stdout + res.stderr)


def test_decompose_door_names_the_epic_when_unknown(tmp_path: Path) -> None:
    res = _door(tmp_path, "ab-00000000", "--groups", GROUPS)
    assert res.returncode == 3, res.stdout + res.stderr
    assert "not found" in (res.stdout + res.stderr)
