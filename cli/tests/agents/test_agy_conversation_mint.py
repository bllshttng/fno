"""agy mints its conversation id through one print-mode turn.

The stream's init line is the ONLY surface that returns the id before the
harness Stop hook holds the process alive, so every way that read can fail
has to raise rather than hand the keeper a name it made up. The live shape
asserted here is the stream-json one measured on agy 1.3.3: line 1 is
``{"event":"init","conversation_id":...}``. The turn runs in the Rust
runtime (crates/fno-agents/src/agy_mint.rs); these tests pin the adapter's
answer mapping and the runtime's reader through the real binary.
"""
from __future__ import annotations

import json
import os

import pytest

from fno.agents.harnesses import agy
from fno.agents.spawn_axes_client import SpawnAxesUnavailable
from fno.agents.dispatch import DispatchAskError


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


def _fake_agy_on_path(monkeypatch, tmp_path, body: str) -> None:
    """A PATH fake for the agy binary: the runtime resolves argv[0] through
    PATH, so a script pins the reader's real behavior end-to-end."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    script = bin_dir / "agy"
    script.write_text(f"#!/bin/sh\n{body}\n")
    script.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bin_dir}:{os.environ['PATH']}")


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


def _seam(monkeypatch, answer: dict) -> None:
    """Pin the spawn-axes seam: the adapter reads its answer through the same
    spawn_axes_call every other ask rides on."""
    import fno.agents.spawn_axes_client as sac

    def _call(payload):
        return answer

    monkeypatch.setattr(sac, "spawn_axes_call", _call)


@pytest.mark.dev_build
def test_the_runtime_reads_the_init_line_of_a_holder(monkeypatch, tmp_path, rust_door):
    """End-to-end through the real binary: the fake agy prints the init line
    then holds; the runtime kills it and answers the id."""
    _fake_agy_on_path(
        monkeypatch,
        tmp_path,
        "printf '%s\\n' '" + INIT_LINE + "'\nsleep 30",
    )
    assert agy.create_conversation(tmp_path) == ENVELOPE["conversation_id"]


@pytest.mark.dev_build
def test_a_holding_turn_refuses_at_the_ceiling(monkeypatch, tmp_path, rust_door):
    _fake_agy_on_path(monkeypatch, tmp_path, "sleep 30")
    with pytest.raises(DispatchAskError) as caught:
        agy.create_conversation(tmp_path, timeout_s=0.3)
    assert "minted no conversation id" in str(caught.value)


def test_the_adapter_answers_a_validated_id(monkeypatch, tmp_path):
    _seam(monkeypatch, {"conversation_id": ENVELOPE["conversation_id"]})
    assert agy.create_conversation(tmp_path) == ENVELOPE["conversation_id"]


def test_a_refused_mint_names_the_reason(monkeypatch, tmp_path):
    _seam(
        monkeypatch,
        {
            "refused": "agy exited 1 while minting a conversation id: "
            "not signed in"
        },
    )
    with pytest.raises(DispatchAskError) as caught:
        agy.create_conversation(tmp_path)
    assert "not signed in" in str(caught.value)
    assert "exited 1" in str(caught.value)


def test_a_stale_binary_reads_as_unavailable(monkeypatch, tmp_path):
    _seam(monkeypatch, {})
    with pytest.raises(SpawnAxesUnavailable) as caught:
        agy.create_conversation(tmp_path)
    assert "conversation_id" in str(caught.value)


def test_a_truncated_id_never_reaches_a_spawn(monkeypatch, tmp_path):
    _seam(monkeypatch, {"conversation_id": "c5661b28"})
    with pytest.raises(agy.AgySessionError) as caught:
        agy.create_conversation(tmp_path)
    assert "not a full UUID" in str(caught.value)


def test_store_path_names_agys_own_conversation_db():
    path = agy.conversation_store_path(ENVELOPE["conversation_id"])
    assert path.name == f"{ENVELOPE['conversation_id']}.db"
    assert path.parent.parts[-3:] == (".gemini", "antigravity-cli", "conversations")
