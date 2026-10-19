"""Tests for ``fno agents resume`` (resume_logic), the Python fallback leg.

The default runtime routes resume to the Rust client; these pin the fallback's
own refusals (exit 13 / 14) and the codex argv it renders.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

import pytest

@dataclass
class _FakeAgentEntry:
    name: str
    harness: str
    cwd: str
    log_path: str = "/tmp/log.jsonl"
    short_id: Optional[str] = None
    harness_session_id: Optional[str] = None
    substrate: Optional[str] = None

def _allow_all_path(_bin: str) -> bool:
    return True

def _deny_all_path(_bin: str) -> bool:
    return False

def _no_exec(*_args, **_kwargs) -> None:
    """Test stand-in for os.execvp; just records that it would have run."""


@pytest.fixture(autouse=True)
def _no_installed_codex(monkeypatch):
    """Pin the codex version probe so exact argvs do not depend on the codex
    installed on the machine running the suite."""
    from fno.agents import mux_spawn

    monkeypatch.setattr(mux_spawn, "_codex_cli_version", lambda: None)
    monkeypatch.setenv("FNO_CODEX_VERSION", "")

# ---------------------------------------------------------------------------
# AC2-HP — codex resume happy path
# ---------------------------------------------------------------------------

def test_codex_resume_builds_correct_argv_and_cwd() -> None:
    from fno.agents.resume_cli import resume_logic

    entry = _FakeAgentEntry(
        name="alpha",
        harness="codex",
        cwd="/path/to/workdir",
        harness_session_id="00000000-1111-2222-3333-444444444444",
    )

    events_seen: list[dict] = []
    res = resume_logic(
        name="alpha",
        registry_loader=lambda: [entry],
        path_checker=_allow_all_path,
        cwd_checker=lambda _c: True,
        emit_event=lambda kind, **kw: events_seen.append({"kind": kind, **kw}),
        execvp=_no_exec,
    )
    assert res.exit_code == 0
    assert res.exec_argv[0] == "codex"
    # The subcommand and the full session id come from the capability
    # contract, in that order. Nothing trails the positional: codex's globals
    # (the -c grant, --cd) all sit before the subcommand.
    assert "resume" in res.exec_argv
    sid = "00000000-1111-2222-3333-444444444444"
    assert res.exec_argv.index("resume") < res.exec_argv.index(sid)
    # Assert the tail too, not just the order: "resume", the id, then the
    # daemon-attach flag the resume_strategy form always appends.
    assert res.exec_argv[-4:] == ["resume", sid, "--remote", "unix://"]
    # codex 0.156.1 refuses a writable_roots override paired with the form's
    # --remote flag, so the grant is absent; --cd is the only global that
    # rides, and it still precedes the subcommand.
    assert not any("writable_roots=" in arg for arg in res.exec_argv)
    assert res.exec_argv.index("--cd") < res.exec_argv.index("resume")
    assert res.exec_cwd == "/path/to/workdir"


# ---------------------------------------------------------------------------
# AC2-ERR — missing cwd → exit 13 with the handle-cost remedy
# ---------------------------------------------------------------------------

def test_missing_cwd_exits_13_with_handle_remedy() -> None:
    from fno.agents.resume_cli import resume_logic

    entry = _FakeAgentEntry(
        name="alpha", harness="codex",
        cwd="",  # explicit empty
        harness_session_id="sess-1",
    )
    res = resume_logic(
        name="alpha",
        registry_loader=lambda: [entry],
        path_checker=_allow_all_path,
        cwd_checker=lambda _c: True,
        execvp=_no_exec,
    )
    assert res.exit_code == 13
    assert "no recorded cwd" in res.stderr
    assert "the row is the resume handle" in res.stderr
    assert "fno agents adopt <id>" in res.stderr

def test_store_resume_threads_cross_project_and_replacement_cwd(tmp_path, monkeypatch):
    """A pruned store session must resolve and launch in the explicit checkout."""
    from types import SimpleNamespace

    from fno.agents.resume_cli import resume_logic

    entry = _FakeAgentEntry(
        name="adopted",
        harness="codex",
        cwd="/deleted/worktree",
        harness_session_id="ab12cdef-0000-0000-0000-000000000004",
    )
    seen: dict[str, object] = {}

    def resolve(_entries, token, **kwargs):
        seen.update(token=token, **kwargs)
        return SimpleNamespace(entry=entry)

    monkeypatch.setattr("fno.agents.registry.resolve_agent_across_sources", resolve)
    replacement = str(tmp_path)
    res = resume_logic(
        name=entry.harness_session_id,
        cross_project=True,
        cwd_override=replacement,
        print_command=True,
        registry_loader=lambda: [],
        path_checker=_allow_all_path,
        cwd_checker=lambda cwd: cwd == replacement,
        execvp=_no_exec,
    )

    assert res.exit_code == 0
    assert res.exec_cwd == replacement
    assert seen == {
        "token": entry.harness_session_id,
        "scope_cwd": replacement,
        "cross_project": True,
    }

# ---------------------------------------------------------------------------
# AC2-UI — --print-command emits a clean one-liner
# ---------------------------------------------------------------------------

def test_print_command_emits_one_liner() -> None:
    from fno.agents.resume_cli import resume_logic

    entry = _FakeAgentEntry(
        name="alpha", harness="codex",
        cwd="/path/with space",
        harness_session_id="sess-abc",
    )
    res = resume_logic(
        name="alpha",
        print_command=True,
        registry_loader=lambda: [entry],
        path_checker=_allow_all_path,
        cwd_checker=lambda _c: True,
        execvp=_no_exec,
    )
    assert res.exit_code == 0
    assert res.output.count("\n") == 1, "output should be a single line + final newline"
    # cd into the quoted cwd; then exec the provider command.
    assert "cd " in res.output
    assert "exec codex " in res.output
    assert " resume sess-abc" in res.output
    assert " --cd '/path/with space' resume " in res.output
    # The space-containing path must be quoted.
    assert "'/path/with space'" in res.output
    # No banner / no leading prose.
    assert not res.output.startswith("resume:")
    assert not res.output.startswith("$")

# ---------------------------------------------------------------------------
# claude rows: the Python runtime refuses
# ---------------------------------------------------------------------------

def test_claude_python_runtime_refuses_without_launching_legacy_wake(monkeypatch) -> None:
    import subprocess

    from fno.agents.resume_cli import resume_logic

    entry = _FakeAgentEntry(
        name="alpha",
        harness="claude",
        cwd="/cwd",
        short_id="deadbeef",
        harness_session_id="00000000-1111-2222-3333-444444444444",
    )
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    monkeypatch.setattr(
        "fno.agents.harnesses.claude.claude_agents_json",
        lambda: ({"deadbeef": {"live_status": "Working"}}, []),
    )
    real_run = subprocess.run

    def _no_spawn(cmd, *a, **kw):
        if "registry-commit" in [str(part) for part in cmd]:
            return real_run(cmd, *a, **kw)  # the registry read door, not a wake
        pytest.fail("spawned subprocess")

    monkeypatch.setattr(subprocess, "run", _no_spawn)

    res = resume_logic(
        name="alpha",
        registry_loader=lambda: [entry],
        path_checker=_allow_all_path,
        cwd_checker=lambda _cwd: True,
        execvp=_no_exec,
    )

    assert res.exit_code == 13
    assert "fno-agents runtime" in res.stderr
    assert "FNO_AGENTS_RUNTIME=python" in res.stderr
    assert "fno doctor update" in res.stderr

# ---------------------------------------------------------------------------
# AC2-FR — missing session_id → exit 13
# ---------------------------------------------------------------------------

def test_missing_session_id_exits_13() -> None:
    from fno.agents.resume_cli import resume_logic

    entry = _FakeAgentEntry(
        name="alpha", harness="codex",
        cwd="/cwd",
        harness_session_id=None,  # explicit absent
    )
    res = resume_logic(
        name="alpha",
        registry_loader=lambda: [entry],
        path_checker=_allow_all_path,
        cwd_checker=lambda _c: True,
        execvp=_no_exec,
    )
    assert res.exit_code == 13
    assert "session_id" in res.stderr

# ---------------------------------------------------------------------------
# CLI not on PATH → exit 14
# ---------------------------------------------------------------------------

def test_provider_cli_not_on_path_exits_14() -> None:
    from fno.agents.resume_cli import resume_logic

    entry = _FakeAgentEntry(
        name="alpha", harness="codex",
        cwd="/cwd",
        harness_session_id="sess-1",
    )
    res = resume_logic(
        name="alpha",
        registry_loader=lambda: [entry],
        path_checker=_deny_all_path,
        execvp=_no_exec,
    )
    assert res.exit_code == 14
    assert "codex" in res.stderr
    assert "PATH" in res.stderr

# ---------------------------------------------------------------------------
# Unsupported provider → exit 13
# ---------------------------------------------------------------------------

def test_unsupported_provider_exits_13_not_14() -> None:
    """Codex P2 round 2: unsupported provider must return exit 13.

    Pre-fix used exit 14, which collided with "CLI not on PATH" and made
    wrapper diagnostics ambiguous. Module contract reserves 14 for PATH.
    """
    from fno.agents.resume_cli import resume_logic

    entry = _FakeAgentEntry(
        name="alpha", harness="unknown_provider",
        cwd="/cwd",
    )
    res = resume_logic(
        name="alpha",
        registry_loader=lambda: [entry],
        path_checker=_allow_all_path,
        cwd_checker=lambda _c: True,
        execvp=_no_exec,
    )
    assert res.exit_code == 13
    assert "not supported" in res.stderr

# ---------------------------------------------------------------------------
# stale cwd at chdir time
# ---------------------------------------------------------------------------

def test_stale_cwd_that_passes_isdir_but_fails_chdir_still_exits_13() -> None:
    """A directory deleted between the isdir check and the chdir call still
    exits 13 and emits no agent_resumed event. cwd_checker is forced to pass
    so the real os.chdir call is what fails."""
    from fno.agents.resume_cli import resume_logic

    entry = _FakeAgentEntry(
        name="alpha", harness="codex",
        cwd="/this/path/almost/certainly/does/not/exist/" + ("x" * 40),
        harness_session_id="sess-1",
    )
    events_seen: list[dict] = []

    res = resume_logic(
        name="alpha",
        registry_loader=lambda: [entry],
        path_checker=_allow_all_path,
        cwd_checker=lambda _c: True,
        emit_event=lambda kind, **kw: events_seen.append({"kind": kind, **kw}),
        execvp=None,
    )
    assert res.exit_code == 13
    assert "the row is the resume handle" in res.stderr
    assert "fno agents adopt <id>" in res.stderr
    assert events_seen == []

# ---------------------------------------------------------------------------
# codex resume argv
# ---------------------------------------------------------------------------

def test_codex_resume_argv_places_the_worktree_and_forces_no_bypass(monkeypatch) -> None:
    """A codex resume must land in the row's own tree.

    Codex asks session-directory vs current-directory and defaults to the
    SESSION directory, the canonical checkout recorded at spawn. That prompt
    was answered by hand during the 2026-08-25 fleet recovery. Unattended it
    is a hang, and answered wrong it is the wrong tree, which looks like
    success.

    WHAT THIS TEST DOES NOT PROVE. It asserts argv shape. It does not observe
    the modal being suppressed, and no test here does, so do not read a green
    run as proof the hang is gone. Suppression rests on two things instead.
    Codex's own reference documents the flag as `--cd, -C` and states that an
    explicit override takes precedence over the `tui.resume_cwd` config, with
    the prompt raised only when the process cwd differs from the session's
    saved directory. And on codex-cli 0.149.1 the flag was probed in both
    positions: a nonexistent directory fails with `No such file or directory`
    BEFORE the terminal check, which is a positive marker that the value is
    read rather than parsed and dropped.

    Neither is the behavior itself. Closing that needs a recorded session
    resumed against a real tty, which no unit test can host.
    """
    from fno.agents import mux_spawn
    from fno.agents.resume_cli import _build_resume_argv

    monkeypatch.setattr(mux_spawn, "_codex_cli_version", lambda: (0, 162, 0))
    argv = _build_resume_argv("codex", "01a03f51-4704-7f33-942a-e4e773d81cfd",
                              cwd="/tmp/wt/x-04b0")
    assert argv is not None
    # The row's own tree, not the session directory codex would otherwise pick.
    assert "--cd" in argv
    assert argv[argv.index("--cd") + 1] == "/tmp/wt/x-04b0"
    # A global belongs before the subcommand, where the -c grant already sits.
    assert argv.index("--cd") < argv.index("resume")
    # No permission bypass: the row records no sandbox posture, so this lane
    # cannot tell a bounded worker from a yolo one. Hook trust is
    # posture-free and rides first, a global like the grant.
    assert "--dangerously-bypass-approvals-and-sandbox" not in argv
    assert argv[1] == "--dangerously-bypass-hook-trust"
    # Identity still comes from the contract.
    assert argv[0] == "codex"
    assert "01a03f51-4704-7f33-942a-e4e773d81cfd" in argv
    assert argv.index("resume") < argv.index("01a03f51-4704-7f33-942a-e4e773d81cfd")
