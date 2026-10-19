"""Python dispatch-workdir precedence after the default inversion.

``_resolve_dispatch_workdir`` mirrors the Rust client's ``effective_worker_cwd``
precedence: ``--cwd`` > ``--here`` (caller) > default canonical. A spawn with NO
explicit cwd source lands on the canonical (main) checkout so the identical
command behaves the same regardless of where the launcher stands;
``--here``/``--in-place`` is the explicit opt-in to keep the caller's cwd;
``--fresh`` survives as an accepted no-op alias.

Canonical resolution is driven through ``FNO_REPO_ROOT`` (the documented test
hook on ``resolve_canonical_repo_root``) so these stay git-fixture-free; the real
git-worktree resolution is proven on the Rust side
(``canonical_repo_root_resolves_main_from_linked_worktree``) and by
``test_resolve_canonical_worktree.py``.
"""
from __future__ import annotations

import os
from pathlib import Path

import pytest

from fno.agents.cli import _resolve_dispatch_workdir


@pytest.fixture(autouse=True)
def _clear_repo_root_env(monkeypatch):
    # Each test sets FNO_REPO_ROOT explicitly; start clean so a developer's
    # ambient env never leaks the canonical root into the assertions.
    monkeypatch.delenv("FNO_REPO_ROOT", raising=False)
    yield


# (explicit, here, fresh) -> expected root, and whether the canonical-redirect
# note fires. The Rust `effective_worker_cwd` unit tests assert the same rows.
_PRECEDENCE_TABLE = [
    (None, False, True, "/canonical", True),  # default canonical; --fresh is a no-op alias
    (None, True, False, "/worktree", False),  # --here keeps the caller, no note
    ("", False, False, "/canonical", True),  # an empty --cwd is absent, never Path("")
    ("/explicit", True, True, "/explicit", False),  # --cwd beats every flag
]


@pytest.mark.parametrize("explicit,here,fresh,expected,note", _PRECEDENCE_TABLE)
def test_dispatch_workdir_precedence(
    monkeypatch, capsys, explicit, here, fresh, expected, note
):
    monkeypatch.setattr(os, "getcwd", lambda: "/worktree")
    monkeypatch.setenv("FNO_REPO_ROOT", "/canonical")
    got = _resolve_dispatch_workdir(explicit, fresh=fresh, here=here)
    assert got == Path(expected).resolve()
    assert ("dispatching from canonical main" in capsys.readouterr().err) is note


def test_resolution_failure_falls_back_to_caller(monkeypatch, capsys):
    # A canonical-resolution exception degrades to the caller cwd, never blocks
    # the dispatch.
    monkeypatch.setattr(os, "getcwd", lambda: "/worktree")

    def _boom():
        raise RuntimeError("no git here")

    monkeypatch.setattr(
        "fno.paths.resolve_canonical_repo_root", _boom, raising=True
    )
    got = _resolve_dispatch_workdir(None, fresh=False, here=False)
    assert got == Path("/worktree").resolve()
    assert "dispatching from canonical main" not in capsys.readouterr().err
