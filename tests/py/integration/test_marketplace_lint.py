"""Integration tests for scripts/lint/check-skill-reach.sh.

- the real repo state passes the lint
- a missing fno requirement fails it
- a toolbag that omits a skill fails it and names the skill
"""
from __future__ import annotations

import subprocess
import textwrap
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[3]
LINT = REPO_ROOT / "scripts" / "lint" / "check-skill-reach.sh"


_CLEAN_SKILL_FRONTMATTER = textwrap.dedent(
    """\
    ---
    name: target
    description: "clean fixture"
    metadata:
      requires:
        binaries:
          - "fno >= 0.1"
    ---
    Body content. No forbidden patterns.
    """
)


def _run(cmd: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, cwd=cwd, check=False)


def _shelled_python_has_yaml() -> bool:
    """Does the bare ``python3`` this lint shells out to import yaml?

    The lint runs under the system interpreter, not the project venv, and its
    frontmatter parse requires PyYAML. CI's python3 carries it, so guarding
    here keeps a dev machine whose system interpreter lacks it from reading as
    a lint failure. Mirrors the same guard in test_skill_bundles.py.
    """
    return _run(["python3", "-c", "import yaml"]).returncode == 0


pytestmark = pytest.mark.skipif(
    not _shelled_python_has_yaml(),
    reason="system python3 lacks PyYAML, required by the marketplace lint (pip install pyyaml)",
)


def _make_skill(tmp_root: Path, name: str, skill_md: str) -> Path:
    skill_dir = tmp_root / "skills" / name
    skill_dir.mkdir(parents=True, exist_ok=True)
    (skill_dir / "SKILL.md").write_text(skill_md)
    return skill_dir


# ---------------------------------------------------------------------------
# AC2-ERR + AC3-UI: Skill() runtime call fails
# ---------------------------------------------------------------------------


def test_toolbag_omitting_a_skill_fails(tmp_path):
    """A skill dir the toolbag never names as fno:<name> fails the lint, and a
    Skill() call in target's text no longer does."""
    skill_md = _CLEAN_SKILL_FRONTMATTER.replace(
        "Body content. No forbidden patterns.",
        'Body content. Then we Skill("fno:fix") at runtime.',
    )
    target = _make_skill(tmp_path, "target", skill_md)
    _make_skill(tmp_path, "fix", "---\nname: fix\n---\nbody\n")
    _make_skill(tmp_path, "tdd", "---\nname: tdd\n---\nbody\n")
    (target / "references").mkdir()
    (target / "references" / "toolbag.md").write_text("| `fno:fix` | red | green |\n")
    result = _run(["bash", str(LINT), "--root", str(tmp_path)])
    assert result.returncode != 0
    assert "does not name fno:tdd" in result.stderr
    assert "does not name fno:fix" not in result.stderr
    assert "Skill(" not in result.stderr


# ---------------------------------------------------------------------------
# AC5-EDGE: real repo state passes the lint (regression-defense)
# ---------------------------------------------------------------------------


def test_real_repo_state_passes_lint():
    """After Phases 4/5/6, the real fno repo state must pass. If this
    test fails, a contributor regressed the marketplace-readiness invariant."""
    result = _run(["bash", str(LINT)])
    assert result.returncode == 0, result.stdout + result.stderr
