"""All three removal call sites must answer one question the same way (AC2-HP).

Three independent implementations used to ask "is `git status --porcelain`
empty" and each blocked on any output:

    scripts/lib/worktree-lifecycle.sh   the `--merged` sweep
    scripts/setup/archive-worktree.sh   the archive strict-check
    crates/fno-agents/src/daemon.rs     the row-GC cleanliness probe

`AGENTS.md` records that N implementations of one operation is a defect class:
a guard placed on one of N reachable paths is decorative. Converging them on
one classifier only helps while they STAY converged, so this test drives the
real bash entry points over a fixture corpus. The gate's own corpus runs in
the cargo suite.
"""
import os
import subprocess
import time
from pathlib import Path

import pytest

from fno.rust_binary import resolve_binary


def _gate_binary():
    """THIS checkout's build first: a deployed fno-agents on PATH may predate
    the verb and answer nothing, which would read as a code defect."""
    for cand in (
        REPO_ROOT / "crates" / "fno-agents" / "target" / "release" / "fno-agents",
        REPO_ROOT / "target" / "release" / "fno-agents",
        REPO_ROOT / "crates" / "fno-agents" / "target" / "debug" / "fno-agents",
        REPO_ROOT / "target" / "debug" / "fno-agents",
    ):
        # Executability, not existence: the changed lane's scrub leaves
        # non-executable files behind, and a pinned non-executable path makes
        # every probe read as probe-failed rather than as "nothing to pin".
        if cand.is_file() and os.access(cand, os.X_OK):
            return cand
    found = resolve_binary()
    if found is None or not os.access(found, os.X_OK):
        pytest.skip(
            "no fno-agents binary resolves; the gate corpus runs in the cargo "
            "suite, this lane only pins bash/binary agreement"
        )
    return found

REPO_ROOT = Path(__file__).resolve().parents[3]
REAPABLE_LIB = REPO_ROOT / "scripts" / "lib" / "worktree-reapable.sh"


def _git(cwd: Path, *args: str) -> None:
    r = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    assert r.returncode == 0, f"git {' '.join(args)} failed: {r.stderr}"


def _make_repo(root: Path) -> Path:
    """A real LINKED worktree, not a standalone repo.

    archive-worktree.sh refuses a canonical checkout (exit 1) before it reaches
    any strict check, and a standalone `git init` repo IS its own canonical. A
    fixture built that way never exercises the predicate under test and reports
    a cheerful pass, so the corpus builds a parent repo and hands back a linked
    worktree of it - the shape every real caller passes.
    """
    parent = root.parent / f"{root.name}-origin"
    parent.mkdir(parents=True)
    _git(parent, "init", "-q", "-b", "main")
    _git(parent, "config", "user.email", "t@example.com")
    _git(parent, "config", "user.name", "t")
    (parent / "a.py").write_text("a = 1\n")
    (parent / "b.py").write_text("b = 2\n")
    _git(parent, "add", "-A")
    _git(parent, "commit", "-qm", "seed")
    _git(parent, "worktree", "add", "-q", str(root), "-b", "feature/fixture")
    # Backdate past the setup window: every corpus row but `unborn` describes
    # an ESTABLISHED tree, and a fresh linked worktree is refused by the
    # unborn gate before its dirt is ever classified.
    old = time.time() - 7200
    os.utime(root / ".git", (old, old))
    return root


def _one_modification(p: Path) -> None:
    (p / "a.py").write_text("a = 999\n")


def _untracked(p: Path) -> None:
    (p / "scratch.py").write_text("nope\n")


CORPUS = [
    ("one-modification", _one_modification, False),
    ("untracked", _untracked, False),
]


def _gate_env() -> dict:
    """The child env with the gate pinned to the binary this module tested.

    The changed lane scrubs target/ mid-job, so a child that re-resolves
    through $PATH or a stale target path answers nothing; the lib then
    synthesizes probe-failed and every not-blocked row flips. With no LIVE
    binary at all there is nothing to pin, and inheriting the parent env
    would hand the script a dead FNO_AGENTS_BIN to probe-fail on, so the
    test skips.
    """
    binary = _gate_binary()
    if binary is None:
        pytest.skip("no live fno-agents binary: bash/binary agreement is unpinnable")
    return dict(os.environ, FNO_AGENTS_BIN=str(binary))


def _archive_script_verdict(path: Path) -> bool:
    """Run archive-worktree.sh far enough to see its dirty verdict.

    Exit 2 is its "strict check failed" code. The script's own docs pin that,
    and the dirty check is the first strict check it runs, so a non-2 exit
    means the tree did not block removal.
    """
    script = REPO_ROOT / "scripts" / "setup" / "archive-worktree.sh"
    r = subprocess.run(
        ["bash", str(script), str(path), "--yes"],
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
        env=_gate_env(),
    )
    # POSITIVE CONTROL. A "not blocked" verdict must mean the script reached and
    # passed the check, never that it bailed earlier for an unrelated reason.
    # The fixture bug this catches was real: a standalone repo made the script
    # exit 1 with "refusing to archive canonical checkout", which read as
    # "not blocked" and passed every clean case without testing anything.
    assert "refusing to archive canonical checkout" not in r.stderr, (
        f"fixture is not a linked worktree; the predicate never ran: {r.stderr}"
    )
    assert r.returncode in (0, 2), (
        f"script failed for an unrelated reason (rc={r.returncode}): {r.stderr}"
    )
    blocked_on_dirt = r.returncode == 2 and (
        "reapable=no" in r.stderr or "dirty working tree" in r.stderr
    )
    return not blocked_on_dirt


@pytest.mark.parametrize("name,mutate,expected", CORPUS, ids=[c[0] for c in CORPUS])
def test_archive_script_agrees(
    tmp_path: Path, name: str, mutate, expected: bool, native_backlog_door
) -> None:
    repo = _make_repo(tmp_path / name)
    mutate(repo)

    actual = _archive_script_verdict(repo)
    assert actual == expected, (
        f"{name}: archive verdict {actual} != corpus {expected}; "
        f"FNO_AGENTS_BIN={os.environ.get('FNO_AGENTS_BIN', '<unset>')}"
    )


def test_deletions_only_worktree_is_actually_removed(tmp_path: Path, native_backlog_door) -> None:
    """Clearing the predicate is not enough: the removal must SUCCEED.

    `git worktree remove` counts a tracked file missing from disk as "modified"
    and refuses with exit 4. So a worktree our classifier affirmatively cleared
    still failed to go, and the whole change was inert on exactly the 17
    worktrees it targets. The end-to-end assertion is that the directory is
    gone, not that a check passed.
    """
    repo = _make_repo(tmp_path / "gone")
    (repo / "a.py").unlink()
    (repo / "b.py").unlink()

    script = REPO_ROOT / "scripts" / "setup" / "archive-worktree.sh"
    r = subprocess.run(
        ["bash", str(script), str(repo), "--yes"],
        capture_output=True, text=True, cwd=str(REPO_ROOT), timeout=60,
    )

    assert r.returncode == 0, (
        "archive failed: "
        f"{r.stderr}\nFNO_AGENTS_BIN={os.environ.get('FNO_AGENTS_BIN', '<unset>')}"
    )
    assert not repo.exists(), "predicate passed but the worktree is still on disk"


def test_helper_fails_closed_when_the_verb_cannot_answer(tmp_path: Path, native_backlog_door) -> None:
    """A gate that cannot answer must never read as permission.

    An absence of "no" is not a yes. This drives the helper with a binary
    override that exits non-zero with no receipt and a PATH stripped of both
    `fno-agents` and `fno` - the shape a partial deploy produces.
    """
    repo = _make_repo(tmp_path / "stale")
    fake = tmp_path / "broken-gate"
    fake.write_text("#!/bin/sh\nexit 2\n")
    fake.chmod(0o755)

    script = (
        f'source "{REAPABLE_LIB}"\n'
        f'export FNO_AGENTS_BIN="{fake}"\n'
        f'if wt_reapable "{repo}"; then echo YES; else echo NO; fi\n'
        f'echo "$WT_REAPABLE_LINE"\n'
    )
    # STRIP THE RESCUERS FOR REAL. The override is accepted (it is executable),
    # so the repo's own build is not consulted; stripping PATH removes the
    # installed CLI fallback. Every lane answers nothing, and the helper must
    # degrade to its fail-closed receipt.
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True,
                       cwd=str(REPO_ROOT), env={"PATH": "/usr/bin:/bin"})

    assert "NO" in r.stdout
    assert "probe-failed" in r.stdout
