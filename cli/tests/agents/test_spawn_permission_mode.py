"""`--permission-mode` passthrough across the spawn surfaces (x-dfa4).

Covers the per-provider mapping table (accepted value -> exact argv; unmappable
-> fail-closed error), the claude `--yolo` -> bypassPermissions fix, the
`--permission-mode`/`--yolo` mutual exclusion. No test performs a live spawn:
the pane dispatch is stubbed.
"""

from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.agents.dispatch import DispatchAskError
from fno.agents.mux_spawn import build_pane_argv, permission_pane_tokens

CWD = Path("/tmp")


# --- mapping table (permission_pane_tokens) --------------------------------


def test_mapping_fail_closed_on_unmappable(rust_door):
    # A colon form needs both axes; a half form must refuse, never guess.
    with pytest.raises(DispatchAskError) as exc:
        permission_pane_tokens("codex", "workspace-write")
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


# --- promoted pane order -----------------------------------------------------
# The lead's precedence condition: an operator flag always outranks
# the role default; no flag means the resolver applies the role default.


@pytest.mark.dev_build
def test_promoted_pane_launches_full_access_without_a_flag(rust_door, monkeypatch, tmp_path):
    """A lead rank on the spawn swaps the bounded pane default for the
    full-access bypass - the role default, answered by the same Rust owner."""
    monkeypatch.setattr("fno.agents.mux_spawn.worker_writable_dirs", lambda *a, **k: [])
    argv = build_pane_argv("codex", "hi", tmp_path, False, None, None, role_level=1)
    assert "--dangerously-bypass-approvals-and-sandbox" in argv
    assert "--sandbox" not in argv


@pytest.mark.dev_build
def test_promoted_pane_explicit_mode_outranks(rust_door):
    """The operator's mode wins over the rank, in both directions of the
    widening: a bounded mode on a promoted pane lands bounded."""
    tokens = permission_pane_tokens("codex", "workspace-write:never", role_level=1)
    assert tokens == [
        "--sandbox",
        "workspace-write",
        "--ask-for-approval",
        "never",
        "-c",
        "sandbox_workspace_write.network_access=true",
    ]


# --- the posture door: this checkout's fno-agents build -------------------


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


# --- build_pane_argv integration -------------------------------------------



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



# --- bash surface: /agent spawn (spawn.sh) ----------------------------------




# --- x-6863: one vocabulary, in Rust; the Python holds no second table ------
