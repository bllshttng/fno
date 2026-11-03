"""The ambient-identity scrub list: which inherited names fno offers to unset.

A claude session carrying seven inherited CODEX_* names once stamped a role
grant with the inherited codex thread id. The owned-identity resolver is
native now; these tests pin the Python scrub list the remedy text reads.
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


# --- the grantor derivation -------------------------------------------------


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
