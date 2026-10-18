"""Dispatch-layer tests for codex routing (Wave 2.1).

Verifies the codex create path routes to ``harnesses.codex`` correctly. Provider invocation is monkeypatched so the
dispatch logic is exercised in isolation; real subprocess testing lives
in ``test_codex_integration_smoke.py`` and ``test_codex_signal_handling.py``.

Plan ACs covered:
- AC1-HP create succeeds, registers codex_session_id, exit 0
- AC1-ERR codex exits non-zero, provider exit code propagates
- AC1-EDGE 0-line JSONL -> exit 11 (test_codex_fatal_error_dispatch.py)
- AC1-FR timeout -> exit 15; registry write failure -> exit 12
- AC3-EDGE --yolo recorded in events.jsonl
- AC3 --yolo on claude create maps to bypassPermissions
- AC4-ERR invalid from_name rejected pre-subprocess
"""
from __future__ import annotations

import subprocess
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from fno.paths_testing import use_tmpdir
from fno.agents.harnesses import codex as codex_mod
from fno.agents.harnesses.codex import (
    CodexInvocationError,
    CodexResult,
    CodexTimeoutError,
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


def _read_events() -> list[dict]:
    """Return the committed event rows for the default journal."""
    from fno import paths

    from tests._event_rows import event_rows

    return event_rows(paths.state_dir() / "events.jsonl")


# ---------------------------------------------------------------------------
# AC1 — create
# ---------------------------------------------------------------------------


class _FakeLockHandle:
    """Minimal lock handle stub for _codex_create_path / _claude_create_path tests."""
    def detach(self) -> None:
        pass


def test_create_codex_routes_to_provider_and_registers(workdir, fake_codex_create):
    """Repointed at _codex_create_path (create contract moved from dispatch_ask to spawn verb)."""
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


def test_create_codex_timeout_maps_to_exit_15(workdir, fake_codex_create):
    """Repointed at _codex_create_path."""
    fake_codex_create.side_effect = CodexTimeoutError(2.0)
    from fno.agents.dispatch import DispatchAskError, _codex_create_path
    with pytest.raises(DispatchAskError) as exc:
        _codex_create_path(
            name="worker-X",
            message="msg",
            cwd=workdir,
            from_name="fno",
            yolo=False,
            timeout_sec=2.0,
            lock_handle=_FakeLockHandle(),
        )
    assert exc.value.exit_code == 15
    assert load_registry() == []


def test_create_codex_emits_yolo_flag_in_events(workdir, fake_codex_create):
    """Repointed at _codex_create_path. agent_ask_started is emitted by the caller
    (dispatch_ask / spawn verb) before routing; _codex_create_path emits agent_ask_done."""
    from fno.agents.dispatch import _codex_create_path
    _codex_create_path(
        name="worker-bootstrap",
        message="msg",
        cwd=workdir,
        from_name="fno",
        yolo=True,
        timeout_sec=5.0,
        lock_handle=_FakeLockHandle(),
    )
    events = _read_events()
    done = [e for e in events if e.get("kind") == "agent_ask_done"]
    assert done and done[-1].get("yolo") is True


# ---------------------------------------------------------------------------
# AC3 — --yolo semantics
# ---------------------------------------------------------------------------


def test_yolo_on_claude_create_maps_to_bypass_permissions(workdir, capsys, monkeypatch):
    """x-dfa4: --yolo for claude create now maps to bypassPermissions (was a
    no-op note). bg_create receives permission_mode=bypassPermissions and the
    misleading 'no effect' note is gone."""
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.harnesses.base import ProviderResult
    seen: dict = {}

    def fake_bg_create(**kw):
        seen.update(kw)
        return ProviderResult(
            exit_code=0,
            stdout="backgrounded · 7c5dcf5d · worker-Y\n",
            stderr="",
            duration_ms=10,
            session_id_out="7c5dcf5d",
        )

    monkeypatch.setattr(claude_mod, "bg_create", fake_bg_create)

    from fno.agents.dispatch import _claude_create_path
    _claude_create_path(
        name="worker-Y",
        message="msg",
        chosen="claude",
        cwd=workdir,
        timeout=5,
        yolo=True,
        lock_handle=_FakeLockHandle(),
    )
    err = capsys.readouterr().err
    assert "--yolo has no effect" not in err
    assert seen.get("permission_mode") == "bypassPermissions"


# ---------------------------------------------------------------------------
# AC4 — from-name
# ---------------------------------------------------------------------------


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
# Project assignment (x-dc97)
# ---------------------------------------------------------------------------


def test_assign_codex_project_detached_execs_the_verb(workdir, monkeypatch):
    """The helper execs the hidden fno-agents verb detached, never waiting."""
    import subprocess

    from fno.agents.codex_project import assign_project_detached

    popen = MagicMock()
    monkeypatch.setattr(subprocess, "Popen", popen)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: workdir / "fno-agents")

    assign_project_detached(workdir, "thread-1")

    assert popen.call_count == 1
    argv = popen.call_args.args[0]
    assert argv[1] == "codex-assign-project"
    assert "--thread-id" in argv and argv[argv.index("--thread-id") + 1] == "thread-1"
    assert popen.call_args.kwargs["start_new_session"] is True


def test_assign_codex_project_detached_fails_open(workdir, monkeypatch):
    """Empty session id or a missing binary leaves nothing behind, no raise."""
    import subprocess

    from fno.agents.codex_project import assign_project_detached

    popen = MagicMock()
    monkeypatch.setattr(subprocess, "Popen", popen)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: None)

    assign_project_detached(workdir, "")
    assign_project_detached(workdir, "thread-1")  # binary None -> no exec

    assert popen.call_count == 0


def test_codex_thread_spawn_forwards_node_to_rust_client(monkeypatch, tmp_path):
    from fno import rust_binary
    from fno.agents import dispatch as dispatch_mod

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: Path("/fake/fno-agents"))
    seen: dict[str, list[str]] = {}

    def fake_run(argv, **kwargs):
        seen["argv"] = list(argv)
        return subprocess.CompletedProcess(
            argv,
            0,
            stdout='{"harness_session_id":"thread-node"}\n',
            stderr="",
        )

    monkeypatch.setattr(dispatch_mod.subprocess, "run", fake_run)
    session_id = dispatch_mod._codex_thread_spawn(
        name="worker-X",
        message="msg",
        cwd=tmp_path,
        from_name="fno",
        model=None,
        yolo=False,
        node="x-535c",
    )

    assert session_id == "thread-node"
    assert "--node" in seen["argv"]
    assert seen["argv"][seen["argv"].index("--node") + 1] == "x-535c"


def _write_spawn_stub(tmp_path: Path, body: str) -> Path:
    stub = tmp_path / "fno-agents-stub"
    stub.write_text(f"#!/bin/sh\n{body}\n")
    stub.chmod(0o755)
    return stub


# Emulates the current binary's contract: the state-root door answers with
# the root the inherited env resolves, and a spawn prints a receipt.
_STUB_DOOR_BODY = (
    'if [ "$1" = "state-root" ]; then printf \'%s\\n\' "$FNO_STATE_DIR"; exit 0; fi\n'
    'echo \'{"harness_session_id":"stub-thread"}\''
)


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
            model=None,
            yolo=False,
            account_env={"HOME": str(tmp_path / "account-home")},
        )
    assert "fno doctor update" in str(excinfo.value)
    assert str(stale) in str(excinfo.value)


def test_sealed_codex_thread_spawn_admits_a_binary_that_honors_the_carrier(
    monkeypatch, tmp_path
):
    """A binary whose state-root door echoes the pin spawns through the seal."""
    from fno import rust_binary
    from fno.agents import dispatch as dispatch_mod

    monkeypatch.setenv("HOME", str(tmp_path / "real-home"))
    good = _write_spawn_stub(tmp_path, _STUB_DOOR_BODY)
    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: good)

    session_id = dispatch_mod._codex_thread_spawn(
        name="worker-sealed",
        message="msg",
        cwd=tmp_path,
        from_name="fno",
        model=None,
        yolo=False,
        account_env={"HOME": str(tmp_path / "account-home")},
    )
    assert session_id == "stub-thread"
