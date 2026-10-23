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
        "scripts/ci/fixtures", "cli/src/fno", "crates/fno-agents/src",
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
    (tmp_path / "cli/src/fno/a.py").write_text("x = 1\n")
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


def _register_leaf(repo: Path, leaf: str) -> None:
    """Register one leaf AND aim the canary at it.

    The gate refuses a scan whose canary names a leaf nobody registered, so a
    test that writes only the registry inherits whichever leaf the repo happens
    to carry. That coupling broke the moment the repo registered a different
    one. Both files move together here.
    """
    (repo / "scripts/ci/retired-config-leaves.txt").write_text(
        f"{leaf}|deleted|use one-shot review evidence\n"
    )
    (repo / "scripts/ci/fixtures/retired-config-leaf-canary.py").write_text(
        f'CONFIG_LEAF_CANARY = "{leaf}"  # retired-config-ok: scanner control only\n'
    )


def test_retired_config_key_in_caller_surface_fails(repo: Path) -> None:
    _register_leaf(repo, "review.github_apps")
    _add(repo, "cli/src/fno/config.py", 'HELP = "set review.github_apps"\n')
    result = _run(repo)
    assert result.returncode == 1
    assert "review.github_apps" in result.stderr


def test_missing_config_canary_fails_closed(repo: Path) -> None:
    (repo / "scripts/ci/fixtures/retired-config-leaf-canary.py").unlink()
    _git(repo, "add", "-A")
    result = _run(repo)
    assert result.returncode == 1
    assert "config canary not found" in result.stderr


def test_failure_message_names_command_replacement_and_ruling(repo: Path) -> None:
    _add(repo, "cli/src/fno/new.py", 'HELP = "Clean up with claude rm <short_id>."\n')
    result = _run(repo)
    assert result.returncode == 1
    assert "cli/src/fno/new.py" in result.stderr
    assert "claude rm" in result.stderr
    assert "fno agents rm" in result.stderr
    assert "d-1900e419" in result.stderr
    # The escape is named at the refusal, not left to a doc page: a gate whose
    # way out is undocumented teaches the reader to override it.
    assert "retired-ok:" in result.stderr


def test_marker_on_the_same_line_clears_the_site(repo: Path) -> None:
    _add(
        repo,
        "cli/src/fno/new.py",
        'MSG = "claude rm <short_id> exited 1"  # retired-ok: reports a failed shellout.\n',
    )
    assert _run(repo).returncode == 0


def test_malformed_registry_line_names_its_line_number(repo: Path) -> None:
    """A silently skipped entry is a retired command the gate stops seeking."""
    reg = repo / "scripts/ci/retired-commands.txt"
    reg.write_text("# header\nclaude rm|missing the ruling field\n")
    _git(repo, "add", "-A")
    result = _run(repo)
    assert result.returncode == 1
    assert ":2:" in result.stderr and "malformed" in result.stderr


def test_a_surface_that_resolves_to_nothing_fails(repo: Path) -> None:
    """The control that the first draft of this gate lacked.

    A pathspec matching zero files fails silently, and a whole-scan control
    cannot see it while another surface still returns hits.
    """
    shutil.rmtree(repo / "crates")
    _git(repo, "add", "-A")
    result = _run(repo)
    assert result.returncode == 1
    assert "surface crates holds no tracked .rs files" in result.stderr


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


def test_a_stale_allowlist_entry_fails(repo: Path) -> None:
    """An exemption for a path that no longer exists silently widens the gate."""
    (repo / "scripts/ci/retired-ok-paths.txt").write_text(
        "docs/deleted.md|it moved and nobody updated this line\n"
    )
    _git(repo, "add", "-A")
    result = _run(repo)
    assert result.returncode == 1
    assert "does not exist" in result.stderr


def test_a_malformed_allowlist_line_names_its_line_number(repo: Path) -> None:
    (repo / "scripts/ci/retired-ok-paths.txt").write_text(
        "# header\ndocs/budgeted.md\n"
    )
    _git(repo, "add", "-A")
    result = _run(repo)
    assert result.returncode == 1
    assert ":2:" in result.stderr and "malformed" in result.stderr


def test_a_regex_registry_entry_still_fails_through_the_prefilter(repo: Path) -> None:
    """An entry written as a regex reaches git grep -F by its literal prefix."""
    _add(repo, "cli/src/fno/new.py", 'HELP = "fno backlog update <id> --title <text>"\n')
    result = _run(repo)
    assert result.returncode == 1
    assert "cli/src/fno/new.py" in result.stderr


def test_the_scan_stays_inside_a_runtime_bound(repo: Path) -> None:
    """The 153-way ERE alternation ran 31+ minutes on the real tree.

    The synthetic tree is tiny, so this bounds the shape and not the tree. The
    bound is deliberately loose; it catches a hang, not a slowdown.
    """
    result = subprocess.run(
        ["bash", "scripts/ci/check-retired-command-strings.sh"],
        cwd=repo,
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 0, result.stderr
