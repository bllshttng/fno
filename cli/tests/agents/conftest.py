"""Shared fixtures for the ``fno agents`` test suite."""
from __future__ import annotations

import pytest


@pytest.fixture(autouse=True)
def _collapse_pane_binding_window(monkeypatch):
    """Collapse the pane binding window so no test pays its wall-clock.

    Production waits `_BINDING_WINDOW_S` (8s) for a pane to bind a session: long
    enough to outlast a slow rollout, and comfortably under the 20s dispatch
    subprocess kill that a longer window would run into. That ceiling is only
    ever paid on the ambiguous path, but a test
    whose fake mux reports a permanently live, never-binding pane would sit out
    the whole thing. Tests that exercise the window pass ``window_s=``
    explicitly and ignore this.
    """
    monkeypatch.setenv("FNO_PANE_BINDING_WINDOW_S", "0.01")


@pytest.fixture(autouse=True)
def _isolate_session_discovery(monkeypatch, tmp_path_factory):
    """Point P1 live-session discovery (ab-098967b4) at an empty tmp dir.

    ``fno agents list`` discovers live sessions by default, reading Claude
    Code's ~/.claude/sessions registry. Without this, the suite would read the
    developer's real sessions dir and `agents list` JSON-shape assertions would
    be host-dependent. Tests in test_discover.py pass ``sessions_dir=``
    explicitly and are unaffected by this env override.
    """
    from fno.agents import discover

    empty = tmp_path_factory.mktemp("empty-claude-sessions")
    monkeypatch.setenv(discover.SESSIONS_DIR_ENV, str(empty))
    # Codex disk-discovery is pure mtime, not psutil-gated, so it would
    # read the developer's real ~/.codex/sessions unless isolated here.
    empty_codex = tmp_path_factory.mktemp("empty-codex-sessions")
    monkeypatch.setenv(discover.CODEX_SESSIONS_DIR_ENV, str(empty_codex))
    monkeypatch.setenv("CODEX_HOME", str(tmp_path_factory.mktemp("empty-codex-home")))
    # Same for the opencode lane. Defense-in-depth, NOT the load-bearing
    # barrier: the parent conftest redirects $HOME before any test module
    # imports, and the default resolves through expanduser("~"), so the real
    # store is already out of reach. This pins the lane directly so it survives
    # a refactor away from expanduser.
    empty_opencode = tmp_path_factory.mktemp("empty-opencode-storage")
    monkeypatch.setenv(discover.OPENCODE_STORAGE_DIR_ENV, str(empty_opencode))


@pytest.fixture(autouse=True)
def _isolate_session_uuid_resolution(monkeypatch):
    """Keep full-UUID resolution host-independent.

    The suite stubs the registry reader to ``None`` (no read of the
    developer's real ``~/.claude/sessions``), so a claude row leaves
    ``claude_session_uuid`` unresolved. Tests that exercise resolution
    override it per-test (monkeypatch order: the test-local setattr wins).
    """
    from fno.agents.harnesses import claude

    monkeypatch.setattr(claude, "resolve_session_uuid", lambda short_id: None)


FAKE_CHILD_PID = 4242


@pytest.fixture(autouse=True)
def _pin_fake_child_pid_probe(monkeypatch):
    """Keep FakeRunner's fixture child pid reading as gone on any host.

    ``FakeRunner`` (test_spawn_pane.py) reports ``child_pid: 4242``, and
    ``_reap_spawned_pane`` guards the pane kill with
    ``spawn_gate._process_start_time`` on that pid - a real read of the host
    process table. When an unrelated live process holds pid 4242 (pids recycle
    constantly on a busy linux runner), the before and after reads match and
    the reap reports the worker survived teardown, failing tests that have
    nothing to do with that host. Pin the one fixture pid to gone and delegate
    every other pid to the real probe; a test that exercises the probe itself
    patches it per-test and wins (monkeypatch order: test wins). The constant
    and this docstring are the cross-reference that keeps FakeRunner's pid
    and FAKE_CHILD_PID the same number.
    """
    from fno.agents import spawn_gate

    real = spawn_gate._process_start_time
    monkeypatch.setattr(
        spawn_gate,
        "_process_start_time",
        lambda pid, *args, **kwargs: (
            None if pid == FAKE_CHILD_PID else real(pid, *args, **kwargs)
        ),
    )


# The provider-exec guard lives in the ROOT cli/tests/conftest.py
# (_block_live_provider_exec) so every cli test is covered, not just this
# directory. x-ec81: the agents-only copy left 36 test files outside
# cli/tests/agents/ reaching a spawn seam unguarded, and one leaked three live
# claude daemons. Do not re-add a copy here.
