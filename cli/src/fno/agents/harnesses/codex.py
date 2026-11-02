"""fno.agents.harnesses.codex - codex exec adapter for US4.

Surface:

- :func:`create` - invoke ``codex exec --json --cd <cwd> ...``, parse the JSONL
  stream, capture session_id from the ``thread.started`` event and last
  assistant text from the ``item.completed`` (item.type=``agent_message``)
  events. Returns :class:`CodexResult`.
- :func:`resume` - invoke ``codex exec resume <session_id> --json ...`` from
  the registry-recorded cwd. Same JSONL loop; session_id is NOT re-captured.
- :func:`inject_from_name` - bracket-prefix prompt injection.
- :func:`sandbox_flag` - argv tokens for the sandbox mode (mutually exclusive
  with the ``--dangerously-bypass-...`` flag).

Locked Decision 13: the _EVENT_TYPES / _ITEM_TYPES dicts below are pinned
from a real capture by ``scripts/smoke/capture-codex-jsonl.sh`` against
codex 0.130.0. Parser code references these by key; literal string drift
in codex's vocabulary is meant to surface via the smoke integration test
(Wave 2.2), not be papered over inline.

Locked Decision 12: ``stderr=subprocess.STDOUT`` merges codex's stderr into
the stdout pipe so a single drainer handles both. Two-pipe deadlock (which
sigma-review caught on PR #299's claude logs --follow path) is avoided by
construction.
"""
from __future__ import annotations

from pathlib import Path


def _git_common_dir(cwd: Path) -> str:
    """Absolute git COMMON dir for ``cwd``; ``""`` outside a repo or on failure."""
    from fno.paths import git_common_dir

    common = git_common_dir(cwd, timeout=10)
    return str(common) if common else ""


def _git_write_roots(cwd: Path) -> list[str]:
    """The common dir, then a linked worktree's own gitdir: codex keeps that
    gitdir read-only inside the common dir. Mirror of provider.rs::git_write_roots."""
    import subprocess

    common, own = _git_common_dir(cwd), ""
    try:
        out = subprocess.run(["git", "-C", str(cwd), "rev-parse", "--absolute-git-dir"],
                             capture_output=True, text=True, timeout=10)
        own = out.stdout.strip() if out.returncode == 0 and common else ""
    except (OSError, subprocess.SubprocessError):
        pass
    linked = own and "\n" not in own and Path(own).resolve() != Path(common).resolve()
    return [common, own] if linked else [common] if common else []


def _resolve_plan_dir(cwd: Path) -> str:
    """Absolute configured plan directory for ``cwd``; ``""`` on failure."""
    try:
        from fno.paths import plans_content_dir

        return str(plans_content_dir(project_root=cwd))
    except Exception:
        return ""


def git_writable_args(cwd: Path) -> list[str]:
    """Return ``--add-dir`` git grants so a bounded worker can commit.

    codex's ``workspace-write`` policy marks ``<project_root>/.git`` read-only,
    so a sandboxed worker cannot take ``index.lock`` and every commit fails
    mid-task. This is NOT worktree-specific - a plain repo fails the same way -
    but a LINKED worktree hides it worse, because its gitdir lives at
    ``<repo>/.git/worktrees/<name>/``, outside the workspace entirely, and the
    resulting "wrong path" error has been misread as data loss.

    The grant is :func:`_git_write_roots`. ``--add-dir`` is repeatable
    and additive, so this composes with a caller's own ``--add-dir`` instead of
    clobbering it the way ``-c sandbox_workspace_write.writable_roots`` would.

    Returns ``[]`` outside a repo, or on any git failure - a grant we cannot
    resolve must never break the spawn.
    """
    return [tok for root in _git_write_roots(cwd) for tok in ("--add-dir", root)]


def plan_writable_args(cwd: Path) -> list[str]:
    """Return ``--add-dir <plan-dir>`` for a bounded worker's plan writes.

    The flag is repeatable, so this composes with the git common-dir grant and
    caller-provided roots. Resolution failure emits no malformed token; target
    initialization separately refuses a bound plan it cannot read.
    """
    plan_dir = _resolve_plan_dir(cwd)
    return ["--add-dir", plan_dir] if plan_dir else []
