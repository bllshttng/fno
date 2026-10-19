"""Tests for classified session-id transitions.

The discriminator is liveness, never a preference: a positively unreachable
predecessor retires in place (succession, one row plus a chain), a positively
reachable predecessor forks into a second row (branch, two rows and one
operator question), and unknown evidence parks the new id additively
(deferred, no question, no overwrite). Every result emits a positive
transition event to the daemon lifecycle log, joinable with the predecessor's
eventual reap.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir
from fno.registry_door import read_registry_document

BIRTH = "e6f78b98-e594-47ed-ad81-84f8a78b8bb7"
REMINT = "08054b1d-a907-47ab-a3d2-4a1e7a87eb4e"


def _spawned_row(name: str = "target-x-f0c2", session_id: str = BIRTH, **extra):
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
            **extra,
        )
    ])


def _reading(verdict: str, basis: str):
    from fno.agents.reachability import Reachability

    return Reachability(verdict=verdict, basis=basis, age_s=None)


def _isolate_daemon_log(monkeypatch, tmp_path: Path) -> None:
    """Point the daemon lifecycle log at this test's tmp tree.

    `agents_home_dir` deliberately ignores state_dir (it resolves
    FNO_AGENTS_HOME, else $HOME), so a test that reads the log must pin the
    env var; chdir keeps the question's project journal in tmp too.
    """
    monkeypatch.setenv("FNO_AGENTS_HOME", str(tmp_path / ".fno" / "agents"))
    monkeypatch.chdir(tmp_path)


def _observe(name: str, session_id: str, *, predecessor_reachable=None, **kwargs):
    from fno.agents.registry import record_session_observation

    return record_session_observation(
        name=name,
        harness="claude",
        session_id=session_id,
        predecessor_reachable=predecessor_reachable,
        **kwargs,
    )


def _transition_events(tmp_path: Path) -> list[dict]:
    """The daemon lifecycle log, where births, deaths, and transitions join."""
    path = tmp_path / ".fno" / "agents" / "events.jsonl"
    # No exists() gate: the store holds the rows even when the raw journal
    # was never written.
    from tests._event_rows import event_rows

    return event_rows(path)


# ---------------------------------------------------------------------------
# Registry-level classification (record_session_observation)
# ---------------------------------------------------------------------------


def test_observation_succession_refuses_successor_already_owned_by_another_row(
    tmp_path: Path, monkeypatch
) -> None:
    """A succession cannot overwrite into a session id already owned by a row."""
    use_tmpdir(monkeypatch, tmp_path)
    _isolate_daemon_log(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, load_registry, write_registry

    successor = "08054b1d-a907-47ab-a3d2-4a1e7a87eb4e"
    write_registry([
        AgentEntry(
            name="target-x-f0c2",
            harness="claude",
            harness_session_id=BIRTH,
            short_id=BIRTH.split("-", 1)[0],
            cwd="/proj",
            log_path="",
            status="live",
        ),
        AgentEntry(
            name="target-existing",
            harness="claude",
            harness_session_id=successor,
            short_id=successor.split("-", 1)[0],
            cwd="/proj",
            log_path="",
            status="live",
        ),
    ])

    with pytest.raises(ValueError, match="succession session .* already has a registry row"):
        _observe("target-x-f0c2", successor, predecessor_reachable=False)

    rows = load_registry()
    assert [row.harness_session_id for row in rows] == [BIRTH, successor]


def test_observation_branch_mints_a_second_row_and_leaves_a_untouched(
    tmp_path: Path, monkeypatch
) -> None:
    """AC2-HP: a positively reachable predecessor stays live on its own row;
    B is a distinct row with its own fno_id and the visible fork edge."""
    use_tmpdir(monkeypatch, tmp_path)
    _isolate_daemon_log(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    _spawned_row()
    entry, outcome = _observe("target-x-f0c2", REMINT, predecessor_reachable=True)
    assert outcome == "branch"
    rows = load_registry()
    assert len(rows) == 2
    live_a = next(row for row in rows if row.harness_session_id == BIRTH)
    branch = next(row for row in rows if row.harness_session_id == REMINT)
    assert live_a.predecessor_session_ids == [], "A's row is never overwritten"
    assert branch.forked_from_session_id == BIRTH
    # A branch row mints its own id at the write; it never copies a session id.
    assert branch.fno_id
    assert branch.fno_id != REMINT
    assert branch.fno_id != BIRTH
    assert branch.name != live_a.name
    assert entry.name == branch.name


def test_observation_succession_replay_is_idempotent_and_byte_stable(
    tmp_path: Path, monkeypatch
) -> None:
    """AC1-ERR: replaying the same payload writes nothing, and the chain
    never duplicates A."""
    use_tmpdir(monkeypatch, tmp_path)
    _isolate_daemon_log(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    _spawned_row()
    _observe("target-x-f0c2", REMINT, predecessor_reachable=False)
    before = read_registry_document(tmp_path / ".fno" / "agents" / "registry.json")[0]
    entry, outcome = _observe("target-x-f0c2", REMINT, predecessor_reachable=False)
    assert outcome == "no-op"
    after = read_registry_document(tmp_path / ".fno" / "agents" / "registry.json")[0]
    assert after == before, "a replay must not rewrite the file"
    row = load_registry()[0]
    assert row.predecessor_session_ids == [BIRTH], "A appears exactly once"


def test_observation_stale_predecessor_evidence_classifies_nothing(
    tmp_path: Path, monkeypatch
) -> None:
    """Evidence sampled for an id the row no longer records must not classify
    against the CURRENT primary - the additive parking is the honest write."""
    use_tmpdir(monkeypatch, tmp_path)
    _isolate_daemon_log(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    _spawned_row()
    entry, outcome = _observe(
        "target-x-f0c2",
        REMINT,
        predecessor_reachable=False,
        expected_predecessor_session_id="session-sampled-before-lock",
    )
    assert outcome == "related"
    row = load_registry()[0]
    assert row.harness_session_id == BIRTH, "stale evidence never retires A"
    assert row.related_session_id == REMINT


# ---------------------------------------------------------------------------
# SessionStart end-to-end: events, questions, fail-soft
# ---------------------------------------------------------------------------


def _main(monkeypatch, tmp_path: Path, session_id: str, reading, name: str):
    from fno.agents import register_session

    if reading is not None:
        monkeypatch.setattr(
            register_session,
            "_reading_for_entry",
            lambda entry: reading,
        )
    return register_session.main([
        "--harness", "claude",
        "--session-id", session_id,
        "--cwd", "/proj",
        "--agent-self", name,
    ])


def test_main_claude_succession_emits_the_classified_event(
    tmp_path: Path, monkeypatch
) -> None:
    """AC1-HP event half: the SessionStart payload for B over a dead A emits
    session_transition_classified naming A, B, the classification, and the
    basis the verdict was read from."""
    use_tmpdir(monkeypatch, tmp_path)
    _isolate_daemon_log(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    _spawned_row()
    assert _main(
        monkeypatch, tmp_path, REMINT, _reading("unreachable", "pane-gone"),
        "target-x-f0c2",
    ) == 0

    events = _transition_events(tmp_path)
    classified = [e for e in events if e["type"] == "session_transition_classified"]
    assert len(classified) == 1, f"exactly one classified event, got {events}"
    data = classified[0]["data"]
    assert data["classification"] == "succession"
    assert data["predecessor_session_id"] == BIRTH
    assert data["successor_session_id"] == REMINT
    assert data["basis"] == "pane-gone"
    assert data["evidence_at"]
    assert load_registry()[0].predecessor_session_ids == [BIRTH]


def test_main_claude_branch_records_the_two_option_operator_question(
    tmp_path: Path, monkeypatch
) -> None:
    """AC2-HP question half + AC2-ERR: the branch asks the operator exactly
    one durable two-option question and never duplicates a claim - the mint
    touches no claim lockfile at all."""
    use_tmpdir(monkeypatch, tmp_path)
    _isolate_daemon_log(monkeypatch, tmp_path)
    from fno.outstanding.core import read_open_questions

    _spawned_row()
    assert _main(
        monkeypatch, tmp_path, REMINT, _reading("reachable", "transcript"),
        "target-x-f0c2",
    ) == 0

    questions = read_open_questions(tmp_path / ".fno")
    branch_qs = [q for q in questions if "session-transition-branch" in q.question]
    assert len(branch_qs) == 1, "exactly one durable question"
    assert list(branch_qs[0].options) == ["inherit node and claim", "start clean"]

    claims_dir = tmp_path / ".fno" / "claims"
    assert not claims_dir.exists() or not any(claims_dir.iterdir()), (
        "a branch mint must never create claim state"
    )

    # A replayed branch observation must not re-ask.
    assert _main(
        monkeypatch, tmp_path, REMINT, _reading("reachable", "transcript"),
        "target-x-f0c2",
    ) == 0
    questions = read_open_questions(tmp_path / ".fno")
    branch_qs = [q for q in questions if "session-transition-branch" in q.question]
    assert len(branch_qs) == 1, "the fork edge is asked once"


def test_main_claude_deferred_emits_the_deferred_event_and_asks_nothing(
    tmp_path: Path, monkeypatch
) -> None:
    """AC3-ERR event half: unavailable evidence emits session_transition_
    deferred naming both ids, and no operator question exists."""
    use_tmpdir(monkeypatch, tmp_path)
    _isolate_daemon_log(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry
    from fno.outstanding.core import read_open_questions

    _spawned_row()
    assert _main(
        monkeypatch, tmp_path, REMINT, _reading("unknown", "silent"),
        "target-x-f0c2",
    ) == 0

    events = _transition_events(tmp_path)
    deferred = [e for e in events if e["type"] == "session_transition_deferred"]
    assert len(deferred) == 1
    assert deferred[0]["data"]["predecessor_session_id"] == BIRTH
    assert deferred[0]["data"]["successor_session_id"] == REMINT
    assert load_registry()[0].harness_session_id == BIRTH, "A is not overwritten"
    assert not read_open_questions(tmp_path / ".fno"), "unknown asks no question"


# -- one name store: the session-names fold ----------------------------------


def _row_with_sid(name, sid, short):
    from fno.agents.registry import AgentEntry, write_registry

    write_registry([
        AgentEntry(
            name=name,
            harness="claude",
            harness_session_id=sid,
            short_id=short,
            cwd="/proj",
            log_path="",
            status="live",
        )
    ])


def test_append_row_alias_refuses_an_alias_another_row_answers_to(
    tmp_path, monkeypatch
) -> None:
    """An ambiguous alias is no address: the append fails closed when another
    row already answers to it, so `find` never has to guess."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import append_row_alias, load_registry

    _row_with_sid("row-a", BIRTH, "aaaa1111")
    _row_with_sid("legible-alias", REMINT, "bbbb2222")
    assert append_row_alias("row-a", "legible-alias") is False
    assert "legible-alias" not in load_registry()[0].aliases, "the ambiguous alias never lands"
