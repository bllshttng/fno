"""The native `fno backlog intake` door.

The Python intake verb moved into the Rust dispatcher; these tests exercise
the door the same way an operator does: one binary invocation per case, the
fixture root pinned (cwd, HOME, state dir) so no subprocess write escapes to
the operator state (the state-canary lesson from the capture port).
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

PLAN = (
    "---\n"
    "created: 2026-05-05\n"
    "title: My Plan\n"
    "---\n"
    "# My Plan\n\n"
    "## Files to Modify\n\n"
    "| File | Action |\n"
    "|---|---|\n"
    "| `cli/src/fno/example.py` | modify |\n"
)


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    yield


def _intake(tmp_path: Path, *args: str):
    """The native intake door pinned to the fixture root (cwd, HOME, state)."""
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (the smoke shard deletes it on purpose)")
    env = dict(os.environ)
    env["HOME"] = str(tmp_path)
    env["FNO_STATE_DIR"] = str(tmp_path / ".fno")
    return subprocess.run(
        [str(binary), "backlog", "intake", *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=tmp_path,
        timeout=60,
    )


def _add(tmp_path: Path, title: str) -> dict:
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (the smoke shard deletes it on purpose)")
    env = dict(os.environ)
    env["HOME"] = str(tmp_path)
    env["FNO_STATE_DIR"] = str(tmp_path / ".fno")
    proc = subprocess.run(
        [str(binary), "backlog", "add", title, "--difficulty", "low"],
        capture_output=True,
        text=True,
        env=env,
        cwd=tmp_path,
        timeout=60,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def test_intake_door_help_exits_zero(tmp_path: Path) -> None:
    res = _intake(tmp_path, "--help")
    assert res.returncode == 0, res.stderr
    assert "backlog intake" in res.stdout
    assert "--claims" in res.stdout and "--dry-run" in res.stdout


def test_intake_door_mints_a_node(tmp_path: Path) -> None:
    plan = tmp_path / "fake-plan.md"
    plan.write_text(PLAN)
    res = _intake(tmp_path, str(plan))
    assert res.returncode == 0, res.stderr
    assert 'intake x' in res.stdout and '-> backlog: "My Plan"' in res.stdout

    # Read back through the same door, not the store file: the door is the
    # user path and the store format is not this test's contract.
    minted = res.stdout.split()[1]
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    env = dict(os.environ)
    env["HOME"] = str(tmp_path)
    env["FNO_STATE_DIR"] = str(tmp_path / ".fno")
    got = subprocess.run(
        [str(binary), "backlog", "get", minted],
        capture_output=True,
        text=True,
        env=env,
        cwd=tmp_path,
        timeout=60,
    )
    assert got.returncode == 0, got.stderr
    assert "My Plan" in got.stdout


def test_intake_door_is_idempotent_on_the_same_plan(tmp_path: Path) -> None:
    plan = tmp_path / "fake-plan.md"
    plan.write_text(PLAN)
    first = _intake(tmp_path, str(plan))
    assert first.returncode == 0, first.stderr
    again = _intake(tmp_path, str(plan))
    assert again.returncode == 0, again.stderr
    assert "already intaked:" in again.stdout


def test_intake_door_refuses_surfaceless_plan(tmp_path: Path) -> None:
    plan = tmp_path / "bare.md"
    plan.write_text("---\ncreated: 2026-05-05\ntitle: Bare\n---\n# Bare\n")
    res = _intake(tmp_path, str(plan))
    assert res.returncode == 2, res.stdout
    assert "no comparable file surface" in res.stdout + res.stderr
    allowed = _intake(tmp_path, str(plan), "--allow-no-surface")
    assert allowed.returncode == 0, allowed.stderr


def test_intake_door_refuses_unparseable_frontmatter(tmp_path: Path) -> None:
    plan = tmp_path / "broken.md"
    plan.write_text(
        "---\ncreated: [unclosed\n---\n# Broken\n\n"
        "## Files to Modify\n\n| File | Action |\n|---|---|\n| `a.py` | modify |\n"
    )
    res = _intake(tmp_path, str(plan))
    assert res.returncode == 1, res.stdout
    assert "cannot parse frontmatter" in res.stdout + res.stderr


def test_intake_door_dry_run_writes_nothing(tmp_path: Path) -> None:
    plan = tmp_path / "fake-plan.md"
    plan.write_text(PLAN)
    res = _intake(tmp_path, str(plan), "--dry-run")
    assert res.returncode == 0, res.stderr
    assert 'would intake: "My Plan"' in res.stdout
    # Nothing landed: the door's own read (backlog find) comes up empty.
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    env = dict(os.environ)
    env["HOME"] = str(tmp_path)
    env["FNO_STATE_DIR"] = str(tmp_path / ".fno")
    got = subprocess.run(
        [str(binary), "backlog", "find", "My Plan"],
        capture_output=True,
        text=True,
        env=env,
        cwd=tmp_path,
        timeout=60,
    )
    assert got.returncode != 0 or "My Plan" not in got.stdout


def test_intake_door_claims_an_existing_node(tmp_path: Path) -> None:
    born = _add(tmp_path, "Claim Me")
    plan = tmp_path / "claim-plan.md"
    plan.write_text(PLAN.replace("My Plan", "Claim Me"))
    res = _intake(tmp_path, str(plan), "--claims", born["id"])
    assert res.returncode == 0, res.stderr
    assert f"linked plan to {born['id']} via cli" in res.stdout


def test_intake_door_refuses_an_unknown_claim(tmp_path: Path) -> None:
    plan = tmp_path / "claim-plan.md"
    plan.write_text(PLAN)
    res = _intake(tmp_path, str(plan), "--claims", "x-0000dead")
    assert res.returncode == 1, res.stdout
    assert "not found on graph" in res.stdout + res.stderr
