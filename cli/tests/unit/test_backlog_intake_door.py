"""The native `fno backlog intake` door.

The Python intake verb moved into the Rust dispatcher; these tests exercise
the door the same way an operator does: one binary invocation per case, the
fixture root pinned (cwd, HOME, state dir) so no subprocess write escapes to
the operator state (the state-canary lesson from the capture port).
"""
from __future__ import annotations

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


def test_intake_door_mints_a_node(tmp_path: Path) -> None:
    plan = tmp_path / "fake-plan.md"
    plan.write_text(PLAN)
    res = _intake(tmp_path, str(plan))
    assert res.returncode == 0, res.stderr
    # The receipt names whatever id prefix this environment's settings mint;
    # the shape is the contract, not the prefix.
    receipt = next(
        (ln for ln in res.stdout.splitlines() if ln.startswith("intake ")), ""
    )
    assert '-> backlog: "My Plan"' in receipt, res.stdout
    minted = receipt.split()[1]
    assert minted, res.stdout

    # Read back through the same door, not the store file: the door is the
    # user path and the store format is not this test's contract.
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


def test_intake_door_refuses_surfaceless_plan(tmp_path: Path) -> None:
    plan = tmp_path / "bare.md"
    plan.write_text("---\ncreated: 2026-05-05\ntitle: Bare\n---\n# Bare\n")
    res = _intake(tmp_path, str(plan))
    assert res.returncode == 2, res.stdout
    assert "no comparable file surface" in res.stdout + res.stderr
    allowed = _intake(tmp_path, str(plan), "--allow-no-surface")
    assert allowed.returncode == 0, allowed.stderr


def test_intake_door_refuses_an_unknown_claim(tmp_path: Path) -> None:
    plan = tmp_path / "claim-plan.md"
    plan.write_text(PLAN)
    res = _intake(tmp_path, str(plan), "--claims", "x-0000dead")
    assert res.returncode == 1, res.stdout
    assert "not found on graph" in res.stdout + res.stderr
