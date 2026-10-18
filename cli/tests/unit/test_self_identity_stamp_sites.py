"""Durable stamp sites resolve identity through the owned path, not precedence.

`resolve_harness_identity` returns the first ambient harness marker in
precedence order. That is right when exactly one harness family is present and
wrong the moment a process inherits a foreign one, so a site that WRITES the
resolved value onto a durable record must use `resolve_self_identity`, which
proves ownership from the process tree and refuses rather than guessing.

`scripts/ci/check-identity-stamp-sites.sh` keeps the remainder from growing.
These tests pin the behavior the gate cannot see.
"""
from __future__ import annotations

from fno.claims.self_identity import resolve_self_identity


def test_a_mixed_env_the_tree_can_decide_resolves_rather_than_refusing(monkeypatch):
    # The incident: a claude session carrying a CODEX_THREAD_ID it inherited
    # from its spawner. The raw primitive no longer launders that into the
    # stranger's id - it refuses the whole mixed env - so nothing is stamped
    # wrong either way. What the owned path adds is the RIGHT answer: the
    # process tree proves which marker this session minted, so a real worker
    # stamps its own identity instead of going unstamped.
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness", lambda *a, **k: "claude"
    )
    env = {"CODEX_THREAD_ID": "foreign-thread", "CLAUDE_CODE_SESSION_ID": "mine"}

    from fno.harness_identity import resolve_harness_identity

    assert resolve_harness_identity(env).session_id is None

    ident = resolve_self_identity(env)
    assert (ident.session_id, ident.harness) == ("mine", "claude")
    assert ident.disposition == "proven"


def test_an_unprovable_disagreement_stamps_nothing(monkeypatch):
    # No harness ancestor (CI, cron, a bare shell) and two families: genuinely
    # undecidable, so both fields come back None and every caller's existing
    # "no ambient identity" branch becomes the refusal.
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness", lambda *a, **k: None
    )

    ident = resolve_self_identity(
        {"CODEX_THREAD_ID": "theirs", "CLAUDE_CODE_SESSION_ID": "mine"}
    )

    assert (ident.session_id, ident.harness) == (None, None)
    assert ident.disposition == "ambiguous"
    # The markers travel with the refusal, so a caller can name both in its
    # message rather than reporting a bare absence.
    assert {marker for marker, _h, _v in ident.markers_present} == {
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
    }
