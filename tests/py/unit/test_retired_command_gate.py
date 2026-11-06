"""The retired-command gate's verdicts, exercised on a synthetic tree.

The gate decides whether a caller-facing string shows a ruling-retired
command in RUNNABLE form. Its verdicts are cheap to state and easy to get
wrong, so each one gets a case here against a repo built for the purpose
rather than against the real tree, whose contents move under every sweep.

The negative cases matter as much as the positive ones. A gate never observed
failing has not been tested, and all three of this gate's controls exist to refuse
a pass that only means "the instrument never matched anything".
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[3]
GATE = REPO_ROOT / "scripts" / "ci" / "check-retired-command-strings.sh"
REGISTRY = REPO_ROOT / "scripts" / "ci" / "retired-commands.txt"
CANARY = REPO_ROOT / "scripts" / "ci" / "fixtures" / "retired-command-canary.sh"
CONFIG_REGISTRY = REPO_ROOT / "scripts" / "ci" / "retired-config-leaves.txt"
CONFIG_CANARY = REPO_ROOT / "scripts" / "ci" / "fixtures" / "retired-config-leaf-canary.py"


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    """A minimal tree the gate can scan, carrying the real gate and registry."""
    for sub in (
        "scripts/ci/fixtures", "crates/fno-agents/src",
        "skills/k", "docs", "hooks", "agents", "commands",
    ):
        (tmp_path / sub).mkdir(parents=True, exist_ok=True)
    shutil.copy(GATE, tmp_path / "scripts/ci/check-retired-command-strings.sh")
    shutil.copy(REGISTRY, tmp_path / "scripts/ci/retired-commands.txt")
    shutil.copy(CANARY, tmp_path / "scripts/ci/fixtures/retired-command-canary.sh")
    shutil.copy(CONFIG_REGISTRY, tmp_path / "scripts/ci/retired-config-leaves.txt")
    shutil.copy(CONFIG_CANARY, tmp_path / "scripts/ci/fixtures/retired-config-leaf-canary.py")
    (tmp_path / "scripts/ci/retired-ok-paths.txt").write_text("# none\n")

    # Every surface must hold at least one tracked file of its extension, or
    # the per-surface control fires before any verdict is reached.
    (tmp_path / "crates/fno-agents/src/a.rs").write_text("fn a() {}\n")
    (tmp_path / "skills/k/a.md").write_text("prose\n")
    (tmp_path / "docs/a.md").write_text("prose\n")
    (tmp_path / "hooks/a.sh").write_text("true\n")
    (tmp_path / "agents/a.md").write_text("prose\n")
    (tmp_path / "commands/a.md").write_text("prose\n")

    _git(tmp_path, "init", "-q", "-b", "main")
    _git(tmp_path, "add", "-A")
    _git(tmp_path, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "init")
    return tmp_path


def _run(repo: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", "scripts/ci/check-retired-command-strings.sh"],
        cwd=repo,
        capture_output=True,
        text=True,
    )


def _add(repo: Path, rel: str, body: str) -> None:
    (repo / rel).write_text(body)
    _git(repo, "add", "-A")


def test_failure_message_names_command_replacement_and_ruling(repo: Path) -> None:
    _add(repo, "crates/fno-agents/src/new.rs", 'const HELP: &str = "Clean up with claude rm <short_id>.";\n')
    result = _run(repo)
    assert result.returncode == 1
    assert "crates/fno-agents/src/new.rs" in result.stderr
    assert "claude rm" in result.stderr
    assert "fno agents rm" in result.stderr
    assert "d-1900e419" in result.stderr
    # The escape is named at the refusal, not left to a doc page: a gate whose
    # way out is undocumented teaches the reader to override it.
    assert "retired-ok:" in result.stderr


def test_losing_the_canary_fixture_is_refused_as_vacuous(repo: Path) -> None:
    """The target control asserts on the FIXTURE, not on production text.

    That is deliberate. A real cleanup may legitimately take the tree to zero
    marked sites, and a control keyed on production text cannot tell that from
    a marker spelling that drifted - it would answer a successful cleanup with
    a permanent red whose only exits are to re-add a forbidden string or to
    delete the gate.
    """
    (repo / "scripts/ci/fixtures/retired-command-canary.sh").unlink()
    _git(repo, "add", "-A")
    result = _run(repo)
    assert result.returncode == 1
    assert "vacuous" in result.stderr
