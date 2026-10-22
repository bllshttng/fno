"""Unit tests for the `fno agents spawn-guard` command (x-73cc).

The command is the single source of truth for the bg-dispatch mutex shared by
`/target bg` (dispatch-node.sh) and `/agent spawn` (spawn.sh): Guard 1 (the
node:<id> claim probe, fail-closed) + Guard 2 (the create-only dispatch:<id>
reservation). The Rust guard decides, so the command tests need the dev build.
These tests root all claims under a tmp dir so they never touch real
~/.fno do state.

Acceptance criteria mapped:
  AC1-HP   dispatchable verdict acquires the reservation
  AC1-UI   the verdict is exactly one parseable line / JSON object
  AC1-EDGE live and corrupted claims -> already-running / corrupted, no reservation
  AC1-FR   on dispatchable the reservation is held by --holder (caller releases)
  AC2-EDGE a racing dispatcher (different holder) gets already-running
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.agents.cli import agents_app
from fno.claims.core import acquire_claim, claim_status

runner = CliRunner()


@pytest.fixture
def claims_tmp(tmp_path: Path, monkeypatch):
    """Root BOTH node:<id> (global root) and dispatch:<id> (cwd/env root) claims
    under one tmp dir and pin the Python runtime so the spawn never
    execs the Rust binary."""
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    return tmp_path


def _invoke(*args: str):
    return runner.invoke(agents_app, ["spawn-guard", *args])


# --- AC1-HP / AC1-FR: dispatchable acquires the reservation ------------------


@pytest.mark.dev_build
def test_dispatchable_free_node_acquires_reservation(claims_tmp):
    res = _invoke("x-aaaa", "--holder", "dispatch-node:111", "--json")
    assert res.exit_code == 0
    obj = json.loads(res.output)
    assert obj["verdict"] == "dispatchable"
    assert obj["reservation_key"] == "dispatch:x-aaaa"
    assert obj["reservation_holder"] == "dispatch-node:111"
    # AC1-FR: the reservation is genuinely held by the caller now.
    status = claim_status("dispatch:x-aaaa")
    assert status["state"] in ("live", "stale")
    assert status["holder"] == "dispatch-node:111"


# --- AC1-EDGE: live + corrupted -> no reservation ----------------------------


@pytest.mark.dev_build
def test_live_claim_already_running_no_reservation(claims_tmp):
    import os

    # A live node:<id> claim held by the running test process.
    acquire_claim("node:x-cccc", "target-session:owner", pid=os.getpid())
    res = _invoke("x-cccc", "--holder", "dispatch-node:222", "--json")
    assert res.exit_code == 0
    obj = json.loads(res.output)
    assert obj["verdict"] == "already-running"
    assert obj["reason"] == "live-claim"
    assert obj["holder"] == "target-session:owner"
    # No reservation was taken.
    assert claim_status("dispatch:x-cccc")["state"] == "free"


@pytest.mark.dev_build
def test_corrupted_claim_verdict_no_reservation(claims_tmp):
    # An unreadable row at node:<id> classifies corrupted (claim_status
    # returns state=corrupted, never raises).
    from tests._table_seed import update_claim

    acquire_claim("node:x-dddd", "h0")
    update_claim("node:x-dddd", schema_version=999)

    res = _invoke("x-dddd", "--holder", "h", "--json")
    assert res.exit_code == 0  # corrupted is a clean verdict
    obj = json.loads(res.output)
    assert obj["verdict"] == "corrupted"
    assert "force-release or repair" in obj["detail"]
    assert claim_status("dispatch:x-dddd")["state"] == "free"


# --- x-ba4b: suspect claim -> skip-not-steal ---------------------------------


@pytest.mark.dev_build
def test_suspect_claim_already_running_no_reservation(claims_tmp):
    """A TTL-unexpired claim with a dead pid (respawned worker) reads suspect;
    spawn-guard must report already-running/reason=suspect-claim and take NO
    reservation, so the dispatcher maps it to skipped-contested and never steals."""
    import psutil

    dead_pid = 999_999
    while psutil.pid_exists(dead_pid):
        dead_pid += 1
    # TTL unexpired + dead pid -> classify() == suspect.
    acquire_claim(
        "node:x-9999", "target-session:respawned", pid=dead_pid, ttl_ms=180_000
    )
    assert claim_status("node:x-9999")["state"] == "suspect"

    res = _invoke("x-9999", "--holder", "dispatch-node:333", "--json")
    assert res.exit_code == 0
    obj = json.loads(res.stdout)
    assert obj["verdict"] == "already-running"
    assert obj["reason"] == "suspect-claim"
    assert obj["holder"] == "target-session:respawned"
    # No reservation was taken - the node stays for the live worker.
    assert claim_status("dispatch:x-9999")["state"] == "free"


# --- AC2-EDGE: racing dispatcher serializes ----------------------------------


@pytest.mark.dev_build
def test_racing_reservation_already_running(claims_tmp):
    import os

    # A peer dispatcher already holds dispatch:<id> (live).
    acquire_claim("dispatch:x-eeee", "dispatch-node:peer", pid=os.getpid(), ttl_ms=180_000)
    res = _invoke("x-eeee", "--holder", "dispatch-node:me", "--json")
    assert res.exit_code == 0
    obj = json.loads(res.output)
    assert obj["verdict"] == "already-running"
    assert obj["reason"] == "reservation-held"
    # The peer still owns the reservation; we did not steal it.
    assert claim_status("dispatch:x-eeee")["holder"] == "dispatch-node:peer"


# --- --no-reserve: Guard 1 only ----------------------------------------------


def test_direct_node_spawn_crosses_shared_guard_before_spawn(
    claims_tmp, monkeypatch: pytest.MonkeyPatch
):
    """A direct `fno agents spawn --node` cannot bypass the shell guard."""
    import fno.agents.cli as cli_mod
    from fno.agents import mux_spawn, spawn_gate

    monkeypatch.setattr(
        cli_mod,
        "_spawn_guard_decision",
        lambda *_a, **_k: (
            {
                "verdict": "refused",
                "reason": "auto-deferred",
                "refusal_line": (
                    "node dispatch refused: node=x-dead verdict=refused "
                    "reason=auto-deferred; no worker launched"
                ),
            },
            0,
        ),
    )
    monkeypatch.setattr(
        mux_spawn,
        "resolve_provenance",
        lambda *_a, **_k: {"FNO_NODE": "x-dead"},
    )
    monkeypatch.setattr(
        spawn_gate,
        "run_gate",
        lambda *_a, **_k: pytest.fail("guard refusal must precede the spawn gate"),
    )

    res = runner.invoke(
        agents_app,
        ["spawn", "--name", "dead-worker", "--node", "x-dead", "--here", "/fix work"],
    )

    assert res.exit_code == 2
    assert "reason=auto-deferred" in res.output
    assert "no worker launched" in res.output


def test_direct_node_spawn_failure_releases_shared_reservation(
    claims_tmp, monkeypatch: pytest.MonkeyPatch
):
    """A substrate failure leaves a node immediately re-dispatchable."""
    import fno.agents.cli as cli_mod
    from fno.agents import mux_spawn, spawn_gate
    from fno.agents.dispatch import DispatchAskError

    monkeypatch.setattr(
        cli_mod,
        "_spawn_guard_decision",
        lambda *_a, **_k: (
            {
                "verdict": "dispatchable",
                "reservation_key": "dispatch:x-fail",
                "reservation_holder": "spawn-cli:test",
            },
            0,
        ),
    )
    monkeypatch.setattr(
        mux_spawn,
        "resolve_provenance",
        lambda *_a, **_k: {"FNO_NODE": "x-fail"},
    )
    monkeypatch.setattr(
        spawn_gate, "run_gate", lambda *_a, **_k: type("G", (), {"release": lambda self: None})()
    )
    monkeypatch.setattr(
        mux_spawn,
        "dispatch_spawn_bounded_pane",
        lambda **_k: (_ for _ in ()).throw(DispatchAskError("mux failed", exit_code=7)),
    )
    released: list[tuple[str, str]] = []
    monkeypatch.setattr(
        "fno.claims.core.release_claim",
        lambda key, holder, **_k: released.append((key, holder)),
    )

    res = runner.invoke(
        agents_app,
        ["spawn", "--name", "failed-worker", "--node", "x-fail", "--here", "/fix work", "--as", "pane", "--split", "left"],
    )

    assert res.exit_code == 7
    assert released == [("dispatch:x-fail", "spawn-cli:test")]


# --- fail-closed on a malformed --ttl (broadened acquire except; gemini review) ---


@pytest.mark.dev_build
def test_malformed_ttl_fails_closed(claims_tmp):
    # _parse_ttl raises ValueError on a bad TTL; the command must fail CLOSED
    # (verdict=error, non-zero) rather than trace out, so the caller refuses.
    res = _invoke("x-4444", "--holder", "h", "--ttl", "not-a-ttl", "--json")
    assert res.exit_code != 0
    obj = json.loads(res.output)
    assert obj["verdict"] == "error"
    # No reservation leaked.
    assert claim_status("dispatch:x-4444")["state"] == "free"
