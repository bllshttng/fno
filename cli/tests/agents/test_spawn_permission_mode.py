"""`--permission-mode` passthrough across the spawn surfaces (x-dfa4).

Covers the per-provider mapping table (accepted value -> exact argv; unmappable
-> fail-closed error), the claude `--yolo` -> bypassPermissions fix, the
`--permission-mode`/`--yolo` mutual exclusion, and the config default that
applies to autonomous dispatchers only. No test performs a live spawn - the pane
dispatch is stubbed and the autonomous dispatchers' subprocess is mocked.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from types import SimpleNamespace

import pytest
from typer.testing import CliRunner

from fno.agents.dispatch import DispatchAskError
from fno.agents.mux_spawn import build_pane_argv, permission_pane_tokens

CWD = Path("/tmp")


# --- mapping table (permission_pane_tokens) --------------------------------


@pytest.mark.parametrize(
    "provider,mode,expected",
    [
        # The two arms codex_posture.rs's own tests do not reach; the rest of
        # the vocabulary is pinned beside the owner.
        ("gemini", "plan", ["--approval-mode", "plan"]),
        ("opencode", "auto", ["--auto"]),
    ],
)
def test_mapping_accepts_provider_native_values(rust_door, provider, mode, expected):
    assert permission_pane_tokens(provider, mode) == expected


@pytest.mark.parametrize(
    "provider,mode",
    [
        ("opencode", "acceptEdits"),  # AC3-ERR: only 'auto' maps
        ("claude", ""),  # empty value required
    ],
)
def test_mapping_fail_closed_on_unmappable(rust_door, provider, mode):
    with pytest.raises(DispatchAskError) as exc:
        permission_pane_tokens(provider, mode)
    assert exc.value.exit_code == 2


@pytest.mark.dev_build
def test_codex_refusal_names_the_overlay_key(rust_door):
    """fno's own builtin carries as codex's bypass flag; a genuinely foreign
    word still refuses and names the config key that fixes it (AC5-HP)."""
    assert permission_pane_tokens("codex", "bypassPermissions") == [
        "--dangerously-bypass-approvals-and-sandbox"
    ]
    with pytest.raises(DispatchAskError) as exc:
        permission_pane_tokens("codex", "acceptEdits")
    assert "agents.*.harness.codex.permission_mode" in str(exc.value)


@pytest.mark.dev_build
def test_codex_pane_bounded_default_rides_the_rust_owner(rust_door, monkeypatch, tmp_path):
    """AC3-HP: with no mode and no yolo, the pane's bounded default IS the
    thread lane's workspace-write:never pair, tokens straight from
    permission_pane_tokens - pane and thread start from one posture, and the
    network override rides with it."""
    monkeypatch.setattr("fno.agents.mux_spawn.worker_writable_dirs", lambda *a, **k: [])
    argv = build_pane_argv("codex", "hi", tmp_path, False, None, None)
    assert argv[:5] == ["codex", "--remote", "unix://", "-C", str(tmp_path)]
    i = argv.index("--sandbox")
    assert argv[i : i + 6] == [
        "--sandbox",
        "workspace-write",
        "--ask-for-approval",
        "never",
        "-c",
        "sandbox_workspace_write.network_access=true",
    ]


# --- agy pane posture: the lane default and its stderr note -------------------


@pytest.fixture
def rust_door(monkeypatch):
    """Pin the posture door to THIS checkout's fno-agents build; skip where
    the checkout has none (the same contract conftest.native_backlog_door
    implements). A stale installed binary must not answer for this tree."""
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))


@pytest.mark.dev_build
def test_agy_pane_default_keeps_bypass(rust_door, capsys):
    argv = build_pane_argv("agy", "hi", CWD, False, "uuid", None, None)
    assert "--dangerously-skip-permissions" in argv
    # The launch names the posture it ran with, once, on stderr.
    captured = capsys.readouterr().err
    assert "agy posture: bypass (lane-default)" in captured


# --- build_pane_argv integration -------------------------------------------


def test_claude_pane_permission_mode_passthrough():
    """AC1/AC2-HP flavor: explicit mode rides as exact passthrough, no skip flag."""
    argv = build_pane_argv("claude", "hi", CWD, False, "uuid", None, "acceptEdits")
    assert "--permission-mode" in argv and "acceptEdits" in argv
    assert "--dangerously-skip-permissions" not in argv


def test_claude_pane_yolo_maps_to_bypass_permissions(monkeypatch):
    """AC4-HP: claude --yolo now means bypassPermissions (was a no-op)."""
    # About the PERMISSION axis; the writable-dir grant is neutralized here and
    # asserted by name in test_writable_dirs.py.
    monkeypatch.setattr("fno.agents.mux_spawn.worker_writable_dirs", lambda *a, **k: [])
    argv = build_pane_argv("claude", "hi", CWD, True, "uuid", None)
    assert argv[-4:] == ["--permission-mode", "bypassPermissions", "--", "hi"]


def test_codex_pane_colon_two_axis_form():
    """AC2-HP: codex <sandbox>:<approval> -> --sandbox X --ask-for-approval Y."""
    argv = build_pane_argv(
        "codex", "hi", CWD, False, None, None, "workspace-write:on-request"
    )
    assert argv[:8] == [
        "codex",
        "--remote",
        "unix://",
        "-C",
        str(CWD),
        "--sandbox",
        "workspace-write",
        "--ask-for-approval",
    ]


# --- cmd_spawn CLI wiring ---------------------------------------------------


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


@pytest.fixture(autouse=True)
def _no_harness_markers(monkeypatch):
    for m in (
        "CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID", "GEMINI_SESSION_ID"
    ):
        monkeypatch.delenv(m, raising=False)


def _stub_pane_path(monkeypatch) -> dict:
    received: dict = {}
    from fno.agents import mux_spawn, spawn_gate

    class _Gate:
        def release(self) -> None:
            pass

    def fake_dispatch_spawn_pane(**kwargs):
        received.update(kwargs)
        return mux_spawn.MuxSpawnResult(
            name=kwargs["name"],
            provider=kwargs["provider"],
            session="sess-1",
            pane_id=1,
            child_pid=None,
            session_uuid=None,
        )

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    monkeypatch.setattr(mux_spawn, "resolve_provenance", lambda *a, **k: None)
    monkeypatch.setattr(
        mux_spawn, "dispatch_spawn_bounded_pane", fake_dispatch_spawn_pane
    )
    return received


def test_mutual_exclusion_yolo_and_permission_mode(runner, monkeypatch):
    """AC5-ERR: --yolo + --permission-mode is one knob at a time."""
    _stub_pane_path(monkeypatch)
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude", "--yolo",
         "--permission-mode", "plan"],
    )
    assert result.exit_code == 2
    assert "mutually exclusive" in result.output


def test_permission_mode_reaches_pane_dispatch(runner, monkeypatch):
    """The explicit flag threads to dispatch_spawn_pane on the pane substrate."""
    received = _stub_pane_path(monkeypatch)
    # x-85fe: pin canonical == caller so this node-less spawn stays put (AC1-EDGE
    # no-op); otherwise a run from a linked worktree adds the redirect note as a
    # second output line and json.loads(result.output) breaks.
    monkeypatch.setenv("FNO_REPO_ROOT", os.getcwd())
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude",
         "--substrate", "pane", "--permission-mode", "acceptEdits"],
    )
    assert result.exit_code == 0, result.output
    assert received["permission_mode"] == "acceptEdits"
    # Receipt names the REQUESTED mode (x-74ea / x-d401): the receipt can back
    # the request, not the outcome - a forced-default environment can ignore
    # the flag while the old key read as an applied grant.
    receipt = json.loads(result.output)
    assert receipt["permission_mode_requested"] == "acceptEdits"
    assert "permission_mode" not in receipt


@pytest.mark.parametrize(
    "extra_args",
    [
        # explicit non-thread substrate: a bare --resume now IMPLIES thread,
        # so the guard is exercised by pinning a non-thread lane explicitly.
        ["--substrate", "pane"],
        ["--substrate", "thread", "--harness", "codex"],  # thread but non-claude
    ],
)
def test_resume_requires_claude_bg(runner, monkeypatch, extra_args):
    """US4: --resume continues a claude thread transcript, so it is rejected on any
    non-(claude, thread) lane with exit 2 before dispatch. The resume value must be a
    real session uuid: the front-door normalizer validates the shape first."""
    _stub_pane_path(monkeypatch)
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "--name", "hi", "--resume",
         "6501096a-1111-2222-3333-444455556666", *extra_args],
    )
    assert result.exit_code == 2
    assert "--resume requires --substrate thread" in result.output


def test_bg_permission_mode_claude_honored_via_python(runner, monkeypatch):
    """claude bg via the Python fallback HONORS --permission-mode (it threads to
    _claude_create_path -> bg_create), never a hard-fail. Regression guard for
    the availability bug where a config default hard-failed autonomous dispatch."""
    from fno import rust_binary
    from fno.agents import dispatch, spawn_gate

    # Force the Python fallback: a smoke run may leave a real fno-agents
    # reachable for the claim door, which would route this to the Rust runtime.
    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: None)

    captured: dict = {}

    class _Gate:
        def release(self) -> None:
            pass

    def fake_dispatch_spawn(**kwargs):
        captured.update(kwargs)
        return dispatch.SpawnResult(
            kind="created", name=kwargs["name"], provider="claude", short_id="abcd1234"
        )

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    monkeypatch.setattr("fno.agents.dispatch.dispatch_spawn", fake_dispatch_spawn)
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude", "--substrate", "thread",
         "--permission-mode", "acceptEdits"],
    )
    assert result.exit_code == 0, result.output
    assert captured["permission_mode"] == "acceptEdits"
    # Locked Decision 5 / Rust parity: the fallback bg receipt names the
    # REQUESTED mode (x-74ea / x-d401) and carries no outcome-shaped key.
    receipt = json.loads(result.output.splitlines()[0])
    assert receipt["permission_mode_requested"] == "acceptEdits"
    assert "permission_mode" not in receipt


def test_bg_yolo_receipt_names_bypass_via_python(runner, monkeypatch):
    """--yolo on the Python claude bg fallback names bypassPermissions in the
    receipt (audit parity with the Rust bg receipt)."""
    from fno import rust_binary
    from fno.agents import dispatch, spawn_gate

    # Force the Python fallback: a smoke run may leave a real fno-agents
    # reachable for the claim door, which would route this to the Rust runtime.
    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: None)

    class _Gate:
        def release(self) -> None:
            pass

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    monkeypatch.setattr(
        "fno.agents.dispatch.dispatch_spawn",
        lambda **kw: dispatch.SpawnResult(
            kind="created", name=kw["name"], provider="claude", short_id="abcd1234"
        ),
    )
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude", "--substrate", "thread", "--yolo"],
    )
    assert result.exit_code == 0, result.output
    receipt = json.loads(result.output.splitlines()[0])
    assert receipt["permission_mode_requested"] == "bypassPermissions"
    assert "permission_mode" not in receipt


def test_codex_yolo_receipt_names_yolo_via_python(runner, monkeypatch):
    """--yolo on a codex spawn names yolo (codex's own spelling) in the
    receipt, never claude's bypassPermissions (x-6c8a)."""
    from fno import rust_binary
    from fno.agents import dispatch, spawn_gate

    # Force the Python fallback so the receipt under test is this module's.
    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: None)

    class _Gate:
        def release(self) -> None:
            pass

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    monkeypatch.setattr(
        "fno.agents.dispatch.dispatch_spawn",
        lambda **kw: dispatch.SpawnResult(
            kind="created", name=kw["name"], provider="codex", short_id="abcd1234"
        ),
    )
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "codex", "--substrate", "thread", "--yolo"],
    )
    assert result.exit_code == 0, result.output
    receipt = json.loads(result.output.splitlines()[0])
    assert receipt["permission_mode_requested"] == "yolo"
    assert "permission_mode" not in receipt

    # The env-resolved harness counts too: no --harness, the invoking-harness
    # marker names codex, and the receipt still reads yolo.
    monkeypatch.setenv("CODEX_THREAD_ID", "t-env-codex")
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w2", "hi", "--substrate", "thread", "--yolo"],
    )
    assert result.exit_code == 0, result.output
    receipt = json.loads(result.output.splitlines()[0])
    assert receipt["permission_mode_requested"] == "yolo"
    assert "permission_mode" not in receipt


def test_claude_python_build_argv_threads_permission_mode():
    """The Python claude bg argv builder mirrors Rust: --permission-mode rides
    between --name and --model; unset is byte-identical."""
    from fno.agents.harnesses.claude import _build_argv

    argv = _build_argv("w1", "hi", False, None, "acceptEdits")
    assert argv == ["claude", "--bg", "--name", "w1", "--permission-mode",
                    "acceptEdits", "--", "hi"]
    assert _build_argv("w1", "hi", False, None, None) == _build_argv("w1", "hi", False)


# --- AC8: config default applies to autonomous dispatchers only -------------


def _fake_settings(mode: str):
    return SimpleNamespace(
        agents=SimpleNamespace(defaults=SimpleNamespace(permission_mode=mode))
    )


def _capture_spawn(monkeypatch, module):
    """Mock the spawn subprocess at ``module.subprocess.run``; return the cmd."""
    captured: dict = {}
    real_run = module.subprocess.run

    def fake_run(cmd, **kwargs):
        # The mint is a real pre-spawn subprocess (x-84b2): serve it with the
        # real binary and keep it out of the capture, which pins the SPAWN argv.
        if {"name-mint", "name-codes", "name-parse"} & {str(p) for p in cmd}:
            return real_run(cmd, **kwargs)
        # Native event commits are infrastructure: ride the real binary too.
        if {"doctor", "event"} <= {str(p) for p in cmd}:
            return real_run(cmd, **kwargs)
        captured["cmd"] = cmd
        return SimpleNamespace(
            returncode=0, stdout='{"name":"w","short_id":"abcd1234"}', stderr=""
        )

    monkeypatch.setattr(module.subprocess, "run", fake_run)
    return captured


def test_think_worker_reads_config_default(monkeypatch):
    from fno.provenance import spawn_think

    captured = _capture_spawn(monkeypatch, spawn_think)
    monkeypatch.setattr(
        spawn_think, "_settings_for", lambda root: _fake_settings("plan")
    )
    spawn_think._spawn_think_worker("x-test", "prompt", None, "slug")
    assert "--permission-mode" in captured["cmd"]
    i = captured["cmd"].index("--permission-mode")
    assert captured["cmd"][i + 1] == "plan"


def test_think_worker_scopes_tier_to_resolved_dispatch_harness(monkeypatch):
    from fno.provenance import spawn_think

    captured = _capture_spawn(monkeypatch, spawn_think)
    settings = SimpleNamespace(
        agents=SimpleNamespace(defaults=SimpleNamespace(permission_mode="")),
        dispatch=SimpleNamespace(
            harness="codex",
            substrate="",
            command="",
            auto_merge=False,
            allowed_verbs=["/target", "/think"],
        ),
    )
    monkeypatch.setattr(spawn_think, "_settings_for", lambda root: settings)
    model_scopes: list[str] = []
    monkeypatch.setattr(
        spawn_think._route_resolve,
        "node_model",
        lambda node, provider, **_kw: model_scopes.append(provider) or "codex-tier-model",
    )

    spawn_think._spawn_think_worker(
        "x-test",
        "/think x-test\n\nfull context",
        None,
        "slug",
        model="",
        node={"difficulty": "high"},
    )

    cmd = captured["cmd"]
    assert model_scopes == ["codex"]
    assert cmd[cmd.index("--model") + 1] == "codex-tier-model"


def test_think_worker_explicit_pane_uses_shared_capability_gate(monkeypatch):
    from fno.provenance import spawn_think

    captured = _capture_spawn(monkeypatch, spawn_think)
    settings = SimpleNamespace(
        agents=SimpleNamespace(defaults=SimpleNamespace(permission_mode="plan")),
        dispatch=SimpleNamespace(
            harness="",
            substrate="pane",
            command="",
            auto_merge=False,
            allowed_verbs=["/target", "/think"],
        ),
    )
    monkeypatch.setattr(spawn_think, "_settings_for", lambda root: settings)

    spawn_think._spawn_think_worker(
        "x-test",
        "/think x-test\n\nfull context",
        None,
        "slug",
        provider="codex",
    )

    cmd = captured["cmd"]
    assert cmd[cmd.index("--harness") + 1] == "codex"
    assert cmd[cmd.index("--substrate") + 1] == "pane"
    assert cmd[cmd.index("--permission-mode") + 1] == "plan"
    assert cmd[-1] == "$fno:think x-test\n\nfull context"


# --- bash surface: /agent spawn (spawn.sh) ----------------------------------

import shutil  # noqa: E402
import stat  # noqa: E402
import subprocess as _sp  # noqa: E402


@pytest.mark.skipif(shutil.which("jq") is None, reason="spawn.sh needs jq")
def test_spawn_sh_forwards_permission_mode(tmp_path):
    """AC6 flavor: /agent spawn (spawn.sh) forwards --permission-mode verbatim."""
    repo = Path(__file__).resolve().parents[3]
    script = repo / "skills" / "agent" / "scripts" / "spawn.sh"
    assert script.exists()

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    argv_file = tmp_path / "fno-argv.txt"
    fake = bin_dir / "fno"
    fake.write_text(
        "#!/bin/sh\n"
        # Answer the collision probe (`fno agents list`) with an empty roster,
        # and the worktree-ensure with an empty path (so spawn.sh skips setup and
        # never treats the spawn receipt as a worktree dir). Record the spawn argv.
        'case "$*" in\n'
        '  *"agents list"*) printf \'{"agents":[]}\\n\'; exit 0 ;;\n'
        '  *"worktree ensure"*) exit 0 ;;\n'
        'esac\n'
        f'printf "%s\\n" "$@" > "{argv_file}"\n'
        'printf \'{"short_id":"deadbeef"}\\n\'\n'
    )
    fake.chmod(fake.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    env = dict(os.environ)
    env["PATH"] = f"{bin_dir}:{env['PATH']}"
    res = _sp.run(
        ["bash", str(script), "--name", "w1", "--harness", "claude",
         "--message", "hi", "--permission-mode", "acceptEdits"],
        capture_output=True, text=True, env=env, timeout=30, cwd=str(tmp_path),
    )
    assert res.returncode == 0, res.stdout + res.stderr
    forwarded = argv_file.read_text().splitlines()
    assert "--permission-mode" in forwarded
    i = forwarded.index("--permission-mode")
    assert forwarded[i + 1] == "acceptEdits"
