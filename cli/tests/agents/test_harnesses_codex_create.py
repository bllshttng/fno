"""Unit tests for ``harnesses.codex.create`` JSONL parser + tee semantics.

Uses the captured fixture from Wave 1.0 as the source-of-truth replay
stream so the parser is exercised against the real codex 0.130.0
vocabulary, not hand-typed event strings.

Plan ACs covered:
- create() spawns `codex exec --json --cd <cwd> --skip-git-repo-check --sandbox <mode> <prompt>` with stdin=DEVNULL and stderr=subprocess.STDOUT
- JSONL parse loop captures session_id from _EVENT_TYPES["session"] events
- JSONL parse loop captures last_msg from _EVENT_TYPES["message"] events
- Tee writes every stdout line to output.jsonl in append mode, line-buffered
- On 0-event stream raise NoSessionIdError carrying the set of event-type names actually seen
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from fno.agents.harnesses import codex as codex_mod
from fno.agents.harnesses.codex import (
    CodexInvocationError,
    CodexResult,
    NoSessionIdError,
)


FIXTURE_PATH = (
    Path(__file__).parent / "fixtures" / "codex-jsonl-sample.jsonl"
)


def _read_fixture_lines() -> list[str]:
    """Return the captured JSONL stream as a list of lines."""
    return FIXTURE_PATH.read_text(encoding="utf-8").splitlines(keepends=True)


def _fixture_thread_id() -> str:
    """Extract the thread.started event's thread_id from the live fixture.

    The fixture is regenerated each time the smoke script runs (Wave 1.0),
    so unit tests cannot hardcode the value — they must read it.
    """
    for raw in _read_fixture_lines():
        stripped = raw.strip()
        if stripped.startswith("{") and '"type":"thread.started"' in stripped:
            event = json.loads(stripped)
            return event["thread_id"]
    raise RuntimeError(f"thread.started not found in fixture {FIXTURE_PATH}")


def _fixture_last_agent_message() -> str:
    """Extract the LAST agent_message text from the live fixture."""
    last = ""
    for raw in _read_fixture_lines():
        stripped = raw.strip()
        if not stripped.startswith("{"):
            continue
        event = json.loads(stripped)
        if (
            event.get("type") == "item.completed"
            and isinstance(event.get("item"), dict)
            and event["item"].get("type") == "agent_message"
        ):
            text = event["item"].get("text")
            if isinstance(text, str):
                last = text
    return last


class _FakePopen:
    """subprocess.Popen stand-in that replays a canned stdout stream.

    Constructor signature mirrors the real Popen's positional + keyword
    arguments so monkeypatching the module attribute works transparently.
    The fake captures argv / kwargs (for assertions) and exposes a
    readline-iterable stdout via :class:`_FakeStdout`.
    """

    def __init__(
        self,
        argv,
        stdin=None,
        stdout=None,
        stderr=None,
        text=False,
        bufsize=0,
        cwd=None,
        env=None,
        start_new_session=False,
    ):
        self.argv = list(argv)
        self.stdin_arg = stdin
        self.stdout_arg = stdout
        self.stderr_arg = stderr
        self.text_arg = text
        self.bufsize_arg = bufsize
        self.cwd_arg = cwd
        self.start_new_session = start_new_session
        self.pid = 99999  # fake PID for getpgid lookups in tests
        # Default: replay the captured fixture; tests override via class attr.
        # Explicit None check so an empty-list override behaves as "no lines",
        # not as "fall through to fixture".
        scripted = self.__class__._scripted_lines
        self._lines = scripted if scripted is not None else _read_fixture_lines()
        self._returncode = self.__class__._scripted_exit_code
        self.stdout = _FakeStdout(self._lines)
        self.terminated = False
        self.killed = False
        self.signals: list[int] = []

    # Class-level knobs so tests can swap behavior without subclassing.
    _scripted_lines: list[str] | None = None
    _scripted_exit_code: int = 0

    def wait(self, timeout=None):
        return self._returncode

    def poll(self):
        # Stdlib Popen.poll returns None if the process is still alive,
        # else its exit code. The fake completes synchronously, so we
        # return the cached exit code (which signals "already reaped").
        return self._returncode

    def terminate(self):
        self.terminated = True

    def kill(self):
        self.killed = True

    def send_signal(self, sig):
        self.signals.append(sig)

    @property
    def returncode(self):
        return self._returncode


class _FakeStdout:
    """Iterable stdin with .readline() conforming to subprocess.PIPE shape."""

    def __init__(self, lines: list[str]):
        self._iter = iter(lines)
        self.closed = False

    def readline(self):
        try:
            return next(self._iter)
        except StopIteration:
            return ""

    def close(self):
        self.closed = True


@pytest.fixture
def fake_popen(monkeypatch):
    """Install _FakePopen as the module-level _subprocess_popen."""
    factory = MagicMock(side_effect=_FakePopen)
    monkeypatch.setattr(codex_mod, "_subprocess_popen", factory)
    # Reset script state between tests.
    _FakePopen._scripted_lines = None
    _FakePopen._scripted_exit_code = 0
    return factory


# ---------------------------------------------------------------------------
# Happy path: fixture replay
# ---------------------------------------------------------------------------


def test_create_captures_session_id_and_last_message_from_fixture(
    tmp_path, fake_popen
):
    out = codex_mod.create(
        cwd=Path("/tmp"),
        prompt="echo hello",
        from_name="fno",
        yolo=False,
        output_path=tmp_path / "output.jsonl",
    )

    assert isinstance(out, CodexResult)
    assert out.exit_code == 0
    # Thread id is dynamic per smoke run; read it from the fixture.
    assert out.session_id == _fixture_thread_id()
    # Last agent_message in the fixture (whatever the model said last).
    assert out.last_msg == _fixture_last_agent_message()
    assert out.duration_ms >= 0


def test_create_tees_every_line_to_output_path(tmp_path, fake_popen):
    out_file = tmp_path / "agents" / "worker-X" / "output.jsonl"
    out_file.parent.mkdir(parents=True)
    out_file.write_text("PRE-EXISTING LINE\n", encoding="utf-8")
    codex_mod.create(
        cwd=Path("/tmp"),
        prompt="prompt",
        from_name="fno",
        yolo=False,
        output_path=out_file,
    )

    assert out_file.exists()
    written = out_file.read_text(encoding="utf-8")
    # Append mode: the stream lands after content already in the file.
    assert written.startswith("PRE-EXISTING LINE\n")
    # Every fixture line — including the non-JSON banner — must be tee'd.
    for raw in _read_fixture_lines():
        assert raw in written, f"missing line in tee: {raw!r}"


def test_create_argv_shape_pin(tmp_path, fake_popen):
    # headless_yolo=False pins the explicit sandboxed argv shape. The autonomous
    # exec lane now DEFAULTS to no-prompt (ab-994222ee); the default-vs-opt-out
    # behavior is covered in test_harnesses_codex_argv.py. This test pins the
    # structural argv (flags + prompt position), so it opts out deterministically.
    codex_mod.create(
        cwd=Path("/tmp/work"),
        prompt="do this",
        from_name="orchestrator",
        yolo=False,
        output_path=tmp_path / "output.jsonl",
        headless_yolo=False,
    )

    fake_popen.assert_called_once()
    # The constructor receives argv as positional kwargs[0] OR call_args[0].
    call_args = fake_popen.call_args
    argv = call_args.args[0]

    # Strict argv ordering: codex --ask-for-approval never exec --json
    # -C <cwd> --skip-git-repo-check --sandbox workspace-write <prompt>.
    # --ask-for-approval is a GLOBAL flag and MUST precede `exec` (codex
    # rejects it after `exec`); --sandbox is an `exec` flag and follows it.
    assert argv[:5] == ["codex", "--ask-for-approval", "never", "exec", "--json"]
    assert argv[5:7] == ["-C", "/tmp/work"]
    assert "--skip-git-repo-check" in argv
    assert "--sandbox" in argv
    assert "workspace-write" in argv
    assert "--dangerously-bypass-approvals-and-sandbox" not in argv
    # Regression (pr704): the global approval flag must come BEFORE `exec`.
    assert argv.index("--ask-for-approval") < argv.index("exec")
    # The prompt arg is the LAST positional and contains the from-name prefix.
    assert argv[-1] == "[from: orchestrator]\n\ndo this"

    # Locked Decision 11 + 12: stdin DEVNULL, stderr merged into stdout.
    assert call_args.kwargs["stdin"] == subprocess.DEVNULL
    assert call_args.kwargs["stdout"] == subprocess.PIPE
    assert call_args.kwargs["stderr"] == subprocess.STDOUT
    assert call_args.kwargs["text"] is True
    assert call_args.kwargs["bufsize"] == 1


def test_create_bounded_argv_grants_git_metadata_write(tmp_path, fake_popen):
    """AC1-HP: a bounded create() into a git repo carries --add-dir <.git>.

    Without it the sandbox refuses index.lock and every commit the worker
    makes fails mid-task.
    """
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)

    codex_mod.create(
        cwd=repo,
        prompt="do this",
        from_name="orchestrator",
        yolo=False,
        output_path=tmp_path / "output.jsonl",
        headless_yolo=False,
    )

    argv = fake_popen.call_args.args[0]
    # Located by PAIR: fno's state-root grant rides the same repeatable flag, so
    # the first --add-dir is no longer necessarily the git one.
    granted = {
        Path(argv[i + 1]).resolve()
        for i, tok in enumerate(argv)
        if tok == "--add-dir"
    }
    assert (repo / ".git").resolve() in granted
    # It is an `exec` flag, so it must follow the subcommand.
    assert argv.index("exec") < argv.index("--add-dir")


def test_create_git_add_dir_composes_with_user_add_dir(tmp_path, fake_popen):
    """AC-EDGE: --add-dir is repeatable, so the git grant is additive - a
    caller's own --add-dir survives instead of being clobbered."""
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)

    codex_mod.create(
        cwd=repo,
        prompt="do this",
        from_name="orchestrator",
        yolo=False,
        output_path=tmp_path / "output.jsonl",
        headless_yolo=False,
        add_dir="/some/shared/dir",
    )

    from fno.agents.writable_dirs import worker_writable_dirs

    argv = fake_popen.call_args.args[0]
    # git common dir + plan dir + the caller's own, plus fno's state-root set,
    # whose SIZE is machine-dependent. Counted from the resolver; presence
    # asserted by name.
    assert argv.count("--add-dir") == 3 + len(worker_writable_dirs(repo))
    assert "/some/shared/dir" in argv
    # The caller's explicit grant leads fno's computed set.
    assert argv.index("/some/shared/dir") < argv.index(worker_writable_dirs(repo)[0])


def test_create_routed_openai_provider_injects_config_and_env(
    tmp_path, fake_popen, monkeypatch
):
    """Item 4 (x-db50): a routed role with an openai-protocol provider prepends
    the `-c` model_provider config (before `exec`) and injects the api key env."""
    from fno.agents import model_routing

    monkeypatch.setattr(
        model_routing,
        "resolve_codex_route",
        lambda role, **kw: model_routing.CodexRoute(
            env={"OPENAI_API_KEY": "oai-key"},
            config_args=[
                "-c",
                "model_providers.zai-openai={ base_url = 'https://z/v4', "
                "env_key = 'OPENAI_API_KEY', wire_api = 'chat' }",
                "-c",
                "model_provider='zai-openai'",
                "-c",
                "model='glm-5.2'",
            ],
        ),
    )
    codex_mod.create(
        cwd=Path("/tmp/work"),
        prompt="do this",
        from_name="fno",
        yolo=False,
        output_path=tmp_path / "output.jsonl",
        headless_yolo=False,
        role="tidy",
    )
    call_args = fake_popen.call_args
    argv = call_args.args[0]
    # The -c config flags are GLOBAL: they must precede `exec`.
    assert argv[0] == "codex"
    assert "-c" in argv and argv.index("-c") < argv.index("exec")
    assert any("model_provider='zai-openai'" in a for a in argv)
    assert any("model='glm-5.2'" in a for a in argv)
    # The api key rides the spawn env (codex reads it via env_key).
    assert call_args.kwargs["env"]["OPENAI_API_KEY"] == "oai-key"


def test_create_yolo_swaps_sandbox_for_dangerous_bypass(tmp_path, fake_popen):
    codex_mod.create(
        cwd=Path("/tmp"),
        prompt="ship it",
        from_name="fno",
        yolo=True,
        output_path=tmp_path / "output.jsonl",
    )
    argv = fake_popen.call_args.args[0]
    assert "--dangerously-bypass-approvals-and-sandbox" in argv
    assert "--sandbox" not in argv
    assert "workspace-write" not in argv


# ---------------------------------------------------------------------------
# Failure modes
# ---------------------------------------------------------------------------


def test_create_stream_with_other_events_but_no_session_warns_with_types(
    tmp_path, fake_popen
):
    # Locked Decision 14: warn-on-drift surfaces what we DID see.
    _FakePopen._scripted_lines = [
        '{"type": "turn.started"}\n',
        '{"type": "item.started", "item": {"type": "command_execution"}}\n',
        '{"type": "turn.completed", "usage": {}}\n',
    ]
    _FakePopen._scripted_exit_code = 0

    with pytest.raises(NoSessionIdError) as exc_info:
        codex_mod.create(
            cwd=Path("/tmp"),
            prompt="prompt",
            from_name="fno",
            yolo=False,
            output_path=tmp_path / "output.jsonl",
        )
    assert exc_info.value.types_seen == {
        "turn.started",
        "item.started",
        "turn.completed",
    }
    assert "did not emit session id" in str(exc_info.value)
    assert "turn.started" in str(exc_info.value)


def test_create_empty_thread_id_is_not_captured_as_session(tmp_path, fake_popen):
    # cv-dcd823ce: an EMPTY thread_id ("") must NOT be captured as the session
    # id. Capturing it would write codex_session_id="" to the registry and make
    # every later resume fail opaquely. The stream still recorded
    # "thread.started" in types_seen, so create fails closed with
    # NoSessionIdError (exit 11). (Mirrors codex_ask.rs's run_codex guard.)
    _FakePopen._scripted_lines = [
        '{"type": "thread.started", "thread_id": ""}\n',
        (
            '{"type": "item.completed", "item": '
            '{"type": "agent_message", "text": "ignored reply"}}\n'
        ),
        '{"type": "turn.completed", "usage": {}}\n',
    ]
    _FakePopen._scripted_exit_code = 0

    with pytest.raises(NoSessionIdError) as exc_info:
        codex_mod.create(
            cwd=Path("/tmp"),
            prompt="prompt",
            from_name="fno",
            yolo=False,
            output_path=tmp_path / "output.jsonl",
        )
    # The event type is still surfaced for forensics even though the id was empty.
    assert "thread.started" in exc_info.value.types_seen


def test_create_nonzero_exit_without_message_raises_invocation_error(
    tmp_path, fake_popen
):
    _FakePopen._scripted_lines = [
        '{"type": "thread.started", "thread_id": "abc-123"}\n',
        # No agent_message, no turn.completed; codex died mid-stream.
    ]
    _FakePopen._scripted_exit_code = 1

    with pytest.raises(CodexInvocationError) as exc_info:
        codex_mod.create(
            cwd=Path("/tmp"),
            prompt="prompt",
            from_name="fno",
            yolo=False,
            output_path=tmp_path / "output.jsonl",
        )
    assert exc_info.value.exit_code == 1


def test_create_missing_codex_binary_raises_invocation_127(
    tmp_path, monkeypatch
):
    def _raise_file_not_found(*args, **kwargs):
        raise FileNotFoundError("codex not found")

    monkeypatch.setattr(codex_mod, "_subprocess_popen", _raise_file_not_found)
    with pytest.raises(CodexInvocationError) as exc_info:
        codex_mod.create(
            cwd=Path("/tmp"),
            prompt="prompt",
            from_name="fno",
            yolo=False,
            output_path=tmp_path / "output.jsonl",
        )
    assert exc_info.value.exit_code == 127


def test_create_tee_open_eacces_raises_invocation_error_exit_12(
    tmp_path, monkeypatch
):
    """silent-failure-hunter row 2 regression: _open_tee failure maps to
    a structured CodexInvocationError (exit 12), not a raw OSError."""
    # Point the output file at a directory we can't write under.
    # Force PermissionError by monkeypatching mkdir to raise it.
    from pathlib import Path as _P

    def _raise_eacces(self, *args, **kwargs):
        raise PermissionError(13, "Permission denied")

    monkeypatch.setattr(_P, "mkdir", _raise_eacces)

    with pytest.raises(CodexInvocationError) as exc_info:
        codex_mod.create(
            cwd=Path("/tmp"),
            prompt="msg",
            from_name="fno",
            yolo=False,
            output_path=tmp_path / "agents" / "worker-X" / "output.jsonl",
        )
    assert exc_info.value.exit_code == 12


def test_create_sigkill_escalation_never_reports_success(tmp_path, fake_popen, monkeypatch):
    """silent-failure-hunter row 3 regression: a force-killed run with a
    partial agent_message previously surfaced as success. Now SIGKILL
    escalation always raises CodexInvocationError."""
    _FakePopen._scripted_lines = [
        '{"type": "thread.started", "thread_id": "sid"}\n',
        (
            '{"type": "item.completed", "item": '
            '{"type": "agent_message", "text": "partial reply"}}\n'
        ),
    ]
    _FakePopen._scripted_exit_code = 0

    # Stub _wait_with_grace to report SIGKILL escalation.
    monkeypatch.setattr(
        codex_mod,
        "_wait_with_grace",
        lambda proc, grace_sec=5.0: (0, True),  # exit 0 but force-killed
    )

    with pytest.raises(CodexInvocationError) as exc_info:
        codex_mod.create(
            cwd=Path("/tmp"),
            prompt="msg",
            from_name="fno",
            yolo=False,
            output_path=tmp_path / "output.jsonl",
        )
    # SIGKILL with exit 0 -> exit 1 (we force a non-zero code for the
    # dispatcher's exit-code propagation).
    assert exc_info.value.exit_code == 1

