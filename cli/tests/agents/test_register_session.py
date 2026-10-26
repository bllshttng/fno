"""Tests for register-existing-session and the SessionStart id restamp.

The registration core lives in ``fno.agents.registry`` and the
fail-open SessionStart entry point in ``fno.agents.register_session``.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir
from fno.registry_door import read_registry_document


def _events(tmp_path: Path) -> list[dict]:
    from tests._event_rows import event_rows

    return event_rows(tmp_path / ".fno" / "events.jsonl")


# ---------------------------------------------------------------------------
# AC7-HP / AC7-UI: registration creates an addressable, verifiable row
# ---------------------------------------------------------------------------


def test_ac7_hp_registers_addressable_entry(tmp_path: Path, monkeypatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry, register_existing_session

    entry = register_existing_session(
        provider="claude",
        session_id="ef9982cc-2543-4cea-9a20-081cca7119f6",
        cwd="/home/user/project",
    )

    assert entry.harness == "claude"
    # The transport short id is claude's own 8-hex attach/job key (the uuid's
    # leading segment), NOT the full uuid: `claude attach <short_id>` refuses a
    # full uuid, so a row carrying one was unattachable from the mux tap.
    assert entry.short_id == "ef9982cc"
    # Registered NON-live: a hand-started session has no live transport, so it
    # must not be a resolve_to_project anycast target (else default sends
    # dead-letter to inbox/<agent-name>/, which its wake hook never reads).
    assert entry.status == "idle"
    # Derived name is the canonical first-eight, the mailbox id a peer addresses.
    assert entry.name == "ef9982cc"

    # AC7-UI: a fresh load shows the row with provider/cwd/status intact.
    rows = load_registry()
    assert len(rows) == 1
    assert rows[0].name == entry.name
    assert rows[0].cwd == "/home/user/project"
    assert rows[0].status == "idle"


def test_ac7_hp_idempotent_on_resame_session(tmp_path: Path, monkeypatch) -> None:
    """The hook re-firing for the same session refreshes, never duplicates."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry, register_existing_session

    register_existing_session(provider="claude", session_id="sess-1", cwd="/a")
    second = register_existing_session(provider="claude", session_id="sess-1", cwd="/b")

    rows = load_registry()
    assert len(rows) == 1
    assert rows[0].cwd == "/b"  # refreshed in place
    assert rows[0].status == "idle"
    assert second.name == rows[0].name


# ---------------------------------------------------------------------------
# Name and transport-key collisions
# ---------------------------------------------------------------------------


def test_ac4_err_generated_name_collision_fails_closed(tmp_path: Path, monkeypatch) -> None:
    """A canonical first-eight overlap between two sessions never blocks
    registration: both register (the name suffixes), and the shared short
    fails closed as ambiguity at read time."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import (
        AgentResolutionError,
        load_registry,
        register_existing_session,
        resolve_agent,
    )

    a = register_existing_session(
        provider="claude", session_id="tail0001-aaaa-bbbb-cccc-dddddddddddd", cwd="/s"
    )
    b = register_existing_session(
        provider="claude", session_id="tail0001-1111-2222-3333-444455556666", cwd="/s"
    )

    assert a.name == "tail0001"
    assert b.name == "tail0001-2"
    assert len(load_registry()) == 2
    with pytest.raises(AgentResolutionError, match="ambiguous"):
        resolve_agent("tail0001")


def test_transport_short_id_cannot_collide_with_existing_handle_namespace(
    tmp_path: Path, monkeypatch
) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentResolutionError, register_existing_session

    register_existing_session(
        provider="codex", session_id="deadbeef-0000-0000-0000-000000000000", cwd="/s", name="worker"
    )
    with pytest.raises(AgentResolutionError, match="transport short id.*collision"):
        register_existing_session(
            provider="codex",
            session_id="cafebabe-0000-0000-0000-000000000000",
            cwd="/s",
            name="other",
            short_id="deadbeef",
        )


# ---------------------------------------------------------------------------
# Input validation
# ---------------------------------------------------------------------------


def test_register_rejects_unknown_provider(tmp_path: Path, monkeypatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import register_existing_session

    with pytest.raises(ValueError, match="unknown harness"):
        register_existing_session(provider="bogus", session_id="x", cwd="/s")


# ---------------------------------------------------------------------------
# `fno agents register` (the /fno-me seam) answers from the Rust client verb
# (crates/fno-agents/src/register.rs); without the binary the Python front
# refuses. Its contracts are pinned by crates/fno-agents/tests/golden/register/
# and tests/register_parity.rs, so no CliRunner cases live here anymore.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# A harness that re-mints the session id footnote passed at spawn
#
# Observed on claude: a worker launched as `claude --session-id <uuid>` carried
# on under a DIFFERENT uuid ~35s in, taking its transcript with it (identical
# message uuids on both sides -- a rename with carry-over, not a fork into two
# live sessions). The row kept the birth id and stopped addressing anything.
#
# The row NAME survives that, which is why the restamp keys on it.
# ---------------------------------------------------------------------------

BIRTH = "e6f78b98-e594-47ed-ad81-84f8a78b8bb7"
REMINT = "08054b1d-a907-47ab-a3d2-4a1e7a87eb4e"
REMINT_SAME_PREFIX = "08054b1d-bbbb-47ab-a3d2-4a1e7a87eb4e"


def _spawned_row(name: str = "target-x-f0c2", session_id: str = BIRTH):
    """A footnote-spawned claude worker row as `fno agents spawn` writes it."""
    from fno.agents.registry import AgentEntry, write_registry

    write_registry([
        AgentEntry(
            name=name,
            harness="claude",
            harness_session_id=session_id,
            short_id=session_id.split("-", 1)[0],
            cwd="/proj",
            log_path="",
            status="live",
        )
    ])


def test_restamp_repoints_row_at_the_reminted_id(tmp_path: Path, monkeypatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry, restamp_harness_session_id

    _spawned_row()
    entry = restamp_harness_session_id(
        name="target-x-f0c2",
        harness="claude",
        session_id=REMINT,
        predecessor_reachable=False,
    )

    assert entry is not None
    rows = load_registry()
    # One worker, one row: the correction lands in place, never as a second row.
    assert len(rows) == 1
    assert rows[0].harness_session_id == REMINT
    # claude addresses by the 8-hex jobId in short_id, so a restamp that fixed
    # only harness_session_id would leave attach/resume on the dead id.
    assert rows[0].short_id == "08054b1d"
    assert rows[0].predecessor_session_ids == [BIRTH]
    # Untouched: the restamp corrects identity, not lifecycle.
    assert rows[0].status == "live"
    assert rows[0].name == "target-x-f0c2"


def test_restamp_branches_a_promoted_live_row(tmp_path: Path, monkeypatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry, restamp_harness_session_id, write_registry

    _spawned_row()
    row = load_registry()[0]
    row.role_level = 1
    row.role_scope = "scope-a"
    row.role_grantor = "human"
    write_registry([row])

    entry = restamp_harness_session_id(
        name="target-x-f0c2",
        harness="claude",
        session_id=REMINT,
        predecessor_reachable=True,
    )

    assert entry is not None
    rows = load_registry()
    assert len(rows) == 2
    predecessor = next(row for row in rows if row.harness_session_id == BIRTH)
    branch = next(row for row in rows if row.harness_session_id == REMINT)
    assert predecessor.role_level == 1
    assert predecessor.role_scope == "scope-a"
    assert predecessor.role_grantor == "human"
    assert branch.name == "target-x-f0c2-branch-08054b1d"
    # The branch mints its own id at the write; it is neither session id.
    assert branch.fno_id
    assert branch.fno_id != REMINT
    assert branch.fno_id != BIRTH
    assert branch.forked_from_session_id == BIRTH
    assert branch.role_level is None
    assert branch.role_scope is None
    assert branch.role_grantor is None
    # Born bg-routable - the 8-hex jobId the rv socket farm keys on,
    # derived from the new session id, not the predecessor's handle.
    assert branch.short_id == REMINT.split("-", 1)[0]

    again = restamp_harness_session_id(
        name="target-x-f0c2", harness="claude", session_id=REMINT
    )
    assert again is not None
    assert again.name == branch.name

    # A second claude fork whose session id shares the
    # first-8 prefix shares the transport short_id the rv socket farm keys on,
    # so the write REFUSES rather than minting a row the join cannot tell
    # apart from the first branch. The -2 name suffix stays reachable for
    # harnesses whose first-8 is not a transport key (codex keeps short_id "").
    from fno.agents.registry import AgentResolutionError

    with pytest.raises(AgentResolutionError, match="collides with row"):
        restamp_harness_session_id(
            name="target-x-f0c2", harness="claude", session_id=REMINT_SAME_PREFIX
        )
    assert len(load_registry()) == 2


def test_restamp_refuses_a_stale_predecessor_observation(
    tmp_path: Path, monkeypatch
) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry, restamp_harness_session_id

    _spawned_row()

    entry = restamp_harness_session_id(
        name="target-x-f0c2",
        harness="claude",
        session_id=REMINT,
        predecessor_reachable=False,
        expected_predecessor_session_id="session-captured-before-lock",
    )

    assert entry is None
    assert load_registry()[0].harness_session_id == BIRTH


def test_restamp_ignores_an_unknown_name_or_a_harness_mismatch(
    tmp_path: Path, monkeypatch
) -> None:
    """The name is a PK, not a search: a miss corrects nothing and creates nothing."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry, restamp_harness_session_id

    _spawned_row()
    assert restamp_harness_session_id(
        name="someone-else", harness="claude", session_id=REMINT
    ) is None
    assert restamp_harness_session_id(
        name="target-x-f0c2", harness="codex", session_id=REMINT
    ) is None

    rows = load_registry()
    assert len(rows) == 1
    assert rows[0].harness_session_id == BIRTH


def test_main_agent_self_restamps_instead_of_registering(tmp_path: Path, monkeypatch) -> None:
    """The whole point of the --agent-self branch: registration keys its upsert
    on harness_session_id, so routing a re-minted worker through it would MISS
    and append a second row for one worker. A claude row's observation is
    ADDITIVE: the re-minted id fills the related slot, the primary stays, and
    the row count stays one."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.register_session import main
    from fno.agents.registry import load_registry

    _spawned_row()
    assert main([
        "--harness", "claude",
        "--session-id", REMINT,
        "--cwd", "/proj",
        "--agent-self", "target-x-f0c2",
    ]) == 0

    rows = load_registry()
    assert len(rows) == 1
    assert rows[0].name == "target-x-f0c2"
    assert rows[0].harness_session_id == BIRTH, "the primary is never replaced"
    assert rows[0].related_session_id == REMINT, "the re-mint lands additively"
    kinds = [e["kind"] for e in _events(tmp_path)]
    assert "session_id_recorded" in kinds


def test_main_agent_self_is_failopen(tmp_path: Path, monkeypatch) -> None:
    """SessionStart must never block on a locked or unwritable registry. The
    claude path's recorder is the seam that can blow up; a non-claude row
    keeps the same guarantee through the lineage restamp."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents import register_session

    def _boom(**kwargs):
        raise OSError("registry unwritable")

    monkeypatch.setattr("fno.agents.registry.record_session_observation", _boom)
    assert register_session.main([
        "--harness", "claude",
        "--session-id", REMINT,
        "--cwd", "/proj",
        "--agent-self", "w1",
    ]) == 0
    assert "session_restamp_failed" in [e["kind"] for e in _events(tmp_path)]


def test_restamp_waits_for_a_row_the_spawner_has_not_written_yet(
    tmp_path: Path, monkeypatch
) -> None:
    """The spawner appends the row after `mux pane run` returns, so a fast worker
    can run this hook first. Losing that race is unrecoverable on any route where
    the restamp is the only path to an id, so the hook waits for its own row.

    The row appearing between the existence check and the restamp must still be
    restamped: checking existence first is what makes that safe.
    """
    use_tmpdir(monkeypatch, tmp_path)
    import fno.agents.register_session as rs

    calls = {"n": 0}

    def _late_row(name: str, harness: str) -> bool:
        # Absent on the first look; the spawner writes it just before the second.
        calls["n"] += 1
        if calls["n"] == 2:
            _spawned_row()
        return calls["n"] > 2

    monkeypatch.setattr(rs, "_row_exists", _late_row)
    monkeypatch.setattr(rs, "_RESTAMP_ROW_POLL_S", 0.0)
    monkeypatch.setenv("FNO_AGENT_ROW_PENDING", "target-x-f0c2")

    assert rs._restamp("target-x-f0c2", "claude", REMINT) == 0

    from fno.agents.registry import load_registry

    rows = load_registry()
    assert len(rows) == 1
    assert rows[0].harness_session_id == BIRTH, "the late row keeps its primary"
    assert rows[0].related_session_id == REMINT, (
        "the late row still records the observed id"
    )


def test_restamp_promotes_a_spawning_row_once_the_worker_names_itself(
    tmp_path: Path, monkeypatch
) -> None:
    """A happy-hosted claude pane cannot be given an id at spawn (happy discards
    the pinned one), so its row parks at `spawning` and the restamp is the ONLY
    path to an id. Leaving it `spawning` after the worker has proven its own
    identity is the same lie as calling it `live` before, just inverted.
    """
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, load_registry, restamp_harness_session_id, write_registry

    write_registry([
        AgentEntry(
            name="worker-happy-pane",
            harness="claude",
            harness_session_id=None,
            cwd="/proj",
            # Needs a resolvable handle; harness_session_id is
            # deliberately None (that's what this test is about), so this
            # stands in for the fallback log a real mint would touch.
            log_path="/proj/worker-happy-pane.log",
            status="spawning",
            mux={"session": "main", "pane_id": 11},
        )
    ])
    entry = restamp_harness_session_id(
        name="worker-happy-pane", harness="claude", session_id=REMINT
    )

    assert entry is not None
    rows = load_registry()
    assert rows[0].harness_session_id == REMINT
    assert rows[0].status == "live", "a worker that named itself is addressable"
    assert rows[0].short_id == "", "still a mux row: exactly one live ref"


def test_restamp_corrects_a_mux_hosted_row(tmp_path: Path, monkeypatch) -> None:
    """The reported row was PANE-hosted, and a mux row is the one shape where
    filling short_id is illegal: `_validate_single_live_ref` enforces mux XOR
    worker XOR bg, so synthesizing a transport key here makes write_registry
    raise, the fail-open except swallow it, and the id change never persist --
    a restamp that no-ops on precisely the row it exists to fix.

    harness_session_id alone is enough to re-address a mux row: `resolve_agent`
    matches the full id and the short derived FROM it, neither of which reads
    the stored short_id.
    """
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, load_registry, restamp_harness_session_id, write_registry

    write_registry([
        AgentEntry(
            name="target-x-f0c2-pane-identity",
            harness="claude",
            harness_session_id=BIRTH,
            cwd="/proj",
            log_path="",
            status="live",
            mux={"session": "main", "pane_id": 10},
        )
    ])
    entry = restamp_harness_session_id(
        name="target-x-f0c2-pane-identity", harness="claude", session_id=REMINT
    )

    assert entry is not None
    rows = load_registry()
    assert rows[0].harness_session_id == REMINT, "the id change must actually persist"
    assert rows[0].short_id == "", "a mux row holds exactly one live ref"
    assert rows[0].mux == {"session": "main", "pane_id": 10}


# ---------------------------------------------------------------------------
# The ADDITIVE SessionStart observation (claude rows).
# One row, at most one optional related id, no lineage: the lineage restamp
# above stays for other harnesses and direct callers, but a claude worker's
# SessionStart never replaces a recorded id again.
# ---------------------------------------------------------------------------
FORK = "f00baa11-2222-4333-8444-555566667777"
THIRD = "aabbbccc-dddd-4eee-8fff-000011112222"


def _observe(name: str, session_id: str):
    from fno.agents.registry import record_session_observation

    return record_session_observation(name=name, harness="claude", session_id=session_id)


def test_observation_fills_the_related_slot_without_touching_the_primary(
    tmp_path: Path, monkeypatch
) -> None:
    """Case 3: a fork's id fills the ONE optional related slot. The primary
    stays exactly as spawned - the old last-writer replacement is the defect
    this exists to end."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    _spawned_row()
    entry, outcome = _observe("target-x-f0c2", FORK)
    assert outcome == "related"
    row = load_registry()[0]
    assert row.harness_session_id == BIRTH, "the primary is never replaced"
    assert row.related_session_id == FORK
    assert len(load_registry()) == 1, "one worker, one row, two valid ids"


def test_observation_refuses_a_third_distinct_id_and_names_both(
    tmp_path: Path, monkeypatch
) -> None:
    """The cap: primary and related already hold two different ids, so a
    third writes NOTHING and the refusal names both recorded ids."""
    use_tmpdir(monkeypatch, tmp_path)

    _spawned_row()
    _observe("target-x-f0c2", FORK)
    before = read_registry_document(tmp_path / ".fno" / "agents" / "registry.json")[0]
    entry, outcome = _observe("target-x-f0c2", THIRD)
    assert outcome == "refused-cap"
    assert entry.harness_session_id == BIRTH
    assert entry.related_session_id == FORK, "the refusal carries both ids"
    after = read_registry_document(tmp_path / ".fno" / "agents" / "registry.json")[0]
    assert after == before, "a refused observation changes nothing on disk"


def test_the_cap_redecides_under_the_registry_lock(
    tmp_path: Path, monkeypatch
) -> None:
    """Two concurrent SessionStarts with different ids both pass a pre-read
    that saw an empty related slot. The first updater fills it; the second
    must re-decide the cap UNDER the lock and write nothing, never overwrite
    the id the first winner recorded."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents import registry as registry_mod

    _spawned_row()  # primary filled, related empty
    # The concurrent winner's write lands on the real file...
    _observe("target-x-f0c2", FORK)
    # ...but THIS caller's pre-read already snapshotted the pre-fork row.
    from fno.agents.registry import AgentEntry as _AE

    stale_snapshot = [
        _AE(
            name="target-x-f0c2",
            harness="claude",
            cwd="/proj",
            log_path="/tmp/forked.log",
            short_id="cafef00d",
            status="live",
        )
    ]

    real_load = registry_mod.load_registry

    def stale_first_read(path=None):
        if stale_first_read.calls:  # noqa: B008 - simple flag
            return real_load(path=path)
        stale_first_read.calls += 1
        return stale_snapshot

    stale_first_read.calls = 0
    monkeypatch.setattr(registry_mod, "load_registry", stale_first_read)

    entry, outcome = _observe("target-x-f0c2", THIRD)
    assert outcome == "refused-cap", outcome
    row = real_load()[0]
    assert row.harness_session_id == BIRTH
    assert row.related_session_id == FORK, (
        "the under-lock cap wrote nothing: the winner's id stays"
    )


def test_a_refused_observation_is_visible_and_the_hook_stays_fail_soft(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """The cap refusal through the hook path: a warning event naming both
    ids, a stderr line, and exit 0 - the session never blocks on it."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents import register_session

    _spawned_row()
    _observe("target-x-f0c2", FORK)
    monkeypatch.setenv("FNO_AGENT_SELF", "target-x-f0c2")
    monkeypatch.setattr(
        "fno.agents.register_session._row_exists", lambda name, harness: True
    )
    rc = register_session.main(
        [
            "--harness",
            "claude",
            "--session-id",
            THIRD,
            "--cwd",
            "/proj",
            "--agent-self",
            "target-x-f0c2",
            "--source",
            "resume",
        ]
    )
    assert rc == 0, "the hook never blocks session start"
    refused = [e for e in _events(tmp_path) if e["kind"] == "session_id_record_refused"]
    assert refused, "the refusal is a named event"
    assert refused[0]["recorded_ids"] == f"{BIRTH},{FORK}"
    # The envelope's top-level source is the channel identity ('agents');
    # the caller's domain source (--source resume) rides in data.
    assert refused[0]["data"]["source"] == "resume"
    err = capsys.readouterr().err
    assert BIRTH in err and FORK in err, "both recorded ids are in the visible line"
    assert (
        f"fno agents adopt {THIRD}" in err
    ), "the refusal names the adopt command that mints the parked id a row"


# ---------------------------------------------------------------------------
# /fno-me inside a spawned worker
# ---------------------------------------------------------------------------


def _write_pane_row(name: str, harness: str, **overrides):
    from fno.agents.registry import AgentEntry, write_registry

    row = AgentEntry(
        name=name,
        harness=harness,
        harness_session_id="11111111-2222-3333-4444-555555555555",
        cwd="/proj",
        log_path="",
        status="live",
    )
    for key, value in overrides.items():
        setattr(row, key, value)
    write_registry([row])
    return row


# ---------------------------------------------------------------------------
# The deferred open: a spawn whose harness session id does not exist yet
# parks the owed sessions row on the worker's registry row; SessionStart's
# first id observation opens it. Covers both SessionStart writers.
# ---------------------------------------------------------------------------

DEFER_NODE = "x-defer1"


def _seed_deferred_node() -> None:
    """One node in the tmp graph the parked row opens at SessionStart."""
    import json

    from fno import paths

    g = paths.graph_json()
    g.parent.mkdir(parents=True, exist_ok=True)
    seed_graph(g, json.dumps({"entries": [{
            "id": DEFER_NODE, "title": "deferred provenance target",
            "type": "feature", "project": "fno", "status": "ready",
        }]}))


def _deferred_sessions() -> list[dict]:
    from fno import paths
    from fno.graph.store import read_graph

    return next(
        e for e in read_graph(paths.graph_json()) if e["id"] == DEFER_NODE
    ).get("sessions", [])


def _parked_row(name: str = "target-x-def1"):
    """A spawned claude row with NO session id yet, carrying the park."""
    from fno.agents.registry import AgentEntry, write_registry

    write_registry([
        AgentEntry(
            name=name,
            harness="claude",
            harness_session_id="",
            pid=4_194_301,
            pid_start_time=1_000,
            node=DEFER_NODE,
            effort="xhigh",
            pending_session_row={"phase": "execute", "merge_grant": None},
            cwd="/proj",
            log_path="",
            status="spawning",
        )
    ])


def test_session_start_first_fill_opens_the_parked_row(
    tmp_path: Path, monkeypatch
) -> None:
    """The claude SessionStart arm: the empty primary accepts its first id and
    the parked payload opens the node's sessions row, then is cleared."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry, record_session_observation

    _seed_deferred_node()
    _parked_row()
    entry, outcome = record_session_observation(
        name="target-x-def1", harness="claude", session_id=REMINT
    )

    assert outcome == "primary"
    assert entry is not None
    rows = _deferred_sessions()
    assert len(rows) == 1
    assert rows[0]["session_id"] == REMINT
    assert rows[0]["phase"] == "execute"
    assert rows[0]["effort"] == "xhigh"
    row = load_registry()[0]
    assert row.pending_session_row is None
    assert row.harness_session_id == REMINT


