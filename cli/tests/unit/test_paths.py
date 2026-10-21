"""Tests for fno.paths typed resolver.

Task 1.2: Create paths.py typed resolver with template substitution.
Task 1.4: Round out coverage for all AC items.

All tests use tmp_path + monkeypatch isolation. An autouse fixture pins
FNO_REPO_ROOT to tmp_path so resolve_repo_root() is isolated
(feedback_fno_repo_root_leaks_between_tests memory entry).
"""
from __future__ import annotations

from pathlib import Path
from typing import Generator

import pytest


# ---------------------------------------------------------------------------
# Autouse fixture: pin FNO_REPO_ROOT and clear caches before each test
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Generator[None, None, None]:
    """Isolate each test: reset caches and pin repo root + settings."""
    # Pin FNO_REPO_ROOT so resolve_repo_root() doesn't wander
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    # Clear settings cache so monkeypatched env takes effect
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    # Clear paths caches (resolve_repo_root now @cached)
    yield
    # Clear again after test to avoid pollution
def _write_settings(tmp_path: Path, content: str) -> Path:
    """Write a settings.yaml to tmp_path and return its path."""
    f = tmp_path / "settings.yaml"
    f.write_text(content, encoding="utf-8")
    return f


def _set_settings(monkeypatch: pytest.MonkeyPatch, tmp_path: Path, content: str) -> None:
    """Write a settings.yaml and wire it via FNO_CONFIG."""
    settings_file = _write_settings(tmp_path, content)
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))


# ---------------------------------------------------------------------------
# ab-fe825805 change 4: FNO_REPO_ROOT foreign-project overload warning
# ---------------------------------------------------------------------------


def _fake_git_toplevel(path: Path):
    """A subprocess.run stand-in that reports `path` as the cwd's git toplevel."""
    return lambda *a, **k: type("R", (), {"returncode": 0, "stdout": str(path) + "\n"})()


def _make_plugin_root(path: Path) -> Path:
    """Stamp `path` with the fno plugin marker file so _is_plugin_root()
    recognizes it (the warning gate keys on the marker, not the basename)."""
    marker = path / "hooks" / "helpers" / "init-target-state.sh"
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text("#!/usr/bin/env bash\n")
    return path


def test_fno_repo_root_warns_when_pinned_to_fno_from_foreign_repo(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """FNO_REPO_ROOT pinned at the fno plugin root + a different cwd repo
    emits a one-line stderr heads-up (the silent-wrong-project footgun)."""
    import fno.paths as paths_mod

    fno_dir = _make_plugin_root(tmp_path / "fno")
    other_repo = tmp_path / "acme-web"
    other_repo.mkdir()
    monkeypatch.setenv("FNO_REPO_ROOT", str(fno_dir))
    # cwd (the pytest cwd) is not inside fno_dir, so the same-repo
    # short-circuit does not fire; the (stubbed) git probe reports other_repo.
    monkeypatch.setattr(paths_mod.subprocess, "run", _fake_git_toplevel(other_repo))

    result = paths_mod.resolve_repo_root()

    assert result == fno_dir.resolve()  # warning is non-fatal
    err = capsys.readouterr().err
    assert "FNO_REPO_ROOT pins" in err
    assert str(fno_dir.resolve()) in err


def test_resolve_canonical_repo_root_uses_git_worktree_list(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The main worktree path from `git worktree list` is the canonical root.

    Simulates a linked worktree: `git worktree list --porcelain` lists the main
    worktree first, and its `worktree <path>` line is the canonical working
    tree. The canonical dir carries a `.git` child so the working-tree gate in
    resolve_canonical_worktree() (which skips bare/separate-git-dir gitdir
    entries) accepts it (ab-91a004af worktree-resolution).
    """
    import fno.paths as paths_mod

    # Leave any real repo so the filesystem short-circuit in
    # resolve_canonical_worktree() stays out of the way and the stubbed
    # porcelain parse is what runs.
    monkeypatch.chdir(tmp_path)
    monkeypatch.delenv("FNO_REPO_ROOT", raising=False)
    canonical = tmp_path / "canonical"
    linked = tmp_path / "linked"
    canonical.mkdir(parents=True)
    linked.mkdir(parents=True)
    # A real working tree has a `.git` child; the helper requires it.
    (canonical / ".git").mkdir()
    (linked / ".git").mkdir()

    class _Result:
        returncode = 0
        # Main worktree first (canonical), then a linked worktree.
        stdout = (
            f"worktree {canonical}\n"
            "HEAD 0000000000000000000000000000000000000000\n"
            "branch refs/heads/main\n"
            "\n"
            f"worktree {linked}\n"
            "HEAD 1111111111111111111111111111111111111111\n"
            "branch refs/heads/feature\n"
        )

    monkeypatch.setattr(paths_mod.subprocess, "run", lambda *a, **k: _Result())

    assert paths_mod.resolve_canonical_repo_root() == canonical.resolve()


def test_ledger_json_pinned_global_ignores_relative_state_dir(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The ledger is cross-project and must NOT fork into a per-repo
    stray. A relative (project-/CWD-anchored) state_dir must not drag the
    ledger into the repo checkout; it stays anchored to the user-global
    ~/.fno. An absolute state_dir (the default and test sandboxes) is honored.
    """
    monkeypatch.chdir(tmp_path)
    _set_settings(
        monkeypatch, tmp_path, "schema_version: 1\nconfig:\n  state_dir: .fno/\n"
    )

    from fno.paths import ledger_json

    result = ledger_json()
    # Pinned to ~/.fno, NOT tmp_path/.fno (which is where a relative state_dir
    # would land graph.json/events under CWD).
    assert result == (Path.home() / ".fno" / "ledger.json").resolve()
    assert tmp_path not in result.parents


def test_global_events_json_ignores_relative_ledger_override(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The journals stopped anchoring on the ledger's folder: a relative
    ``config.paths.ledger_json`` moves the ledger, never the journals."""
    _set_settings(
        monkeypatch,
        tmp_path,
        "schema_version: 1\nconfig:\n  paths:\n    ledger_json: state/ledger.json\n",
    )

    from fno.paths import global_events_json, ledger_json

    ledger = ledger_json()
    assert ledger == (Path.home() / ".fno" / "state" / "ledger.json").resolve()
    assert global_events_json() == (Path.home() / ".fno" / "events.jsonl").resolve()


def test_graphql_quota_lock_ignores_project_state_dir(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """One GitHub identity needs one machine lock across all repositories."""
    machine_home = tmp_path / "home"
    monkeypatch.setenv("HOME", str(machine_home))
    _set_settings(
        monkeypatch,
        tmp_path,
        f"schema_version: 1\nconfig:\n  state_dir: '{tmp_path / 'project-state'}'\n",
    )

    from fno.paths import graphql_quota_lock

    assert graphql_quota_lock() == (
        machine_home / ".fno" / "state" / "locks" / "github-graphql-quota.lock"
    )


# ---------------------------------------------------------------------------
# AC1-FR: Cache lifetime (same object on second call)
# ---------------------------------------------------------------------------


def test_paths_cache_sees_a_settings_edit(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """The settings cache key fingerprints the file contents (x-b545): a
    same-key rewrite IS seen in-process, with no cache_clear. The edited
    state_dir must stay inside the sandbox so the hermetic guard permits it."""
    changed_dir = tmp_path / "changed"
    settings_file = _write_settings(tmp_path, "schema_version: 1\n")
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))

    from fno.paths import graph_json

    first = graph_json()
    # Rewrite the file - the cache reparses and the new state_dir resolves.
    settings_file.write_text(
        f"schema_version: 1\nconfig:\n  state_dir: '{changed_dir}'\n",
        encoding="utf-8",
    )
    second = graph_json()
    assert second == changed_dir.resolve() / "db" / "graph.json"
    assert first != second, "a settings edit must invalidate the keyed cache"


# ---------------------------------------------------------------------------
# AC1-EDGE: Unknown {foo} variable rejected at resolve time
# ---------------------------------------------------------------------------


def test_unknown_template_variable_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC1-EDGE: {foo} in a path raises a hard error at resolve time."""
    _set_settings(
        monkeypatch,
        tmp_path,
        "schema_version: 1\nconfig:\n  state_dir: '/home/{foo}/fno'\n",
    )

    from fno.paths import state_dir

    with pytest.raises(Exception, match=r"\{foo\}|unknown.*variable|unrecognized"):
        state_dir()


# ---------------------------------------------------------------------------
# AC1-EDGE: fleet_dir, postmortems_dir, worktrees_base, memory_dir all derive from state_dir
# ---------------------------------------------------------------------------


def test_all_global_dirs_derive_from_custom_state_dir(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC1-EDGE: All global dirs derive from state_dir when paths.* block is empty."""
    custom_dir = str(tmp_path / "mystate")
    _set_settings(
        monkeypatch,
        tmp_path,
        f"schema_version: 1\nconfig:\n  state_dir: '{custom_dir}'\n",
    )

    from fno.paths import (
        fleet_dir,
        global_events_json,
        ledger_json,
        memory_dir,
        postmortems_dir,
        worktrees_base,
    )

    base = Path(custom_dir).resolve()
    assert fleet_dir() == base / "fleet"
    assert postmortems_dir() == base / "postmortems"
    assert worktrees_base() == base / "worktrees"
    assert memory_dir() == base / "memory"
    assert ledger_json() == base / "ledger.json"
    assert global_events_json() == base / "events.jsonl"


def test_handoffs_dir_bare_vault_name_anchors_at_home(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Regression test for ab-347f6482.

    obsidian.vault is conventionally a bare vault name (e.g. 'myvault')
    mapping to ~/<name> - the semantics vault_root() already implements.
    _resolve()'s {vault} substitution returned the raw relative value, so
    the assembled path anchored at project_root (the current worktree)
    and every pre-promise handoff landed at a junk worktree-local path.
    """
    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setenv("HOME", str(fake_home))
    worktree = tmp_path / "conductor" / "loc-ratchet"
    worktree.mkdir(parents=True)
    _set_settings(
        monkeypatch,
        tmp_path,
        "schema_version: 1\n"
        "config:\n"
        "  obsidian:\n    enabled: true\n    vault: 'myvault'\n"
        "  project:\n    id: 'myproj'\n",
    )

    from fno.paths import handoffs_dir

    result = handoffs_dir(project_root=worktree)
    expected = (fake_home / "myvault" / "internal" / "myproj" / "handoffs").resolve()
    assert result == expected, f"bare vault name must anchor at $HOME, got {result}"
    assert not str(result).startswith(str(worktree)), (
        "handoffs_dir must never anchor inside the worktree"
    )


# ---------------------------------------------------------------------------
# x-2e75: stable project-folder identity for internal/<project>/ paths.
# When config.project.id is unset, derive the folder name from the git remote
# (stable across worktrees/clones) instead of the checkout basename, which
# sprawls N stray internal/<name>/ folders in a shared vault. Sanitized so a
# derived or configured name can never escape internal/<project>/.
# ---------------------------------------------------------------------------


def _git_init_with_remote(d: Path, url: str | None) -> None:
    """Init a git repo at ``d`` with an optional origin remote."""
    import subprocess

    d.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    if url is not None:
        subprocess.run(["git", "remote", "add", "origin", url], cwd=d, check=True)


_VAULT_SETTINGS = (
    "schema_version: 1\nconfig:\n"
    "  obsidian:\n    enabled: true\n    vault: '{vault}'\n"
)


def test_handoffs_dir_uses_git_remote_slug_not_basename(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Scenario 1: git remote drives the folder name, not the checkout basename."""
    checkout = tmp_path / "fno-attest-placement"
    _git_init_with_remote(checkout, "git@github.com:org/footnote.git")
    vault = tmp_path / "vault"
    _set_settings(monkeypatch, tmp_path, _VAULT_SETTINGS.format(vault=vault))
    monkeypatch.setattr("fno.paths._warned_unset_project_id", False, raising=False)

    from fno.paths import handoffs_dir

    result = handoffs_dir(project_root=checkout)
    assert str(result).endswith("internal/footnote/handoffs")
    assert "fno-attest-placement" not in str(result)


def test_traversal_project_id_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Scenario 4: a traversal-bearing config.project.id is rejected (raises)."""
    vault = tmp_path / "vault"
    _set_settings(
        monkeypatch,
        tmp_path,
        _VAULT_SETTINGS.format(vault=vault) + "  project:\n    id: '../../etc'\n",
    )

    from fno.paths import handoffs_dir

    with pytest.raises(ValueError):
        handoffs_dir(project_root=tmp_path)


# ---------------------------------------------------------------------------
# x-fc25: FNO_STATE_DIR - the pinned carrier for the state root
# ---------------------------------------------------------------------------


def test_state_dir_honors_fno_state_dir_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The env carrier moves the state root ahead of the config default.

    seal_state_root pins this var around a forwarded HOME, so a worker on a
    non-claude oauth_dir account resolves the same graph.json its parent did.
    """
    _set_settings(monkeypatch, tmp_path, "schema_version: 1\n")
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / "pinned"))

    from fno.paths import graph_json, state_dir

    assert state_dir() == (tmp_path / "pinned").resolve()
    assert graph_json() == (tmp_path / "pinned").resolve() / "db" / "graph.json"


def test_ledger_json_seal_beats_the_ledger_override(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """FNO_STATE_DIR wins before any config.paths.ledger_json override: a
    sealed worker never chases an override into a foreign root."""
    override_ledger = tmp_path / "elsewhere" / "ledger.json"
    _set_settings(
        monkeypatch,
        tmp_path,
        "schema_version: 1\n"
        "config:\n"
        "  paths:\n"
        f"    ledger_json: '{override_ledger}'\n",
    )
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / "pinned"))

    from fno.paths import ledger_json

    assert ledger_json() == (tmp_path / "pinned").resolve() / "ledger.json"


def test_migrate_from_checkout_refuses_the_state_root(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC5-EDGE: the state root is not a checkout journal. A session whose
    cwd was $HOME outside a checkout once moved the GLOBAL journal into a fake
    space behind a MOVED-TO pointer at the top level of the state root; the
    refusal keeps the pointer and the fake-space move from ever recurring."""
    state = tmp_path / ".fno"
    state.mkdir()
    old = state / "events.jsonl"
    old.write_text("global rows\n")
    new = tmp_path / "spaces" / "x" / "events.jsonl"
    monkeypatch.setenv("FNO_STATE_DIR", str(state))

    from fno.paths import migrate_from_checkout

    assert migrate_from_checkout(old, new) is False
    assert old.exists()
    assert not (state / "MOVED-TO").exists()


def test_agents_registry_path_follows_declared_agents_home(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC1-HP: with FNO_AGENTS_HOME declared, a bare write_registry lands in
    the declared home, never the config state_dir (the 2026-09-27 probe
    overwrote the live registry through exactly this gap)."""
    from fno.agents.registry import AgentEntry, write_registry

    _set_settings(
        monkeypatch,
        tmp_path,
        f"schema_version: 1\nconfig:\n  state_dir: '{tmp_path / '.fno'}'\n",
    )
    declared = tmp_path / "other" / "agents"
    declared.mkdir(parents=True)
    monkeypatch.setenv("FNO_AGENTS_HOME", str(declared))

    entry = AgentEntry(
        name="leader",
        cwd="/tmp/x",
        log_path="/tmp/x/log",
        harness="claude",
        harness_session_id="aaaaaaaa-0000-0000-0000-111111111111",
    )
    write_registry([entry])

    assert (declared / "registry.json").exists()
    assert not (tmp_path / ".fno" / "agents" / "registry.json").exists()
