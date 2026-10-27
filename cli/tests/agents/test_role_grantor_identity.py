"""The role grantor resolves identity through the one owned-identity resolver.

A claude session carrying seven inherited CODEX_* names once stamped a role
grant with the inherited codex thread id: the registry recorded a codex grantor
for a role a claude session issued, while ``fno whoami`` and ``fno backlog
idea`` answered the claude session correctly in the same minute. One path of N
reached a different answer, and it was the path that stamps authority.

These tests hold the collapse. Every grantor derivation reaches
``resolve_self_identity``, so a second implementation added to any of them fails
here rather than in review.

The rule the tests pin: the process-tree walk is the ONLY prover. A self-set
marker such as ``CLAUDECODE`` survives a fork, so it names ancestry rather than
self and must never stand in for the walk - promoting it contradicts a codex
session's own marker and loses a valid identity. With no walk, resolution
refuses rather than guesses, and ``fno whoami`` names the inherited family.
"""
from __future__ import annotations


import pytest

from fno.harness_identity import AMBIENT_IDENTITY_ENV

CLAUDE_SID = "119e3c52-62bf-43b4-b3c4-3c7ce659f802"
CODEX_TID = "01a02125-4eb4-7bf1-b74e-d238887eb092"


@pytest.fixture
def poisoned_env(monkeypatch):
    """The measured environment: a real claude session whose env also carries an
    inherited codex thread, plus the marker the claude binary set for itself."""
    for name in AMBIENT_IDENTITY_ENV:
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("CLAUDECODE", "1")
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", CLAUDE_SID)
    monkeypatch.setenv("CODEX_THREAD_ID", CODEX_TID)


def _walk(monkeypatch, harness):
    """Pin what the process-tree walk can prove about this process."""
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness", lambda *_a, **_k: harness
    )


# --- the self-set marker is not an identity input ---------------------------


def test_a_sole_codex_marker_survives_an_inherited_claude_marker(monkeypatch):
    """The trap this file exists to keep shut.

    CLAUDECODE looks like proof of a claude self: a shell that never ran claude
    cannot produce it. But it survives a fork, so a codex session started from a
    shell that HAD run claude carries it. Reading it as proof there contradicts
    that session's own CODEX_THREAD_ID and turns a cleanly resolving codex
    session into an ambiguous one, taking every identity consumer with it.
    """
    from fno.claims.self_identity import resolve_self_identity

    for name in AMBIENT_IDENTITY_ENV:
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("CODEX_THREAD_ID", CODEX_TID)
    monkeypatch.setenv("CLAUDECODE", "1")
    _walk(monkeypatch, None)

    identity = resolve_self_identity()

    assert identity.session_id == CODEX_TID
    assert identity.harness == "codex"


# --- the grantor derivation -------------------------------------------------


def test_grantor_is_the_claude_session_never_the_inherited_codex_thread(
    poisoned_env, monkeypatch
):
    """The measured defect, inverted into an assertion.

    ``_capture_parent_edge`` is what the spawn path stamps as ``role_grantor``
    (``spawned_by_session or "human"``), so the session id it returns IS the
    recorded grantor.
    """
    from fno.agents.dispatch import _capture_parent_edge

    _walk(monkeypatch, "claude")
    session_id, harness, _cwd = _capture_parent_edge()

    assert session_id == CLAUDE_SID
    assert session_id != CODEX_TID
    assert harness == "claude"

    grantor = session_id or "human"
    assert grantor == CLAUDE_SID


def test_no_walk_refuses_to_guess_rather_than_stamping_a_stranger(
    poisoned_env, monkeypatch
):
    """psutil denied, no harness ancestor, a container hiding the parent chain.

    Two families and nothing proven, so no lineage is stamped. The grant records
    "human" and `fno whoami` names the family to clear; that is the honest
    answer, because the environment alone cannot say which session this is.
    """
    from fno.agents.dispatch import _capture_parent_edge

    _walk(monkeypatch, None)
    session_id, harness, _cwd = _capture_parent_edge()

    assert session_id is None
    assert harness is None


# --- the names fno cannot unexport ------------------------------------------


def test_an_opencode_session_is_never_told_to_delete_its_own_marker(monkeypatch):
    """The keep-family must come from the resolver, not from `_detect_harness`.

    That helper fails open to "claude" for any harness outside claude/codex/
    gemini, so a cleanly resolving opencode session would be handed the command
    to unset its own OPENCODE_SESSION_ID, costing it its mail handle and its
    claims. It prints on every whoami and every SessionStart injection, so the
    wrong advice is loud. The whoami render itself is native now; this file's
    remaining coverage of the remedy lives with the resolver tests here.
    """
    from fno.harness_identity import ambient_identity_strip_flags

    for name in AMBIENT_IDENTITY_ENV:
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("OPENCODE_SESSION_ID", "ses_9f2a")

    assert ambient_identity_strip_flags("opencode") == []


def test_the_remedy_never_offers_to_delete_the_hermes_spawn_guard(monkeypatch):
    """HERMES_SESSION_ID is a fail-closed guard, not a session identity:
    HermesCliAdapter reads it as "shell spawn is FORBIDDEN". It is never a
    keep-family, so an unguarded strip list would offer to delete it and turn
    that refusal into a real spawn."""
    from fno.harness_identity import ambient_identity_strip_flags

    for name in AMBIENT_IDENTITY_ENV:
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", CLAUDE_SID)
    monkeypatch.setenv("HERMES_SESSION_ID", "hermes-1")
    monkeypatch.setenv("CODEX_THREAD_ID", CODEX_TID)

    flags = ambient_identity_strip_flags("claude")

    assert "HERMES_SESSION_ID" not in flags
    assert "CODEX_THREAD_ID" in flags


def test_the_marker_scrubs_so_a_child_never_inherits_it():
    """It resolves nothing, and it still scrubs: a codex child carrying its
    claude parent's CLAUDECODE sends `fno doctor` to the wrong settings file."""
    assert "CLAUDECODE" in AMBIENT_IDENTITY_ENV
