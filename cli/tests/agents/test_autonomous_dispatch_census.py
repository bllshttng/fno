"""Regression census for autonomous dispatch routing ownership."""
from __future__ import annotations

import re
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


def test_positive_control_falls_back_without_ripgrep(monkeypatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda name: None)
    source = _read_with_positive_control(
        "skills/target/scripts/dispatch-node.sh",
        "dispatch-node.sh",
    )
    assert "fno agents spawn --node" in source




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


def test_dispatch_harness_registry_entry_carries_its_migration() -> None:
    """The registry description of the deprecated key must teach the migration
    itself, matching the sibling `dispatch.auto_merge` entry's shape (AC8)."""
    registry = _read_with_positive_control(
        "cli/src/fno/config/registry.py",
        '"dispatch.auto_merge"',
    )
    entry = next(
        line for line in registry.splitlines() if '"dispatch.harness"' in line
    )
    assert "DEPRECATED" in entry
    assert "fno config set agents.profiles.target.provider" in entry


def test_context_think_legacy_substrate_is_compatibility_only() -> None:
    config = _read_with_positive_control(
        "cli/src/fno/config/__init__.py",
        "class ThinkSpawnBlock",
    )
    registry = _read_with_positive_control(
        "cli/src/fno/config/registry.py",
        "think_spawn.enabled",
    )
    guide = _read_with_positive_control(
        "docs/configuration-guide.md",
        "think_spawn.enabled",
    )

    block = re.search(
        r"class ThinkSpawnBlock\(BaseModel\):(.*?)\nclass ",
        config,
        re.DOTALL,
    )
    assert block is not None
    assert "substrate: Optional[str] = None" in block.group(1)
    assert '"think_spawn.substrate"' in registry
    assert "deprecated compatibility fallback" in registry.lower()
    assert "deprecated compatibility fallback" in guide.lower()
