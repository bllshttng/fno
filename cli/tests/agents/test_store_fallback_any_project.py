"""Harness-store healing resolves from any project.

The project confinement retired 2026-10-25 (ruling d-460ea8c7) refused a hit
whose cwd sat outside the caller's project and taught callers to retry with
the retired --cross-project flag. The heal is machine-wide now: a full session
id, a short id, or a name resolves from any project, and the one guard left is
uniqueness (more than one store hit refuses with the candidate list).

These tests pin the machine-wide behavior on the in-process healer AND on the
exec'd ``fno agents heal-token`` binary, so a future confinement cannot return
without a test failing on both.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from fno.agents.store_fallback import StoreHit, heal_from_harness_store


def _git_repo(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q"], cwd=path, check=True)
    subprocess.run(["git", "config", "user.email", "t@t.t"], cwd=path, check=True)
    subprocess.run(["git", "config", "user.name", "t"], cwd=path, check=True)
    (path / "x").write_text("x")
    subprocess.run(["git", "add", "-A"], cwd=path, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "i"], cwd=path, check=True)
    return path


def _git_worktree(main: Path, dest: Path) -> Path:
    subprocess.run(
        ["git", "-C", str(main), "worktree", "add", "-q", str(dest)],
        check=True,
    )
    return dest


@pytest.fixture
def two_repos(tmp_path):
    """A main repo, a worktree of it, and an unrelated foreign repo."""
    main = _git_repo(tmp_path / "footnote")
    worktree = _git_worktree(main, tmp_path / "wt")
    foreign = _git_repo(tmp_path / "regready")
    return main, worktree, foreign


# ---------------------------------------------------------------------------
# heal_from_harness_store: a foreign-repo hit heals with no flag
# ---------------------------------------------------------------------------


def _stub_hits(monkeypatch, hits):
    monkeypatch.setattr(
        "fno.agents.store_fallback.complete_store_hits",
        lambda token, **kw: list(hits),
    )


def test_heal_adopts_a_foreign_hit_without_a_flag(two_repos, monkeypatch, tmp_path):
    """The retired confinement refused exactly this shape behind --cross-project."""
    main, _, foreign = two_repos
    sid = "ab12cdef-0000-0000-0000-000000000001"
    _stub_hits(monkeypatch, [StoreHit("claude", sid, str(foreign))])
    entry = heal_from_harness_store(
        "ab12cdef",
        registry_path=tmp_path / "registry.json",
    )
    assert entry is not None
    assert entry.harness_session_id == sid


# ---------------------------------------------------------------------------
# Binary: the exec'd `fno agents heal-token` resolves a foreign session
# ---------------------------------------------------------------------------


def _seed_claude_transcript(projects_dir: Path, session_id: str, cwd: Path) -> None:
    """Drop a claude-shaped transcript whose recorded cwd is ``cwd``."""
    slot = projects_dir / "footnote-test-project"
    slot.mkdir(parents=True, exist_ok=True)
    (slot / f"{session_id}.jsonl").write_text(
        json.dumps({"cwd": str(cwd)}) + "\n"
    )


def test_binary_heal_token_resolves_a_foreign_session(two_repos, tmp_path):
    """From the footnote scope AND the foreign repo, the same foreign-repo
    session resolves (exit 0). The retired binary refused the first shape with
    exit 3 and a --cross-project remedy."""
    main, _worktree, foreign = two_repos
    sid = "ab12cdef-0000-0000-0000-000000000009"
    projects = tmp_path / "projects"
    _seed_claude_transcript(projects, sid, foreign)

    env = {
        **os.environ,
        # Point the probe at the seeded transcript; isolate the registry.
        "FNO_CLAUDE_PROJECTS_DIR": str(projects),
        "FNO_STATE_DIR": str(tmp_path / "fno-state"),
        # Keep the host's real sessions/codex/opencode stores out of the probe.
        "FNO_CLAUDE_SESSIONS_DIR": str(tmp_path / "sessions-empty"),
        "FNO_CODEX_SESSIONS_DIR": str(tmp_path / "codex-empty"),
    }
    for k in ("FNO_AGENTS_HOME",):
        env.pop(k, None)
    (tmp_path / "fno-state").mkdir(parents=True, exist_ok=True)
    (tmp_path / "sessions-empty").mkdir(exist_ok=True)
    (tmp_path / "codex-empty").mkdir(exist_ok=True)

    cli_dir = Path(__file__).resolve().parents[2]  # .../cli (the uv project)

    def _run(cwd: Path):
        # fno-py is the Python CLI entrypoint (imports the editable source); the
        # Rust `fno` mux dispatches to a separately-deployed fno-py, so it would
        # test the deployed copy, not this worktree's fix.
        return subprocess.run(
            ["uv", "run", "--project", str(cli_dir), "fno-py",
             "agents", "heal-token", "ab12cdef"],
            cwd=str(cwd), env=env, capture_output=True, text=True,
        )

    # From the footnote scope: the foreign-repo session resolves (exit 0).
    out_main = _run(main)
    assert out_main.returncode == 0, out_main.stderr + out_main.stdout
    row = json.loads(out_main.stdout.strip().splitlines()[-1])
    assert row["harness_session_id"] == sid

    # From the foreign repo itself: the same session resolves too.
    out_foreign = _run(foreign)
    assert out_foreign.returncode == 0, out_foreign.stderr + out_foreign.stdout
