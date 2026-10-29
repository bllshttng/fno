"""Dispatch-layer tests for the codex create path and sealed thread spawn.

Provider invocation is monkeypatched so the dispatch logic is exercised in
isolation. Covered: create registers the row, each provider failure maps to
its exit code with the registry untouched, an invalid from_name is refused
before any subprocess, and a stale binary is refused under a sealed env.
"""
from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from fno.paths_testing import use_tmpdir
from fno.agents.harnesses import codex as codex_mod
from fno.agents.harnesses.codex import (
    CodexInvocationError,
    CodexResult,
    NoSessionIdError,
)
from fno.agents.registry import (
    load_registry,
)


@pytest.fixture
def workdir(tmp_path, monkeypatch):
    """Isolated fno home with codex marked available on PATH."""
    use_tmpdir(monkeypatch, tmp_path)
    # Stub PATH so `which("codex")` returns truthy.
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "codex").write_text("#!/bin/sh\necho fake\n", encoding="utf-8")
    (bin_dir / "codex").chmod(0o755)
    monkeypatch.setenv("PATH", str(bin_dir))
    return tmp_path


@pytest.fixture
def fake_codex_create(monkeypatch):
    """Replace codex_mod.create with a MagicMock returning a happy CodexResult.

    Tests can mutate the return_value or side_effect to script failures.
    """
    mock = MagicMock(return_value=CodexResult(
        exit_code=0,
        session_id="codex-sid-abc",
        last_msg="hello",
        duration_ms=42,
    ))
    monkeypatch.setattr(codex_mod, "create", mock)
    return mock


# ---------------------------------------------------------------------------
# AC1 — create
# ---------------------------------------------------------------------------


class _FakeLockHandle:
    """Minimal lock handle stub for _codex_create_path / _claude_create_path tests."""
    def detach(self) -> None:
        pass


def test_create_codex_routes_to_provider_and_registers(workdir, fake_codex_create):
    """Repointed at _codex_create_path (create contract moved from dispatch_ask to spawn command)."""
    from fno.agents.dispatch import _codex_create_path

    result = _codex_create_path(
        name="worker-X",
        message="echo hello",
        cwd=workdir,
        from_name="fno",
        yolo=False,
        timeout_sec=10.0,
        effort="low",
        lock_handle=_FakeLockHandle(),
    )

    assert result.kind == "followup"  # codex reply printed verbatim like a followup
    assert result.short_id == "codex-sid-abc"
    assert result.reply == "hello"

    fake_codex_create.assert_called_once()
    call = fake_codex_create.call_args
    assert call.kwargs["cwd"] == workdir
    assert call.kwargs["yolo"] is False
    assert call.kwargs["from_name"] == "fno"
    assert call.kwargs["prompt"] == "echo hello"

    entries = load_registry()
    assert len(entries) == 1
    e = entries[0]
    assert e.name == "worker-X"
    assert e.harness == "codex"
    assert e.harness_session_id == "codex-sid-abc"
    assert e.cwd == str(workdir)
    assert e.status == "live"
    assert e.effort == "low"


def test_create_codex_treats_identity_collision_as_registry_failure(
    workdir, fake_codex_create, monkeypatch
):
    from fno.agents import dispatch as dispatch_mod
    from fno.agents.dispatch import DispatchAskError, _codex_create_path
    from fno.agents.registry import AgentResolutionError

    detached: list[bool] = []

    class _TrackedLockHandle:
        def detach(self) -> None:
            detached.append(True)

    monkeypatch.setattr(
        dispatch_mod,
        "update_registry",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            AgentResolutionError("identity collision", ambiguous=True)
        ),
    )

    with pytest.raises(DispatchAskError, match="identity collision") as exc:
        _codex_create_path(
            name="worker-X",
            message="echo hello",
            cwd=workdir,
            from_name="fno",
            yolo=False,
            timeout_sec=10.0,
            lock_handle=_TrackedLockHandle(),
        )

    assert exc.value.exit_code == 12
    assert detached == [True]


def test_create_codex_no_session_id_maps_to_exit_11(workdir, fake_codex_create):
    """Repointed at _codex_create_path."""
    fake_codex_create.side_effect = NoSessionIdError({"turn.started", "turn.completed"})

    from fno.agents.dispatch import DispatchAskError, _codex_create_path
    with pytest.raises(DispatchAskError) as exc:
        _codex_create_path(
            name="worker-X",
            message="msg",
            cwd=workdir,
            from_name="fno",
            yolo=False,
            timeout_sec=5.0,
            lock_handle=_FakeLockHandle(),
        )
    assert exc.value.exit_code == 11
    # Registry MUST NOT have a row for a failed create.
    assert load_registry() == []


def test_create_codex_propagates_provider_specific_exit_code(workdir, fake_codex_create):
    """Gemini PR #305 round 3: dispatch must propagate CodexInvocationError.exit_code.
    Repointed at _codex_create_path."""
    fake_codex_create.side_effect = CodexInvocationError(12)
    from fno.agents.dispatch import DispatchAskError, _codex_create_path
    with pytest.raises(DispatchAskError) as exc:
        _codex_create_path(
            name="worker-X",
            message="msg",
            cwd=workdir,
            from_name="fno",
            yolo=False,
            timeout_sec=5.0,
            lock_handle=_FakeLockHandle(),
        )
    assert exc.value.exit_code == 12

    fake_codex_create.side_effect = CodexInvocationError(127)
    with pytest.raises(DispatchAskError) as exc:
        _codex_create_path(
            name="worker-Y",
            message="msg",
            cwd=workdir,
            from_name="fno",
            yolo=False,
            timeout_sec=5.0,
            lock_handle=_FakeLockHandle(),
        )
    assert exc.value.exit_code == 127


# ---------------------------------------------------------------------------
# AC4 — from-name
# ---------------------------------------------------------------------------


    # The actual bracket prefix is applied by codex.create() (verified in
    # test_harnesses_codex_create), but we assert dispatch passes the
    # name through unchanged.


def test_invalid_from_name_rejected_before_subprocess(workdir, fake_codex_create):
    """AC4-ERR: from_name validator rejects XML-unsafe input on the codex path.

    Reuses the US2 validator (no fresh regex per AC1.3 invariant); the rule
    set is: non-empty AND <=128 chars AND no XML-unsafe characters (", <, >, &).
    """
    from fno.agents.dispatch import DispatchAskError, _validate_inputs
    for bad in ('', '"bad"', '<inject>', 'amp&persand', 'right>angle'):
        with pytest.raises(DispatchAskError) as exc:
            _validate_inputs(name="worker-X", message="msg", from_name=bad)
        assert exc.value.exit_code == 2, f"input {bad!r}"
    # Provider MUST NOT have been called for any of the rejected inputs.
    fake_codex_create.assert_not_called()


# ---------------------------------------------------------------------------
# Sealed thread spawn
# ---------------------------------------------------------------------------


def _write_spawn_stub(tmp_path: Path, body: str) -> Path:
    stub = tmp_path / "fno-agents-stub"
    stub.write_text(f"#!/bin/sh\n{body}\n")
    stub.chmod(0o755)
    return stub


# Emulates the current binary's contract: the state-root door answers with
# the root the inherited env resolves, and a spawn prints a receipt.


def test_sealed_codex_thread_spawn_refuses_a_stale_binary(monkeypatch, tmp_path):
    """A binary predating the FNO_STATE_DIR carrier must not inherit a sealed env."""
    from fno import rust_binary
    from fno.agents import dispatch as dispatch_mod

    monkeypatch.setenv("HOME", str(tmp_path / "real-home"))
    stale = _write_spawn_stub(
        tmp_path, 'echo "read graph from $HOME/.fno/graph.json"'
    )
    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: stale)

    with pytest.raises(dispatch_mod.DispatchAskError) as excinfo:
        dispatch_mod._codex_thread_spawn(
            name="worker-stale",
            message="msg",
            cwd=tmp_path,
            from_name="fno",
            account_env={"HOME": str(tmp_path / "account-home")},
        )
    assert "fno doctor update" in str(excinfo.value)
    assert str(stale) in str(excinfo.value)
