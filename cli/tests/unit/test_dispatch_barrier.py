"""Claim-visibility barrier at the spawn-guard choke point (x-a7ab 1.2 / x-b44e).

spawn-guard is the single source of truth for the dispatch:<id> reservation,
called by both spawn.sh and dispatch-node.sh. After acquiring the reservation it
re-reads the claims dir to confirm THIS holder is the one on disk before
returning dispatchable; a peer that won a visibility-lagged race surfaces as a
different holder and this dispatcher skips with duplicate-claim so exactly one
worker launches.
"""
import json

import pytest
from typer.testing import CliRunner

# Import the claims CLI module BEFORE any test patches fno.claims.core. Its
# module-level `from .core import acquire_claim` binds whatever core carries at
# first import, so a patch that lands first (as xdist worker ordering can do)
# bakes the stub into the CLI module for the rest of the process, and a later
# handover test in the same worker calls the stub instead of the real acquire.
import fno.claims.cli  # noqa: F401

from fno.agents.cli import _spawn_guard_decision, agents_app

runner = CliRunner()


def _last_json(out: str) -> dict:
    for line in reversed(out.strip().splitlines()):
        line = line.strip()
        if line.startswith("{"):
            return json.loads(line)
    raise AssertionError(f"no JSON verdict in output: {out!r}")


def _route_to(monkeypatch, root):
    # Route every claim key (node:/dispatch:) to an isolated tmp root so the real
    # acquire/claim_status machinery runs hermetically: FNO_CLAIMS_ROOT (the
    # documented global-root override) is the one door. It covers anything that
    # resolves through the env at call time, so a module-identity or import-order
    # pathology can never route a CLI acquire to the real shared root.
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(root))
    # Scrub this process's harness identity, so a fixture acquire stamps no
    # session id: the suite seeds dead-pid holders, and a stamp from the LIVE
    # pytest session lets the classifier's witness leg answer `live` for them.
    from fno.harness_identity import AMBIENT_IDENTITY_ENV

    for name in AMBIENT_IDENTITY_ENV:
        monkeypatch.delenv(name, raising=False)


@pytest.mark.dev_build
def test_spawn_guard_serializes_two_callers(monkeypatch, tmp_path):
    # AC2-HP: two dispatch attempts for one node -> exactly one dispatchable.
    _route_to(monkeypatch, tmp_path)
    r1 = runner.invoke(agents_app, ["spawn-guard", "N", "--holder", "A", "--ttl", "3m", "--json"])
    assert r1.exit_code == 0, r1.output
    assert _last_json(r1.output)["verdict"] == "dispatchable"
    r2 = runner.invoke(agents_app, ["spawn-guard", "N", "--holder", "B", "--ttl", "3m", "--json"])
    v2 = _last_json(r2.output)
    assert v2["verdict"] == "already-running"
    assert v2["reason"] in ("reservation-held", "duplicate-claim")


# ---------------------------------------------------------------------------
# the guard clears what it can prove, and refuses loudly otherwise (x-05be)
# ---------------------------------------------------------------------------


def _dead_pid():
    import psutil

    dead = 999_999
    while psutil.pid_exists(dead):
        dead += 1
    return dead


# ---------------------------------------------------------------------------
# spawn --node takes THE node claim, and the worker inherits it (x-cd1e)
# ---------------------------------------------------------------------------


@pytest.mark.dev_build
def test_the_guard_claims_the_node_key_not_just_the_reservation(monkeypatch, tmp_path):
    """The measured defect: five workers spawned with an explicit --node and
    `fno agents claim status node:<id>` read free for every one of them."""
    _route_to(monkeypatch, tmp_path)
    from fno.claims.core import claim_status

    verdict, _exit = _spawn_guard_decision(
        "N", "spawn-cli:1", ttl="3m", handover_holder="spawn-handover:t-N"
    )
    assert verdict["verdict"] == "dispatchable"
    info = claim_status("node:N", root=tmp_path)
    assert info["state"] in ("live", "suspect"), info
    assert info["holder"] == "spawn-handover:t-N"


@pytest.mark.dev_build
def test_the_worker_inherits_the_claim_without_a_refusal(monkeypatch, tmp_path):
    """x-cd1e case 6. The handover must not abort the worker, and the key must
    never read free in between - the gap is the whole point of claiming early."""
    _route_to(monkeypatch, tmp_path)
    import os

    from fno.claims.cli import cli as claims_cli
    from fno.claims.core import claim_status

    _spawn_guard_decision(
        "N", "spawn-cli:1", ttl="3m", handover_holder="spawn-handover:t-N"
    )
    r = runner.invoke(
        claims_cli,
        [
            "acquire", "node:N",
            "--holder", "target-session:sid-1",
            "--ttl", "2h",
            "--pid", str(os.getpid()),
            "--handover-from", "spawn-handover:t-N",
        ],
    )
    assert r.exit_code == 0, r.output
    info = claim_status("node:N", root=tmp_path)
    assert info["holder"] == "target-session:sid-1"
    assert info["state"] == "live"


def test_a_wrong_handover_holder_cannot_take_a_live_claim(monkeypatch, tmp_path):
    """Naming the prior holder is the proof. Guessing wrong falls through to an
    ordinary acquire, which refuses a live foreign claim exactly as today."""
    _route_to(monkeypatch, tmp_path)
    import os

    from fno.claims.cli import cli as claims_cli
    from fno.claims.core import acquire_claim, claim_status

    acquire_claim(
        "node:N", "target-session:incumbent", ttl_ms=3_600_000,
        pid=os.getpid(), root=tmp_path,
    )
    # Pin the intruder to a pid DIFFERENT from the incumbent's. An omitted
    # --pid resolves the ambient harness session, which on an identity-less CI
    # runner degrades to this same process: the intruder then reacquires the
    # claim as its own pid (legal) and the refusal under test never fires.
    # getppid() is alive and distinct inside a pytest worker.
    r = runner.invoke(
        claims_cli,
        [
            "acquire", "node:N",
            "--holder", "target-session:intruder",
            "--ttl", "2h",
            "--pid", str(os.getppid()),
            "--handover-from", "spawn-handover:t-guessed",
        ],
    )
    assert r.exit_code == 1, f"{r.output}\n{_barrier_diag(tmp_path)}"
    status = claim_status("node:N", root=tmp_path)
    assert status.get("holder") == "target-session:incumbent", (
        f"{status}\n{_barrier_diag(tmp_path)}"
    )


def _barrier_diag(tmp_path) -> str:
    """What the failure needs to name: where the CLI's root actually pointed.

    x-c319: under the CI changed-packet the acquire has been observed to win
    where the seeded incumbent should refuse it, and the root the test seeded
    is the first suspect. Resolve the SAME doors the test set up, read the
    value NOW, and put the answer in the assertion message.
    """
    import os

    from fno.claims.io import claim_path
    from fno.claims.io import global_claims_dir

    def _seen(label: str, root) -> str:
        """Does a claim FILE exist where this door points, and what holds it?"""
        try:
            path = claim_path("node:N", root=root)
        except Exception as exc:  # noqa: BLE001 - a diagnostic never raises
            return f"{label}=<unresolvable: {type(exc).__name__}>"
        if not path.exists():
            return f"{label}={path}:ABSENT"
        try:
            from fno.claims.core import claim_status

            info = claim_status("node:N", root=root)
            return f"{label}={path}:holder={info.get('holder')}:state={info.get('state')}"
        except Exception as exc:  # noqa: BLE001
            return f"{label}={path}:PRESENT:<unreadable: {type(exc).__name__}>"

    # The doors, each answered SEPARATELY. The earlier version printed the
    # roots and stopped, so a failure could not say whether the seeded claim was
    # missing, in another directory, or present and read as free - three causes
    # with one appearance. A path plus its holder separates them in one run.
    return (
        f"[diag] global={global_claims_dir()} "
        f"env={os.environ.get('FNO_CLAIMS_ROOT')!r} tmp={tmp_path} "
        + " ".join(
            (
                _seen("seeded", tmp_path),
                _seen("default", None),
            )
        )
    )


@pytest.mark.dev_build
def test_the_bare_guard_command_never_clears_a_barrier_it_cannot_replace(
    monkeypatch, tmp_path
):
    """`fno agents spawn-guard` takes no handover holder, so it never takes the
    node claim. Clearing a dead spawner's reservation there removed a booting
    worker's only barrier and put nothing in its place. The clear is legitimate
    only for the caller that replaces it."""
    _route_to(monkeypatch, tmp_path)
    from fno.claims.core import acquire_claim, claim_status

    acquire_claim(
        "dispatch:N", "spawn-cli:99", ttl_ms=180_000, pid=_dead_pid(), root=tmp_path
    )
    verdict, exit_code = _spawn_guard_decision("N", "spawn-cli:me", ttl="3m")
    assert verdict["verdict"] == "already-running"
    assert verdict["reason"] == "reservation-held"
    # The positive marker: the reservation is still on disk, holder unchanged.
    assert claim_status("dispatch:N", root=tmp_path)["holder"] == "spawn-cli:99"
    assert exit_code == 0


def test_a_guard_timeout_names_its_cause(monkeypatch):
    """A guard that never answered under a slow shared primary printed
    reason=error, which named nothing an operator could check."""
    import subprocess

    from fno import rust_binary

    def _times_out(*_args, **_kwargs):
        try:
            raise subprocess.TimeoutExpired(["fno-agents"], 30)
        except subprocess.TimeoutExpired as exc:
            raise rust_binary.CommandUnavailable(f"fno-agents failed: {exc}") from exc

    monkeypatch.setattr(rust_binary, "command_call", _times_out)
    verdict, exit_code = _spawn_guard_decision("N", "spawn-cli:me")
    assert exit_code == 3
    assert " reason=guard-timeout; " in verdict["refusal_line"]
    assert "store.remote_url" in verdict["detail"]


@pytest.mark.dev_build
def test_a_held_node_refuses_and_hands_back_its_own_reservation(monkeypatch, tmp_path):
    """A session that claimed the node through its own `fno do target init` is
    invisible to the reservation, which only dedups other DISPATCHERS. Swallowing
    that as a best-effort hiccup put a second worker on a live session's node.

    And the refusal must not keep the reservation it just took: nothing
    downstream releases it, and a `dispatch:` key is unreapable inside its TTL,
    so it would block every dispatcher for the full three minutes."""
    _route_to(monkeypatch, tmp_path)
    import os

    from fno.claims.core import acquire_claim, claim_status

    acquire_claim(
        "node:N", "target-session:someone-else", ttl_ms=3_600_000,
        pid=os.getpid(), root=tmp_path,
    )
    verdict, exit_code = _spawn_guard_decision(
        "N", "spawn-cli:me", ttl="3m", handover_holder="spawn-handover:t-N"
    )
    assert verdict["verdict"] == "already-running"
    assert verdict["reason"] == "live-claim"
    assert exit_code == 0
    assert (
        claim_status("node:N", root=tmp_path)["holder"] == "target-session:someone-else"
    )
    assert claim_status("dispatch:N", root=tmp_path)["state"] == "free"


# --- x-2fe6: the reservation is taken after the launch is proven, and released
# --- on an observation every substrate reaches ----------------------------


def _spawn_env(monkeypatch, tmp_path, *, node="x-abcd"):
    """Route claims to TMP_PATH and stub everything cmd_spawn touches downstream."""
    _route_to(monkeypatch, tmp_path)
    from fno.agents import mux_spawn, spawn_gate

    monkeypatch.setattr(
        mux_spawn, "resolve_provenance", lambda n, s, p: {"FNO_NODE": node} if n else {}
    )

    class FakeGuard:
        def release(self):
            pass

    monkeypatch.setattr(spawn_gate, "run_gate", lambda name, substrate, **kw: FakeGuard())
    return FakeGuard


def _spy_guard(monkeypatch):
    """Replace the node guard with a spy that records every call and takes nothing."""
    from fno.agents import cli as agents_cli

    calls: list[tuple] = []

    def spy(*a, **k):
        calls.append((a, k))
        return {
            "verdict": "dispatchable",
            "reservation_key": "dispatch:x-abcd",
            "reservation_holder": "spawn-cli:spy",
        }, 0

    monkeypatch.setattr(agents_cli, "_spawn_guard_decision", spy)
    return calls


def test_a_resume_registry_failure_never_took_the_reservation(monkeypatch, tmp_path):
    """AC1-HP: the resume-provider exit runs BEFORE the node guard.

    The registry read raising used to exit 2 with `dispatch:<id>` already held
    by a pid that was gone, stranding the node for the whole TTL with nothing
    launched. The spy is the positive marker: a free claim alone cannot tell
    "released" from "never taken".
    """
    _spawn_env(monkeypatch, tmp_path)
    from fno.claims.core import claim_status

    guard_calls = _spy_guard(monkeypatch)

    def boom():
        raise RuntimeError("registry unreadable")

    monkeypatch.setattr("fno.agents.registry.load_registry", boom)

    result = runner.invoke(
        agents_app,
        [
            "spawn", "--name", "w1", "/fix hi",
            "--harness", "claude", "--substrate", "thread",
            "--node", "x-abcd", "--resume", "11111111-2222-3333-4444-555555555555",
        ],
    )
    assert result.exit_code == 2, result.output
    assert guard_calls == []
    assert claim_status("dispatch:x-abcd", root=tmp_path)["state"] == "free"
    assert claim_status("node:x-abcd", root=tmp_path)["state"] == "free"


#: The `provider` field on both spawn results carries the HARNESS axis, not a
#: model provider: `cmd_spawn` renders it as the receipt's "harness" key. Bind
#: the value to a harness-named constant so these fixtures name the axis the
#: value belongs to rather than restating the field's misnomer.
_FIXTURE_HARNESS = "claude"


@pytest.mark.dev_build
def test_a_pane_spawn_still_holds_the_reservation_for_its_worker(
    monkeypatch, tmp_path
):
    """AC5-HP: the fix is a SPLIT, not a blanket release.

    A pane worker outlives this process, so its reservation must stay held or
    the next dispatcher launches a second worker onto the same node.
    """
    _spawn_env(monkeypatch, tmp_path)
    from fno.agents import mux_spawn
    from fno.agents.mux_spawn import MuxSpawnResult
    from fno.claims.core import claim_status

    monkeypatch.setattr(
        mux_spawn,
        "dispatch_spawn_bounded_pane",
        lambda **kw: MuxSpawnResult(
            name="w1",
            provider=_FIXTURE_HARNESS,
            session="s0",
            pane_id=1,
            child_pid=None,
            session_uuid="11111111-2222-3333-4444-555555555555",
            short_id="abcd1234",
            bound=True,
        ),
    )
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "/fix hi", "--harness", "claude",
         "--as", "pane", "--split", "left", "--node", "x-abcd"],
    )
    assert result.exit_code == 0, result.output
    assert claim_status("dispatch:x-abcd", root=tmp_path)["state"] == "live"


def test_a_failed_release_names_the_key_it_could_not_free(monkeypatch, capsys):
    """A release that no-ops must not report the same nothing as one that worked.

    Swallowing the exception is correct: this runs on the way out of a failure
    and raising would mask the error being handled. Saying nothing is not. A
    silently-failed release leaves the key held for its whole TTL under a holder
    that is gone, which is the same wedge the rest of this file deletes.
    """
    from fno.agents import cli as agents_cli

    def boom(key, holder, **kw):
        raise OSError("claims dir is read-only")

    monkeypatch.setattr("fno.claims.core.release_claim", boom)
    agents_cli._release_dispatch_claims(
        ("dispatch:x-abcd", "spawn-cli:1"), ("node:x-abcd", "spawn-handover:w1")
    )
    err = capsys.readouterr().err
    # Both keys named, and the way out named with them.
    assert "dispatch:x-abcd" in err
    assert "node:x-abcd" in err
    assert "stays held until its TTL expires" in err
    assert "fno agents claim release" in err


