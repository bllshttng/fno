"""Regression census for autonomous dispatch routing ownership."""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]


def _read_with_positive_control(relative: str, marker: str) -> str:
    """Read one owned path only after an anchored ripgrep control finds it."""
    source = (ROOT / relative).read_text(encoding="utf-8")
    rg = shutil.which("rg")
    if rg is None:
        assert marker in source, f"positive control {marker!r} missing from {relative}"
        return source
    found = subprocess.run(
        [rg, "-n", "--glob", "!/target/**", marker, relative],
        cwd=ROOT,
        text=True,
        capture_output=True,
    )
    assert found.returncode == 0, (
        f"positive control {marker!r} missing from {relative}: {found.stderr}"
    )
    return source


def test_blueprint_completion_dispatches_nothing() -> None:
    blueprint = _read_with_positive_control(
        "skills/blueprint/SKILL.md",
        "fno backlog session close",
    )
    decompose = _read_with_positive_control(
        "skills/blueprint/references/epic-decomposition.md",
        "fno backlog decompose",
    )
    for text in (blueprint, decompose):
        assert "fno backlog advance" not in text
        assert "--source sob" not in text
        assert "fno agents spawn" not in text
    assert "autolaunch-on-ready" not in blueprint
