"""Integration tests for the stale-skill-reference audit gate.

These exercise the bash audit script directly. They run the real script
against a temporary worktree to avoid coupling to the main repo's current
cleanup state.
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
AUDIT_SCRIPT = REPO_ROOT / "scripts" / "ci" / "check-no-stale-skill-refs.sh"
RETIRED_SKILLS = REPO_ROOT / "scripts" / "ci" / "retired-skills.txt"


def _make_tiny_repo(tmp_path: Path) -> Path:
    """Build a minimal repo skeleton the audit script can scan."""
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    # Match the scan paths the script expects.
    for sub in ("hooks", "scripts", "agents", "skills", ".claude-plugin", "cli/src/fno"):
        (tmp_path / sub).mkdir(parents=True, exist_ok=True)
    # Copy the audit script into the same relative location so resolve_repo_root works.
    audit_dest = tmp_path / "scripts" / "ci" / "check-no-stale-skill-refs.sh"
    audit_dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(AUDIT_SCRIPT, audit_dest)
    audit_dest.chmod(0o755)
    registry_dest = tmp_path / "scripts" / "ci" / "retired-skills.txt"
    shutil.copy(RETIRED_SKILLS, registry_dest)
    subprocess.run(["git", "add", "-A"], cwd=tmp_path, check=True)
    subprocess.run(
        ["git", "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "init"],
        cwd=tmp_path,
        check=True,
    )
    return tmp_path


def _run_audit(repo: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", "scripts/ci/check-no-stale-skill-refs.sh"],
        cwd=repo,
        capture_output=True,
        text=True,
    )


def test_audit_fails_on_planted_stale_ref(tmp_path: Path) -> None:
    """Plant a hook that sources a removed skill; audit must fail."""
    repo = _make_tiny_repo(tmp_path)
    bad = repo / "hooks" / "fake-hook.sh"
    bad.write_text("#!/usr/bin/env bash\nsource skills/distill/scripts/foo.sh\n")
    result = _run_audit(repo)
    assert result.returncode == 1, result.stdout + result.stderr
    assert "distill" in result.stdout
    assert "AUDIT FAIL" in result.stdout


def test_audit_ignores_bare_word_matches(tmp_path: Path) -> None:
    """Bare-word occurrences of a skill name in prose lists do NOT trigger the audit.

    Example: the impeccable-stages baseline list contains the bare token
    `distill` as a stage name, unrelated to the removed `/distill` skill. The
    audit's pattern must discriminate against this kind of false positive.
    """
    repo = _make_tiny_repo(tmp_path)
    benign = repo / "skills" / "impeccable" / "SKILL.md"
    benign.parent.mkdir(parents=True, exist_ok=True)
    benign.write_text("Known stages: craft, polish, distill, extract, shape.\n")
    result = _run_audit(repo)
    assert result.returncode == 0, result.stdout + result.stderr


def test_audit_rejects_malformed_skill_name(tmp_path: Path) -> None:
    """Sanity check: a malformed registry name fails closed."""
    repo = _make_tiny_repo(tmp_path)
    registry = repo / "scripts" / "ci" / "retired-skills.txt"
    registry.write_text("bad.name|cut|test ruling\n")
    result = _run_audit(repo)
    assert result.returncode == 2, result.stdout + result.stderr
    assert "malformed skill name" in result.stderr


def test_audit_rejects_missing_retired_skill_registry(tmp_path: Path) -> None:
    """A missing registry must not turn the stale-reference audit green."""
    repo = _make_tiny_repo(tmp_path)
    (repo / "scripts" / "ci" / "retired-skills.txt").unlink()
    result = _run_audit(repo)
    assert result.returncode == 2, result.stdout + result.stderr
    assert "AUDIT ERROR" in result.stderr
