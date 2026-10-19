"""x-9844 Fix 3: `spawn --resume` revives an exited claude row in place.

The name-collision check stops refusing the one case that is a revival: an
exited claude row whose own recorded ``claude_session_uuid`` equals the
``--resume`` target. That row is updated in place (new short_id, same uuid)
instead of refused or duplicated. The row is found by name first, then by the
resumed uuid itself, so a row that answers to another name (an adopted
short-id-named row) revives under the caller's explicit --name and no stale
second row is left behind. Everything else stays fail-closed.

Coverage:
  - ``_is_revival`` gate: dead+uuid-match -> True; live / mismatch / no-resume /
    non-claude -> False. Liveness is the reality probe, never the status field.
  - AC1-HP: spawn --resume against an exited own-name row updates it in place
    (one row, new short_id, same uuid) instead of exiting 2.
  - AC2-EDGE: a uuid mismatch stays a collision (exit 2, row unchanged).
  - AC2-HP: a same-name spawn with no --resume stays a collision.

x-7fef extends this module with the writer-claim LIFETIME contract: the
``session:<uuid>`` claim is taken in the create path for every resume, re-pinned
to the spawned supervisor's pid, and outlives the acquiring process on success
AND on registry-write failure. It is released only when no child was spawned.
"""
from __future__ import annotations

import os
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir
from fno.agents import dispatch
from fno.agents.registry import AgentEntry, load_registry, update_registry

DEAD_UUID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
OTHER_UUID = "ffffffff-1111-2222-3333-444444444444"


@pytest.fixture(autouse=True)
def _admit_spawn_gate(monkeypatch):
    """Hermetic spawn gate: this suite tests revival semantics, not admission.

    The gate is a transport over the fno-agents binary, which a clean checkout
    and the smoke-pytest shard leave unresolved, so every spawn here refused
    exit 87 before reaching the revival paths under test. Sibling spawn suites
    stub the same seam; the gate's own behavior is covered in
    test_spawn_gate_agreement.py. The revival answer rides the same runtime:
    the stub mirrors the binary's predicate (claude, uuid match, not live,
    by-name wins) against the seeded registry, driven by each test's own
    session_is_live stub.
    """
    _admitting_gate(monkeypatch)

    def _stub_revival(name, harness, sid):
        from fno.agents.harnesses import claude as claude_mod
        from fno.agents.registry import load_registry

        rows = load_registry()
        named = next((e for e in rows if e.name == name), None)
        row = named or next(
            (e for e in rows if e.harness_session_id == sid), None
        )
        if row is None:
            return {"revive": False, "by": None}
        live = False
        if row.short_id:
            live = claude_mod.session_is_live(row.short_id)
        revive = (
            harness == "claude"
            and row.harness == "claude"
            and row.harness_session_id == sid
            and not live
        )
        return {"revive": revive, "by": "name" if named else "session"}

    monkeypatch.setattr(dispatch, "_revival_answer", _stub_revival)


@pytest.fixture
def workdir_claude(tmp_path, monkeypatch):
    """Isolated fno home with the fake claude on PATH (emits short_id 7c5dcf5d).
    Mirrors test_spawn_uuid_capture's fixture."""
    from tests.agents._fake_claude import install_fake_claude

    use_tmpdir(monkeypatch, tmp_path)
    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    return tmp_path


def _seed_row(
    name: str, short_id: str, uuid, node: str | None = None,
    requested_model: str | None = "claude-opus-5",
) -> None:
    row = AgentEntry(
        name=name,
        harness="claude",
        cwd="/tmp",
        log_path="/tmp/rev.log",
        short_id=short_id,
        harness_session_id=uuid,
        node=node,
        requested_model=requested_model,
    )
    update_registry(lambda entries: entries + [row])


# ---------------------------------------------------------------------------
# Unit: the _is_revival gate (probes reality, not the status field)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Integration: the CLI spawn path
# ---------------------------------------------------------------------------


def test_spawn_resume_revives_in_place(workdir_claude, monkeypatch) -> None:
    """AC1-HP: spawn --resume against the row's own exited name updates it in
    place - one row, fresh short_id - instead of exiting 2. The row records
    the source uuid as lineage; the live session itself runs under a new id
    (`claude --bg --resume` always forks)."""
    from fno.agents.cli import agents_app
    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setattr(claude_mod, "session_is_live", lambda sid: False)
    _seed_row("rev-agent", "deadbeef", DEAD_UUID)

    result = CliRunner().invoke(
        agents_app,
        ["spawn", "--name", "rev-agent", "-H", "claude", "--resume", DEAD_UUID,
         "--substrate", "thread", "hi"],
        catch_exceptions=False,
    )
    assert result.exit_code == 0, result.output

    rows = [e for e in load_registry() if e.name == "rev-agent"]
    assert len(rows) == 1  # revived in place, not a same-name duplicate
    assert rows[0].short_id == "7c5dcf5d"  # fresh short_id from the new spawn
    # The row keeps the SOURCE id as lineage: `--bg --resume` always forks,
    # so the live session's real uuid is unknowable from the shell.
    assert rows[0].harness_session_id == DEAD_UUID


def test_spawn_resume_uuid_mismatch_is_collision(workdir_claude, monkeypatch) -> None:
    """AC2-EDGE: a --resume uuid that is not the row's own is a collision, not a
    revival; the row is left untouched."""
    from fno.agents.cli import agents_app
    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setattr(claude_mod, "session_is_live", lambda sid: False)
    _seed_row("rev-agent", "deadbeef", DEAD_UUID)

    result = CliRunner().invoke(
        agents_app,
        ["spawn", "--name", "rev-agent", "-H", "claude", "--resume", OTHER_UUID,
         "--substrate", "thread", "hi"],
        catch_exceptions=False,
    )
    assert result.exit_code == 2, result.output
    rows = [e for e in load_registry() if e.name == "rev-agent"]
    assert len(rows) == 1
    assert rows[0].short_id == "deadbeef"  # unchanged


def test_spawn_resume_onto_another_named_row_renames_it(
    workdir_claude, monkeypatch
) -> None:
    """A --resume to a NEW name onto a session that already has a row revives
    THAT row under the caller's name: the row and the harness title agree, the
    node carry survives, and the registry keeps one row for the session (never
    a stale second row the revival left behind)."""
    from fno.agents.cli import agents_app
    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setattr(claude_mod, "session_is_live", lambda sid: False)
    _seed_row("ac-t-source", "deadbeef", DEAD_UUID, node="x-256c")

    result = CliRunner().invoke(
        agents_app,
        ["spawn", "--name", "wake-f8b81903", "-H", "claude", "--resume", DEAD_UUID,
         "--substrate", "thread", "hi"],
        catch_exceptions=False,
    )
    assert result.exit_code == 0, result.output

    rows = load_registry()
    row = next((e for e in rows if e.name == "wake-f8b81903"), None)
    assert row is not None, "the revived row takes the caller's name"
    assert row.node == "x-256c"  # the join a wake must not lose
    assert row.harness_session_id == DEAD_UUID  # the uuid stays primary
    assert not [e for e in rows if e.name == "ac-t-source"], "no stale second row"


def test_spawn_resume_fork_carries_provider_and_model_axes(
    workdir_claude, monkeypatch
) -> None:
    """x-50d0: the wake also dropped the provider and model axes - a zai row
    woke reading anthropic with no requested model, so the session fell back.
    The fork inherits every axis the caller left unstated."""
    from fno.agents.cli import agents_app
    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setattr(claude_mod, "session_is_live", lambda sid: False)
    monkeypatch.setattr(
        claude_mod, "resolve_session_uuid",
        lambda short_id: "beefface-2222-3333-4444-555555555555",
    )
    update_registry(
        lambda entries: entries
        + [
            AgentEntry(
                name="ac-t-source",
                harness="claude",
                cwd="/tmp",
                log_path="/tmp/rev.log",
                short_id="deadbeef",
                harness_session_id=DEAD_UUID,
                node="x-256c",
                provider="zai",
                model="glm-5.3-flash[1m]",
                model_basis="verified",
                requested_model="glm-5.3-flash[1m]",
                requested_provider="zai",
                route_provider_id="zai",
            )
        ]
    )

    result = CliRunner().invoke(
        agents_app,
        ["spawn", "--name", "wake-axes", "-H", "claude", "--resume", DEAD_UUID,
         "--substrate", "thread", "hi"],
        catch_exceptions=False,
    )
    assert result.exit_code == 0, result.output

    forked = next((e for e in load_registry() if e.name == "wake-axes"), None)
    assert forked is not None
    assert forked.node == "x-256c"
    assert forked.provider == "zai"
    assert forked.model == "glm-5.3-flash[1m]"
    # The pin answers from the row's request; the fork is verified only when
    # its own substitution probe observes the model it actually got.
    assert forked.model_basis == "requested"
    assert forked.requested_model == "glm-5.3-flash[1m]"
    assert forked.requested_provider == "zai"
    assert forked.route_provider_id == "zai"


def test_spawn_resume_refused_when_session_claim_held(
    workdir_claude, monkeypatch, tmp_path
) -> None:
    """x-9844 Lane 2: a detached revival refuses (exit 11) when another live
    writer already holds the session:<uuid> claim, instead of spawning a second
    supervisor onto one transcript. The row is left untouched."""
    import os as _os

    from fno.agents.cli import agents_app
    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    monkeypatch.setattr(claude_mod, "session_is_live", lambda sid: False)
    _seed_row("rev-agent", "deadbeef", DEAD_UUID)

    # A different live writer already holds the session single-writer claim.
    claude_mod.acquire_session_writer_claim(
        session_uuid=DEAD_UUID, holder="other-writer", pid=_os.getpid()
    )

    result = CliRunner().invoke(
        agents_app,
        ["spawn", "--name", "rev-agent", "-H", "claude", "--resume", DEAD_UUID,
         "--substrate", "thread", "hi"],
        catch_exceptions=False,
    )
    assert result.exit_code == 11, result.output
    rows = [e for e in load_registry() if e.name == "rev-agent"]
    assert len(rows) == 1
    assert rows[0].short_id == "deadbeef"  # not revived - no 2nd supervisor


# ---------------------------------------------------------------------------
# x-7fef: writer-claim lifetime - pinned to the supervisor, outlives us
# ---------------------------------------------------------------------------

CLAIM_KEY = f"session:{DEAD_UUID}"


def _raise_registry(_fn):
    raise OSError("registry unwritable")


@pytest.fixture
def revive_ready(workdir_claude, monkeypatch, tmp_path):
    """A seeded exited row + isolated claims root, ready for `spawn --resume`.

    Yields a real, LIVE pid that stands in for the spawned supervisor. It must be
    live so the pinned claim classifies live (a dead pid gets reclaimed by stale
    recovery, and held-vs-freed becomes indistinguishable), and it must NOT be
    this process's pid or the re-pin assertion would hold vacuously - the claim
    starts out pinned to the acquiring process.
    """
    import subprocess
    import sys

    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims"))
    monkeypatch.setattr(claude_mod, "session_is_live", lambda sid: False)
    _seed_row("rev-agent", "deadbeef", DEAD_UUID)

    # sys.executable, not `sleep`: workdir_claude stubs PATH to the fake-claude
    # bin dir, so a bare command name is unresolvable here.
    sleeper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
    assert sleeper.pid != os.getpid()
    try:
        yield sleeper.pid
    finally:
        sleeper.terminate()
        sleeper.wait()


def _pin_supervisor(monkeypatch, pid: int | None) -> None:
    """Point ``locate_session`` at a supervisor with ``pid`` (None = sidecar miss)."""
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.harnesses._claude_session_registry import SessionLocator

    def _locate(short_id: str):
        if pid is None:
            return None
        return SessionLocator(
            pid=pid,
            short_id=short_id,
            messaging_socket_path="/tmp/fake.sock",
            jobs_dir=Path("/tmp"),
        )

    monkeypatch.setattr(claude_mod, "locate_session", _locate)


def _spawn_resume() -> object:
    from fno.agents.cli import agents_app

    return CliRunner().invoke(
        agents_app,
        ["spawn", "--name", "rev-agent", "-H", "claude", "--resume", DEAD_UUID,
         "--substrate", "thread", "hi"],
        catch_exceptions=False,
    )


def _status() -> dict:
    from fno.claims import claim_status
    from fno.claims.io import global_claims_root

    return claim_status(CLAIM_KEY, root=global_claims_root())


def test_writer_claim_held_when_registry_write_fails(revive_ready, monkeypatch):
    """AC4-FR: the exit-12 orphan path KEEPS the claim. The supervisor is writing
    the transcript even though no registry row names it, so freeing the claim
    here is exactly the double-writer window codex flagged."""
    from fno.claims import acquire_claim
    from fno.claims.core import ClaimHeldByOther
    from fno.claims.io import global_claims_root

    supervisor_pid = revive_ready
    _pin_supervisor(monkeypatch, supervisor_pid)

    monkeypatch.setattr(dispatch, "update_registry", _raise_registry)

    result = _spawn_resume()
    assert result.exit_code == 12, result.output

    st = _status()
    assert st["state"] == "live"
    assert st["pid"] == supervisor_pid

    with pytest.raises(ClaimHeldByOther):
        acquire_claim(CLAIM_KEY, "someone-else", root=global_claims_root())


def test_writer_claim_released_when_claude_never_ran(revive_ready, monkeypatch):
    """AC5-ERR: exit 127 means claude never executed, so no supervisor can exist
    and the claim must be freed - otherwise a missing binary would lock the
    session out of every later wake."""
    from fno.agents.harnesses import claude as claude_mod

    _pin_supervisor(monkeypatch, revive_ready)

    def _fail(**_kw):
        raise claude_mod.ProviderSubprocessError(127, "claude CLI not found")

    monkeypatch.setattr(claude_mod, "bg_create", _fail)

    result = _spawn_resume()
    assert result.exit_code == 1, result.output
    assert _status()["state"] == "free"

    # And a fresh holder can immediately take it.
    from fno.claims import acquire_claim
    from fno.claims.io import global_claims_root

    assert acquire_claim(CLAIM_KEY, "next-writer", root=global_claims_root())


def test_unknown_orphan_guard_survives_the_caller_dying(revive_ready, monkeypatch):
    """The point of the TTL, proven the only way that counts: with the holder's
    pid dead, the claim must still refuse a competing writer. A pure PID-liveness
    claim would be STALE here and the next wake would reclaim it."""
    from fno.agents.harnesses import claude as claude_mod
    from fno.claims import acquire_claim, claim_status
    from fno.claims.core import ClaimHeldByOther
    from fno.claims.io import global_claims_root

    _pin_supervisor(monkeypatch, revive_ready)

    def _timeout(**_kw):
        raise claude_mod.ProviderSubprocessError(124, "claude --bg timed out")

    monkeypatch.setattr(claude_mod, "bg_create", _timeout)
    assert _spawn_resume().exit_code == 1

    # Rewrite the holder's pid to a dead one: the acquiring `fno agents mail send` has
    # exited, which is exactly when the old lifetime stopped guarding anything.
    from tests._table_seed import update_claim

    root = global_claims_root()
    update_claim(CLAIM_KEY, root=root, pid=_reap_a_dead_pid())

    assert claim_status(CLAIM_KEY, root=root)["state"] == "suspect"
    with pytest.raises(ClaimHeldByOther):
        acquire_claim(CLAIM_KEY, "a-later-wake", root=root)


def _reap_a_dead_pid() -> int:
    """A pid that is certainly not running: spawn and reap a trivial child."""
    import subprocess
    import sys

    p = subprocess.Popen([sys.executable, "-c", ""])
    p.wait()
    return p.pid


def test_claim_substrate_fault_fails_closed(revive_ready, monkeypatch):
    """A corrupt claim file raises ClaimCorrupted, which is neither
    SessionWriterClaimError nor the OSError/RuntimeError wake_and_deliver
    catches. It must surface as exit 11 (writer-possibly-live) rather than
    propagate and abort `fno agents mail send` before its durable fallback runs."""
    from fno.agents.harnesses import claude as claude_mod
    from fno.claims.io import ClaimCorrupted

    def _corrupt(**_kw):
        raise ClaimCorrupted("claim file is not valid YAML")

    monkeypatch.setattr(claude_mod, "acquire_session_writer_claim", _corrupt)

    result = _spawn_resume()
    assert result.exit_code == 11, result.output


def _admitting_gate(monkeypatch) -> None:
    """An unrouted wake asks the spawn gate; stub it with an admitting guard."""

    class _Guard:
        def release(self):
            return None

    monkeypatch.setattr(
        "fno.agents.spawn_gate.run_gate",
        lambda *args, **kwargs: _Guard(),
    )


def test_a_wake_fork_row_names_the_parent_it_was_handed(revive_ready, monkeypatch):
    """The wake fork stamps the parent triple it was handed, not the sender's
    ambient session."""
    from fno.agents.harnesses import claude as claude_mod

    _pin_supervisor(monkeypatch, revive_ready)
    monkeypatch.setattr(
        claude_mod,
        "resolve_session_uuid",
        lambda short_id: "beefface-2222-3333-4444-555555555555",
    )
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "sender-s")

    dispatch.dispatch_spawn(
        name="wake-deadbeef",
        message="m",
        harness="claude",
        cwd=Path("/tmp"),
        resume_session_id=DEAD_UUID,
        parent_edge=("k1-parent", "claude", "/k1"),
    )
    row = next(e for e in load_registry() if e.name == "wake-deadbeef")
    assert row.spawned_by_session == "k1-parent"
    assert row.spawned_by_cwd == "/k1"


