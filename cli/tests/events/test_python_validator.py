"""Tests for the Python event builders and schema loader at fno.events.

The envelope judge is native; its rules are pinned by the Rust goldens.
These cover the Python-side builders and the schema-missing refusal.
"""
from __future__ import annotations

import pytest

from fno.events import (
    SchemaUnavailableError,
    ValidationError,
    mission_complete,
    phase_transition,
    validate,
    worktree_overlap_observed,
)


# -- AC1-HP: typed builders --


def test_phase_transition_rejects_unknown_kwarg() -> None:
    with pytest.raises(TypeError, match="unexpected keyword argument"):
        phase_transition(  # type: ignore[call-arg]
            gate="g", phase="p", nonce="n", session_id="s", source="target", whoops="extra"
        )


def test_mission_complete_rejects_bad_status() -> None:
    with pytest.raises(ValidationError, match=r"unknown status"):
        mission_complete(mission_id="mt-001", status="sideways")


def test_integrity_warning_rejects_unknown_kind() -> None:
    from fno.events import integrity_warning

    with pytest.raises(ValidationError, match=r"unknown integrity_warning kind"):
        integrity_warning(
            kind="bogus_kind_value",
            phase="register",
            session_id="sess-77",
            artifact_path="/tmp/x.md",
        )


# -- AC4-EDGE: SchemaUnavailableError on bad path --

def test_schema_unavailable_raises(monkeypatch, tmp_path) -> None:
    """Resolving with no sibling schema.yaml must raise SchemaUnavailableError.

    The loader reads ``schema.yaml`` beside the package module. We point the
    module's ``__file__`` at an empty tmp dir (no sibling schema) so the lookup
    misses; the live package schema other tests rely on is untouched.
    """
    import fno.events as events_mod

    fake_module = tmp_path / "events" / "__init__.py"
    fake_module.parent.mkdir(parents=True)
    monkeypatch.setattr(events_mod, "__file__", str(fake_module))
    with pytest.raises(SchemaUnavailableError, match="events schema not found"):
        events_mod._resolve_manifest_path()


def test_worktree_overlap_observed_builder_is_deterministic_and_valid() -> None:
    """The builder sorts peers, stamps a stable observation id, and validates."""
    a = worktree_overlap_observed(
        observer_session_id="obs-1",
        peer_session_ids=["peer-b", "peer-a"],
        repository_key="/repo/.git",
        worktree_key="/repo/.git/worktrees/wt1",
    )
    b = worktree_overlap_observed(
        observer_session_id="obs-1",
        peer_session_ids=["peer-a", "peer-b"],
        repository_key="/repo/.git",
        worktree_key="/repo/.git/worktrees/wt1",
    )
    # Envelope ts differs by call; the data payload must be byte-identical.
    assert a["data"] == b["data"], "peer order must not change the observation"
    assert a["data"]["peer_session_ids"] == ["peer-a", "peer-b"], "peers sorted"
    assert a["data"]["observation_id"] == (
        "33acb94fc5648cc8d5289413548c0166fe29e7f51a7daccd922484ed95ce9d26"
    ), "observation id pinned for parity"
    assert validate(a) is None


def test_worktree_overlap_observed_rejects_observer_as_its_own_peer() -> None:
    """The observer cannot be its own peer (the predicate excludes SELF_ID)."""
    with pytest.raises(ValidationError):
        worktree_overlap_observed(
            observer_session_id="obs-1",
            peer_session_ids=["obs-1"],
            repository_key="/repo/.git",
            worktree_key="/repo/.git/wt1",
        )


def test_every_declared_source_is_a_legal_envelope_source() -> None:
    """A type's sources: list must draw only from values the envelope
    itself allows, or the per-type check in validate() could refuse
    every producer of that source unconditionally."""
    import fno.events as events_mod

    events_mod._require_schema()
    envelope_sources = set(events_mod.ALLOWED_SOURCES)
    assert events_mod.EVENT_TYPES is not None
    violations = [
        (name, source)
        for name, spec in events_mod.EVENT_TYPES.items()
        for source in (spec.get("sources") or [])
        if source not in envelope_sources
    ]
    assert violations == []
