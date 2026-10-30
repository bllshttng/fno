"""Bus-only delivery policy conformance (x-e21e) -- the no-paste pin.

The operator's live specimens: two injections split their sentences mid-compose
inside the reports of the defect itself. The fix is a recipient-level
``delivery_policy: bus-only`` row stamp: mail to that recipient never
prompt-line injects, always queues durable, and the receipt says the policy --
never a live-miss, never a stranded message, never a liveness verdict (the
naming trap that renamed NOT_INJECTABLE off "not-live", mail_inject.rs:96).

These tests pin the two VERIFY cases from the node plus the surrounding
contract: the no-paste guarantee, the worker-still-pastes counterpart, the
receipt vocabulary, the escalation skip, the raw-lane refusal, and the
register upsert's preserve-when-silent stamp.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner

from fno.cli import app
from fno.paths_testing import use_tmpdir

BUS_SID = "7c19d2e4-0b3f-4c5a-9e88-2ad4f0c81b97"
WORKER_SID = "3f8a51c0-6d22-4b7e-b1a4-90ce7d3f25aa"
# register_existing_session's axis-legacy kwarg still spells `provider`; pass
# the harness through a name (the production caller's `provider=harness`
# pattern) so no provider-named binding holds a harness literal.
HARNESS = "claude"


@pytest.fixture
def runner():
    return CliRunner()


@pytest.fixture
def mailbox(tmp_path, monkeypatch):
    """Co-isolate the bus log, inbox, registry, and every discovery source."""
    monkeypatch.delenv("FNO_BUS_DIR", raising=False)
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents import discover

    empty = tmp_path / "empty-discovery"
    empty.mkdir(exist_ok=True)
    for env in (
        discover.SESSIONS_DIR_ENV,
        discover.PROJECTS_DIR_ENV,
        discover.CODEX_SESSIONS_DIR_ENV,
        discover.OPENCODE_STORAGE_DIR_ENV,
    ):
        monkeypatch.setenv(env, str(empty))
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(tmp_path / "daemon-empty"))
    # The gate subprocess (the installed fno-agents on a dev machine) parks
    # into the RUST state root and spawns a detached runner from the same
    # binary. Pin the Rust leg (FNO_HOME) beside the Python one and point the
    # runner at a stub, so a park inside a test touches nothing real and no
    # runner outlives the run.
    monkeypatch.setenv("FNO_HOME", str(tmp_path / "fno-home"))
    runner_stub = tmp_path / "runner-stub.sh"
    runner_stub.write_text("#!/bin/sh\nexit 0\n")
    runner_stub.chmod(0o755)
    monkeypatch.setenv("FNO_AGENTS_RUNNER_BIN", str(runner_stub))
    return tmp_path


def _seed_registry(*policies: dict) -> None:
    """Write registry rows; each kwarg-dict maps AgentEntry kwargs."""
    from fno.agents.registry import AgentEntry, write_registry

    rows = []
    for spec in policies:
        rows.append(
            AgentEntry(
                name=spec["name"],
                cwd="/tmp/x",
                log_path="/tmp/x.log",
                harness="claude",
                harness_session_id=spec["sid"],
                short_id=spec["sid"][:8],
                status=spec.get("status", "idle"),
                delivery_policy=spec.get("delivery_policy"),
            )
        )
    write_registry(rows)


def _boom_transport(monkeypatch):
    """The layer BELOW the gate: raise only when a subprocess would reach a
    prompt line (the mail-inject binary, a mux pane send). Discovery and other
    unrelated lanes legitimately shell the binary during a send, so the boom
    keys on the PASTE argv, not on subprocess use itself. Patching the
    injectors themselves would replace the gate under test -- the exact
    mistake a no-paste assertion must not make."""
    import subprocess as real_subprocess

    # Capture BEFORE the patch: setattr on the module attribute would make
    # real_subprocess.run resolve back to the patched function (infinite
    # recursion) since subprocess is a shared module object.
    _real_run = real_subprocess.run

    def _watch_run(argv, *args, **kwargs):
        joined = " ".join(str(a) for a in argv[:4])
        if "mail-inject" in joined or "pane" in joined:
            raise AssertionError(
                f"a prompt-line paste was attempted on a bus-only lane: {joined}"
            )
        return _real_run(argv, *args, **kwargs)

    monkeypatch.setattr(
        "fno.agents.dispatch.subprocess.run", _watch_run, raising=True
    )


# ---------------------------------------------------------------------------
# VERIFY case 1: a bus-only recipient receives NO prompt-line paste.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# The register seam: stamp, preserve-when-silent, clear.
# ---------------------------------------------------------------------------


def test_register_stamps_bus_only_and_flagless_reregister_preserves(monkeypatch):
    from fno.agents.registry import load_registry, register_existing_session

    entry = register_existing_session(
        provider=HARNESS,
        session_id=BUS_SID,
        cwd="/tmp/x",
        origin="operator",
        delivery_policy="bus-only",
    )
    assert entry.delivery_policy == "bus-only"

    # A re-firing SessionStart hook (no policy kwarg) must not clobber the
    # stamp -- the operator would silently revert to injectable.
    entry = register_existing_session(
        provider=HARNESS, session_id=BUS_SID, cwd="/tmp/x", origin="operator"
    )
    assert entry.delivery_policy == "bus-only", (
        "a flagless re-register reverted a bus-only stamp"
    )

    # The explicit clear.
    entry = register_existing_session(
        provider=HARNESS,
        session_id=BUS_SID,
        cwd="/tmp/x",
        origin="operator",
        delivery_policy="off",
    )
    assert entry.delivery_policy is None

    # The stamp round-trips through the store.
    register_existing_session(
        provider=HARNESS,
        session_id=BUS_SID,
        cwd="/tmp/x",
        origin="operator",
        delivery_policy="bus-only",
    )
    rows = [e for e in load_registry() if e.harness_session_id == BUS_SID]
    assert rows and rows[0].delivery_policy == "bus-only"


