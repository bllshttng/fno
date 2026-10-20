"""Unit tests for the `fno agents spawn-guard` verb (x-73cc).

The verb is the single source of truth for the bg-dispatch mutex shared by
`/target bg` (dispatch-node.sh) and `/agent spawn` (spawn.sh): Guard 1 (the
node:<id> claim probe, fail-closed) + Guard 2 (the create-only dispatch:<id>
reservation). These tests root all claims under a tmp dir so they never touch
real ~/.fno do state.

Acceptance criteria mapped:
  AC1-HP   dispatchable verdict acquires the reservation
  AC1-ERR  a crashing probe fails closed (verdict=error, non-zero exit)
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
    # x-5c08 deliberately emits a loud contested-claim warning on stderr; the
    # JSON stdout contract remains one parseable object for shell callers.
    obj = json.loads(res.stdout)
    assert obj["verdict"] == "already-running"
    assert obj["reason"] == "suspect-claim"
    assert obj["holder"] == "target-session:respawned"
    assert "WARNING: dispatch blocked for x-9999" in res.stderr
    # No reservation was taken - the node stays for the live worker.
    assert claim_status("dispatch:x-9999")["state"] == "free"


# --- AC2-EDGE: racing dispatcher serializes ----------------------------------


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
                "holder": "target-session:prior",
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
        ["spawn", "--name", "failed-worker", "--node", "x-fail", "--here", "/fix work", "--split", "left"],
    )

    assert res.exit_code == 7
    assert released == [("dispatch:x-fail", "spawn-cli:test")]


# --- AC1-ERR: a crashing probe fails closed ----------------------------------


def test_probe_crash_fails_closed(claims_tmp, monkeypatch):
    def _boom(*a, **k):
        raise RuntimeError("simulated probe crash")

    monkeypatch.setattr("fno.claims.core.claim_status", _boom)
    res = _invoke("x-3333", "--holder", "h", "--json")
    assert res.exit_code != 0  # fail-closed -> non-zero
    obj = json.loads(res.output)
    assert obj["verdict"] == "error"
    assert "not dispatching" in obj["detail"]
    # No reservation leaked.
    assert claim_status("dispatch:x-3333")["state"] == "free"


# --- fail-closed on a malformed --ttl (broadened acquire except; gemini review) ---


def test_malformed_ttl_fails_closed(claims_tmp):
    # _parse_ttl raises ValueError on a bad TTL; the verb must fail CLOSED
    # (verdict=error, non-zero) rather than trace out, so the caller refuses.
    res = _invoke("x-4444", "--holder", "h", "--ttl", "not-a-ttl", "--json")
    assert res.exit_code != 0
    obj = json.loads(res.output)
    assert obj["verdict"] == "error"
    # No reservation leaked.
    assert claim_status("dispatch:x-4444")["state"] == "free"


# --- x-2fe6: a held claim proves a HOLDER, never a WORKER --------------------
#
# AC6-HP / AC7-HP / AC8-HP / AC9-EDGE. Both directions in every pair, so the
# discriminator is proven to discriminate rather than to answer one way.


def test_a_worked_authority_outage_names_itself_not_a_running_worker(
    claims_tmp, monkeypatch: pytest.MonkeyPatch
):
    """When the worked overlay refuses, a FREE claim must not render as
    `already-running`: the guard could not check, and that is not occupancy
    (three free nodes were refused as already-running while their leads fell
    back to a costlier lane). The verdict says unknown and the detail carries
    the reader failure verbatim."""
    _native_worker_observation(monkeypatch, "", worked_error="roster timeout")
    res = _invoke("x-9999", "--holder", "probe:1", "--no-reserve", "--json")
    assert res.exit_code == 0
    obj = json.loads(res.output)
    assert obj["verdict"] == "unknown"
    assert obj["reason"] == "worked-authority-unavailable"
    assert "roster timeout" in obj["detail"]


# --- x-c08a: the refusal names the worker ROW, never the claim ----------------


def _native_worker_observation(monkeypatch, worker, *, worked_error=None, warning=None):
    from fno import rust_binary

    call = rust_binary.verb_call

    def observe(verb, payload, *args, **kwargs):
        if verb == ["backlog", "advance", "--stdin"] and payload.get("op") == "claim":
            return {"holder": "unknown", "truth_status": "unknown", "worker": worker,
                    "action": "blocked", "blocks_dispatch": True,
                    "block_reason": "worked-authority-unavailable" if worked_error else f"held: worked overlay: {worker}",
                    "verdict": "dead_predecessor", "worked_error": worked_error,
                    "warning": warning}
        return call(verb, payload, *args, **kwargs)

    monkeypatch.setattr(rust_binary, "verb_call", observe)


def test_a_stale_claim_with_a_worker_row_names_the_row_not_the_claim(
    claims_tmp, monkeypatch: pytest.MonkeyPatch
):
    """AC3-HP, the x-6f98 specimen: the claim read stale with a dead
    predecessor while a worker row sat on the node. The old receipt said
    `unproven-claim` and named the claim's prior holder, and the operator
    followed it to a claim `fno agents claim status` read as UNCLAIMED."""
    import psutil

    from fno.claims.io import global_claims_root as _global_claims_root
    from tests._table_seed import update_claim

    dead_pid = 999_999
    while psutil.pid_exists(dead_pid):
        dead_pid += 1
    acquire_claim(
        "node:x-6f98", "spawn-handover:bp-6f98-locked-decision",
        pid=dead_pid, ttl_ms=60_000,
    )
    update_claim("node:x-6f98", expires_at=1)
    assert claim_status("node:x-6f98", root=_global_claims_root())["state"] == "stale"

    _native_worker_observation(monkeypatch, "bp-6f98-locked-decision")
    res = _invoke("x-6f98", "--holder", "probe:1", "--no-reserve", "--json")
    assert res.exit_code == 0
    obj = json.loads(res.stdout)
    assert obj["verdict"] == "already-running"
    assert obj["reason"] == "worker-row"
    assert obj["worker"] == "bp-6f98-locked-decision"
    assert "fno agents peek bp-6f98-locked-decision" in obj["remedy"]
    # The claim's prior holder is NOT the occupant, so it is not on the receipt.
    assert "holder" not in obj
    assert "bp-6f98-locked-decision" not in json.dumps(obj.get("prior_holder", ""))
