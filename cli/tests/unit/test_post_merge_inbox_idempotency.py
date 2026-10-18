"""Tests for skills/ship/scripts/inbox-has-pr.sh - the idempotency guard.

The post-merge ritual must be a no-op on re-run (BDD: "idempotent re-run").
The guard keys on a `<!-- post-merge:pr-<N> -->` marker so a second run for the
same PR skips writing a duplicate inbox section. Pure file inspection, no `fno`
dependency, so it is deterministic across CLI versions.

Node: ab-6eeb20ae.
"""

from __future__ import annotations

import subprocess
from pathlib import Path


# repo_root/cli/tests/unit/this_file -> parents[3] == repo root
SCRIPT = Path(__file__).resolve().parents[3] / "skills" / "ship" / "scripts" / "inbox-has-pr.sh"


def _run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", str(SCRIPT), *args],
        capture_output=True,
        text=True,
    )


def test_missing_inbox_file_exits_1(tmp_path: Path) -> None:
    """Inbox file does not exist yet -> safe to write (exit 1), no crash."""
    assert _run(str(tmp_path / "nope.md"), "123").returncode == 1


def test_missing_args_exits_2() -> None:
    """Usage error when args are missing."""
    assert _run().returncode == 2
    assert _run("only-one-arg").returncode == 2


def test_marker_does_not_false_match_other_pr(tmp_path: Path) -> None:
    """PR 12 must not match a marker for PR 123 (no substring false positive)."""
    inbox = tmp_path / "inbox.md"
    inbox.write_text("<!-- post-merge:pr-123 -->\n", encoding="utf-8")
    assert _run(str(inbox), "12").returncode == 1
    assert _run(str(inbox), "123").returncode == 0
