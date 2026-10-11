"""agy mints its conversation id through one print-mode turn.

The stream's init line is the ONLY surface that returns the id before the
harness Stop hook holds the process alive, so every way that read can fail
has to raise rather than hand the keeper a name it made up. The live shape
asserted here is the stream-json one measured on agy 1.3.3: line 1 is
``{"event":"init","conversation_id":...}``.
"""
from __future__ import annotations

import json
import os
import subprocess

import pytest

from fno.agents.harnesses import agy


@pytest.fixture
def rust_door(monkeypatch):
    """Pin the mint-argv door to THIS checkout's fno-agents build; skip where
    the checkout has none (the same contract conftest.native_backlog_door
    implements)."""
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))


class _FakeAgy:
    """Popen double with a REAL pipe: select/os.read in the mint reader need
    a true fd, so CompletedProcess doubles do not work here."""

    def __init__(self, lines: list[str], returncode: int = 0, hold: bool = False):
        read_fd, write_fd = os.pipe()
        self.stdout = os.fdopen(read_fd, "rb", buffering=0)
        os.write(write_fd, "".join(line + "\n" for line in lines).encode())
        if not hold:
            os.close(write_fd)
        self._write_fd = write_fd
        self.returncode = None if hold else returncode
        self.terminated = False
        self.killed = False
        self.waited = False

    def poll(self) -> int | None:
        return self.returncode

    def terminate(self) -> None:
        self.terminated = True
        self.returncode = -15

    def kill(self) -> None:
        self.killed = True
        self.returncode = -9

    def wait(self, timeout: float | None = None) -> int | None:
        self.waited = True
        return self.returncode


ENVELOPE = {
    "conversation_id": "c5661b28-bcba-4690-8b2e-4a4a88541e8c",
    "status": "SUCCESS",
    "response": "OK\n",
    "duration_seconds": 1.497642,
    "num_turns": 1,
}

INIT_LINE = json.dumps(
    {"event": "init", "conversation_id": ENVELOPE["conversation_id"]}
)


def _agy_only_popen(monkeypatch, maker):
    """Stub ONLY the agy process: the mint-argv door runs the real fno-agents
    binary through the same stdlib subprocess.Popen, so a blanket stub would
    answer for the binary too (its answer would be mint-nonsense)."""
    real_popen = subprocess.Popen

    def _popen(argv, *positional, **kwargs):
        if argv and argv[0] == agy.AGY_BINARY:
            return maker(argv, kwargs)
        return real_popen(argv, *positional, **kwargs)

    monkeypatch.setattr(agy.subprocess, "Popen", _popen)


@pytest.mark.dev_build
def test_mint_reads_the_conversation_id_and_pins_the_argv(monkeypatch, tmp_path, rust_door):
    seen: dict = {}

    def responder(argv, kwargs):
        seen["argv"] = argv
        seen["cwd"] = kwargs.get("cwd")
        seen["proc"] = _FakeAgy([INIT_LINE], hold=True)
        return seen["proc"]

    _agy_only_popen(monkeypatch, responder)

    assert agy.create_conversation(tmp_path) == ENVELOPE["conversation_id"]
    # `-p` is agy's print form; stream-json's init line carries the id; the
    # bypass keeps an unattended mint from wedging on a tool approval.
    assert seen["argv"][:2] == ["agy", "-p"]
    assert "--output-format" in seen["argv"] and "stream-json" in seen["argv"]
    assert "--dangerously-skip-permissions" in seen["argv"]
    assert seen["cwd"] == str(tmp_path)
    # The 1.3.3 print turn never exits on its own: a found id terminates it.
    assert seen["proc"].terminated


@pytest.mark.dev_build
def test_mint_carries_the_spawn_axes(monkeypatch, tmp_path, rust_door):
    """The mint turn launches on the spawn's selected model, effort and
    permission posture - not the harness defaults (the audit's A4: the first
    turn of every agy thread ran on whatever the binary defaulted to)."""
    seen: dict = {}

    def responder(argv, kwargs):
        seen["argv"] = argv
        return _FakeAgy([INIT_LINE])

    _agy_only_popen(monkeypatch, responder)

    agy.create_conversation(
        tmp_path, model="m-1", effort="high", permission_mode="accept-edits"
    )
    argv = seen["argv"]
    assert argv[argv.index("--model") + 1] == "m-1"
    assert argv[argv.index("--effort") + 1] == "high"
    assert argv[argv.index("--mode") + 1] == "accept-edits"
    assert "--dangerously-skip-permissions" not in argv


@pytest.mark.parametrize(
    "proc_factory, fragment",
    [
        (lambda: _FakeAgy([], returncode=1), "exited 1"),
        (lambda: _FakeAgy(["Welcome to the Antigravity CLI"]), "was not JSON"),
        (lambda: _FakeAgy(['{"status": "SUCCESS"}']), "no conversation_id"),
        (
            lambda: _FakeAgy(['{"conversation_id": "c5661b28"}']),
            "not a full UUID",
        ),
    ],
)
@pytest.mark.dev_build
def test_every_unreadable_mint_refuses(monkeypatch, tmp_path, proc_factory, fragment, rust_door):
    _agy_only_popen(monkeypatch, lambda argv, kwargs: proc_factory())

    with pytest.raises(agy.AgySessionError) as caught:
        agy.create_conversation(tmp_path)
    assert fragment in str(caught.value)


@pytest.mark.dev_build
def test_a_timed_out_mint_refuses_rather_than_inventing_an_id(monkeypatch, tmp_path, rust_door):
    proc = _FakeAgy(["harness chatter"], hold=True)
    _agy_only_popen(monkeypatch, lambda argv, kwargs: proc)

    with pytest.raises(agy.AgySessionError) as caught:
        agy.create_conversation(tmp_path, timeout_s=0.3)
    assert "minted no conversation id" in str(caught.value)
    # A wedged mint is never left behind.
    assert proc.terminated


@pytest.mark.dev_build
def test_a_holding_turn_yields_the_id_and_is_terminated(monkeypatch, tmp_path, rust_door):
    """The measured 1.3.3 shape: the turn prints the init line then the Stop
    hook holds the process alive; the mint reads the id and terminates it."""
    proc = _FakeAgy([INIT_LINE], hold=True)
    _agy_only_popen(monkeypatch, lambda argv, kwargs: proc)

    assert agy.create_conversation(tmp_path) == ENVELOPE["conversation_id"]
    assert proc.terminated and proc.waited


def test_store_path_names_agys_own_conversation_db():
    path = agy.conversation_store_path(ENVELOPE["conversation_id"])
    assert path.name == f"{ENVELOPE['conversation_id']}.db"
    assert path.parent.parts[-3:] == (".gemini", "antigravity-cli", "conversations")
