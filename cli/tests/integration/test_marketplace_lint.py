"""Integration tests for scripts/lint/no-cross-skill-runtime-calls.sh.

- the real repo state passes the lint
- a Skill() call, a path escape, or a missing fno requirement fails it
- the Skill() error names file:line and recommends Read or Task/Agent
"""
from __future__ import annotations

import subprocess
import textwrap
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[3]
LINT = REPO_ROOT / "scripts" / "lint" / "no-cross-skill-runtime-calls.sh"


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


def test_skill_runtime_call_fails(tmp_path):
    """A Skill() call in any *.md inside a driver skill fails the lint with
    a line-pinpoint error and a fix recommendation."""
    skill_md = _CLEAN_SKILL_FRONTMATTER.replace(
        "Body content. No forbidden patterns.",
        'Body content. Then we Skill("blueprint", "...") at runtime.',
    )
    _make_skill(tmp_path, "target", skill_md)
    result = _run(["bash", str(LINT), "--root", str(tmp_path)])
    assert result.returncode != 0
    # AC3-UI: file:line:match output for navigation
    assert "skills/target/SKILL.md:" in result.stderr
    # AC3-UI: fix recommendation
    assert "Read" in result.stderr
    assert "Task/Agent" in result.stderr


# ---------------------------------------------------------------------------
# AC2-ERR: path-escape failure
# ---------------------------------------------------------------------------


def test_shared_path_escape_fails(tmp_path):
    """A `../../_shared/X.md` link in a driver skill fails the lint."""
    skill_md = _CLEAN_SKILL_FRONTMATTER.replace(
        "Body content. No forbidden patterns.",
        "See [auto-merge](../../_shared/auto-merge.md).",
    )
    _make_skill(tmp_path, "target", skill_md)
    result = _run(["bash", str(LINT), "--root", str(tmp_path)])
    assert result.returncode != 0
    assert "cross-skill path escape" in result.stderr
    assert "bundled references/ or agents/" in result.stderr


# ---------------------------------------------------------------------------
# Missing requires.binaries.fno fails
# ---------------------------------------------------------------------------


def test_missing_fno_in_requires_fails(tmp_path):
    """A metadata.requires.binaries block that lists other binaries but not
    fno fails the lint."""
    skill_md = textwrap.dedent(
        """\
        ---
        name: target
        description: "fno missing"
        metadata:
          requires:
            binaries:
              - "gh >= 2.0"
              - "git >= 2.30"
        ---
        body
        """
    )
    _make_skill(tmp_path, "target", skill_md)
    result = _run(["bash", str(LINT), "--root", str(tmp_path)])
    assert result.returncode != 0
    assert "does not declare 'fno'" in result.stderr


# ---------------------------------------------------------------------------
# AC5-EDGE: real repo state passes the lint (regression-defense)
# ---------------------------------------------------------------------------


def test_real_repo_state_passes_lint():
    """After Phases 4/5/6, the real fno repo state must pass. If this
    test fails, a contributor regressed the marketplace-readiness invariant."""
    result = _run(["bash", str(LINT)])
    assert result.returncode == 0, result.stdout + result.stderr
