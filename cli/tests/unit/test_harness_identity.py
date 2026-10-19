"""Tests for shared ambient harness session identity resolution."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from fno.registry_door import read_registry_document
from fno.harness_identity import (
    AMBIENT_IDENTITY_ENV,
    HARNESS_SESSION_MARKERS,
    HarnessIdentity,
    LEGACY_HANDLE_RE,
    canonical_handle,
    current_session_id,
    current_session_ids,
    harness_of_session_id,
    legacy_suffix_handle,
    resolve_harness_identity,
    resolve_owned_identity,
    session_identity_key,
)


@pytest.mark.parametrize(
    ("session_id", "expected"),
    [
        ("b936b571-e0aa-40ed-a07d-97acb9a87db1", "claude"),
        ("01a06886-9405-74a1-8afd-5b67baf89604", "codex"),
        ("ses_7f3a9b2cAbCd1234", "opencode"),
        ("8ad8e13c", None),
    ],
)
def test_harness_of_session_id_reads_the_shapes_own_harness(session_id, expected):
    assert harness_of_session_id(session_id) == expected


@pytest.mark.parametrize(
    ("marker", "session_id", "harness"),
    [
        ("CODEX_THREAD_ID", "thread-1", "codex"),
    ],
)
def test_resolves_each_supported_marker(marker, session_id, harness):
    assert resolve_harness_identity({marker: f"  {session_id}  "}) == HarnessIdentity(
        session_id=session_id,
        harness=harness,
    )


def test_same_family_dup_resolves_and_disagreement_degrades():
    """Within ONE family: the same id under two names (codex sets both the
    thread id and the legacy session id) resolves normally; two DIFFERENT ids
    of one family are a disagreement and degrade rather than pick the
    durable-looking one by position (x-0992). Cross-family never gets here -
    see the refusal test below."""
    same = {
        "CODEX_THREAD_ID": "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4",
        "CODEX_SESSION_ID": "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4",
    }
    assert resolve_harness_identity(same) == HarnessIdentity(
        "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4", "codex"
    )
    disagree = {
        "CODEX_THREAD_ID": "thread",
        "CODEX_SESSION_ID": "codex-session",
    }
    assert resolve_harness_identity(disagree) == HarnessIdentity(None, None)


def test_cross_family_markers_refuse_instead_of_precedence():
    """x-b57a: markers from two harness families refuse, they do not pick.

    The precedence winner here is a foreign inherited marker laundered into
    this session's identity - the leak that made whoami report harness codex
    for a claude session carrying CODEX_THREAD_ID. The refusal makes this
    resolver agree with infer_invoking_harness on every environment."""
    env = {
        "CODEX_THREAD_ID": "thread",
        "CLAUDE_CODE_SESSION_ID": "claude",
        "CODEX_SESSION_ID": "codex-session",
        "GEMINI_SESSION_ID": "gemini",
    }
    assert resolve_harness_identity(env) == HarnessIdentity(None, None)


def test_canonical_pair_requires_process_proof_and_respects_collision():
    session_id = "11111111-1111-4111-8111-111111111111"
    env = {"FNO_HARNESS_NAME": "claude", "FNO_HARNESS_SESSION_ID": session_id}

    owned = resolve_owned_identity(
        env,
        prove=lambda _harness, _session: False,
        collide=lambda _harness, _session: "victim",
    )

    assert owned.disposition == "ambiguous"
    assert owned.session_id is None
    assert owned.harness is None
    assert owned.rejected == (
        {
            "harness": "claude",
            "session_id": session_id,
            "reason": "owned_by_live_row",
            "owner": "victim",
        },
    )


def test_canonical_proof_beats_own_row_collision():
    """AC1-HP: proof is self. A spawned worker's stamped id is held in the
    registry by the worker's OWN row (spawn mints both in one act), so a
    proven canonical identity resolves even though a live row matches the
    id (x-6d6c: the worker read as a competing holder of itself)."""
    session_id = "11111111-1111-4111-8111-111111111111"
    env = {
        "FNO_HARNESS_NAME": "codex",
        "FNO_HARNESS_SESSION_ID": session_id,
        "CODEX_THREAD_ID": session_id,
    }

    owned = resolve_owned_identity(
        env,
        prove=lambda _harness, _session: True,
        collide=lambda _harness, _session: "t-32db-waves45",
    )

    assert owned.disposition == "canonical"
    assert owned.session_id == session_id
    assert owned.harness == "codex"
    assert owned.rejected == ()


@pytest.mark.parametrize(
    "env",
    [
        {"FNO_HARNESS_SESSION_ID": "session-only"},
    ],
)
def test_partial_canonical_stamp_has_named_refusal(env):
    owned = resolve_owned_identity(env)
    assert owned.disposition == "invalid"
    assert owned.session_id is None
    assert owned.harness is None


def test_canonical_vendor_contradiction_refuses_instead_of_precedence_guessing():
    env = {
        "FNO_HARNESS_NAME": "claude",
        "FNO_HARNESS_SESSION_ID": "11111111-1111-4111-8111-111111111111",
        "CODEX_THREAD_ID": "22222222-2222-4222-8222-222222222222",
    }

    assert resolve_harness_identity(env) == HarnessIdentity(None, None)
    owned = resolve_owned_identity(env)
    assert owned.disposition == "contradiction"
    assert owned.session_id is None
    assert owned.harness is None


# ---- ambiguity-aware owned resolution (AC1-HP / AC2-HP / AC5-CON) --------


def test_owned_disagreement_degrades_without_proof():
    """AC1-HP core: a claude session with a foreign CODEX_THREAD_ID, and no
    proof available, degrades rather than guessing codex by precedence."""
    env = {"CODEX_THREAD_ID": "foreign", "CLAUDE_CODE_SESSION_ID": "mine"}
    owned = resolve_owned_identity(env)  # no prove/collide injected
    assert owned.disposition == "ambiguous"
    assert owned.session_id is None
    assert owned.harness is None
    assert ("CODEX_THREAD_ID", "codex", "foreign") in owned.markers_present
    assert ("CLAUDE_CODE_SESSION_ID", "claude", "mine") in owned.markers_present


def test_owned_disagreement_prefers_the_proven_marker():
    """AC1-HP: when one marker is provably this process's, the disagreeing set
    resolves to it, not to precedence."""
    env = {"CODEX_THREAD_ID": "foreign", "CLAUDE_CODE_SESSION_ID": "mine"}

    def prove(harness: str, sid: str) -> bool:
        return harness == "claude" and sid == "mine"

    owned = resolve_owned_identity(env, prove=prove)
    assert owned.disposition == "proven"
    assert owned.harness == "claude"
    assert owned.session_id == "mine"


def test_owned_multi_family_without_proof_degrades_not_fallback():
    """Two families, no prover: collision-elimination is unsafe without proof
    (it could reject the session's own row when the prover is unavailable and
    leave an inherited foreign marker as the winner), so even when one marker is
    owned by another live row the resolver degrades rather than stamp the other
    by elimination. The collision is still recorded for the event."""
    env = {"CODEX_THREAD_ID": "foreign", "CLAUDE_CODE_SESSION_ID": "mine"}

    def collide(harness: str, sid: str):
        return "owner" if sid == "foreign" else None

    owned = resolve_owned_identity(env, collide=collide)  # no prove
    assert owned.disposition == "ambiguous"
    assert owned.session_id is None
    assert owned.harness is None
    assert owned.rejected[0]["session_id"] == "foreign"


def test_ambient_identity_env_covers_direct_read_markers():
    """AC6-HP scrub coverage: the env list includes the legacy direct-read
    markers (CLAUDECODE_SESSION_ID, HERMES_SESSION_ID), not just the resolver
    tuple, so a module reading a marker directly is scrubbed too."""
    assert "CLAUDECODE_SESSION_ID" in AMBIENT_IDENTITY_ENV
    assert "HERMES_SESSION_ID" in AMBIENT_IDENTITY_ENV
    assert "CODEX_THREAD_ID" in AMBIENT_IDENTITY_ENV
    assert "CLAUDE_CODE_SESSION_ID" in AMBIENT_IDENTITY_ENV
    # Routing vars are NOT identity and must never be swept.
    assert "CLAUDE_CONFIG_DIR" not in AMBIENT_IDENTITY_ENV
    assert "ANTHROPIC_API_KEY" not in AMBIENT_IDENTITY_ENV


# ---- registry collision (AC3-ERR / AC4-ERR) ----------------------------


def _register(tmp_path, session_id, provider="codex", status="live"):
    from fno.agents.registry import register_existing_session

    reg = tmp_path / "agents.json"
    entry = register_existing_session(
        provider=provider, session_id=session_id, cwd="/x", registry_path=reg
    )
    if status != "live":

        data = read_registry_document(reg)[0]
        for row in data.get("agents", []):
            if row.get("name") == entry.name:
                row["status"] = status
        _replace_registry(reg, data)
    return entry.name, reg


def test_row_owning_session_id_finds_live_owner(tmp_path):
    """AC3-ERR: a live row owning a candidate id is contention for a caller
    that proves no binding; the collider returns the owner's name."""
    from fno.agents.registry import row_owning_session_id

    sid = "019fc87d-ddff-7c90-926a-6bdd7ebb186c"
    name, reg = _register(tmp_path, sid)
    assert (
        row_owning_session_id(sid, registry_path=reg, self_binding=None) == name
    )
    # A different id is free.
    assert (
        row_owning_session_id(
            "019fffff-0000-0000-0000-ffffffffffff",
            registry_path=reg,
            self_binding=None,
        )
        is None
    )
    # Case-insensitive UUID match.
    assert (
        row_owning_session_id(sid.upper(), registry_path=reg, self_binding=None)
        == name
    )


def test_row_owning_session_id_self_row_is_not_contention(tmp_path):
    """The spine: a live row matching the caller's own (harness, id) binding
    is the caller's OWN row, and self is never reported as an owner."""
    from fno.agents.registry import row_owning_session_id

    sid = "019fc87d-ddff-7c90-926a-6bdd7ebb186c"
    name, reg = _register(tmp_path, sid, provider="codex")
    assert (
        row_owning_session_id(
            sid, registry_path=reg, self_binding=("codex", sid)
        )
        is None
    )
    # The row exists; only the self verdict changed.
    assert (
        row_owning_session_id(sid, registry_path=reg, self_binding=None) == name
    )


def test_row_owning_session_id_walk_proven_dead_pid_row_is_self(tmp_path):
    """A restart re-registers the same row in place (same name, same session
    id, still live) and leaves its spawn-time pid dead. When the caller's walk
    names the row's harness family and the row pid is dead (or names this
    process's own harness ancestor), that row is the restarted session's own,
    never contention. Every self-blind shape survives: a walk that is silent
    or names another family, a row whose pid leg never resolved, and a live
    foreign pid all leave the row an owner (the round-1 P1 refusal)."""

    from fno.agents.registry import row_owning_session_id

    sid = "019fc87d-ddff-7c90-926a-6bdd7ebb186c"
    name, reg = _register(tmp_path, sid, provider="claude")

    def _set_pid(pid):
        data = read_registry_document(reg)[0]
        for row in data.get("agents", []):
            if row.get("harness_session_id") == sid:
                row["pid"] = pid
        _replace_registry(reg, data)

    _set_pid(99999999)
    assert (
        row_owning_session_id(
            sid, registry_path=reg, self_binding=None, walk_harness="claude"
        )
        is None
    )
    # A silent or foreign walk keeps the refusal.
    assert (
        row_owning_session_id(
            sid, registry_path=reg, self_binding=None, walk_harness=None
        )
        == name
    )
    assert (
        row_owning_session_id(
            sid, registry_path=reg, self_binding=None, walk_harness="codex"
        )
        == name
    )
    # A row whose pid leg never resolved keeps the refusal (fail-closed).
    _set_pid(None)
    assert (
        row_owning_session_id(
            sid, registry_path=reg, self_binding=None, walk_harness="claude"
        )
        == name
    )
    # A live foreign pid keeps the refusal: a live session owns the id.
    _set_pid(1)
    assert (
        row_owning_session_id(
            sid, registry_path=reg, self_binding=None, walk_harness="claude"
        )
        == name
    )


def test_row_owning_session_id_absent_or_unreadable_is_none(tmp_path):
    """AC4-ERR: an absent or unreadable registry returns None (cannot prove a
    collision) and never raises, so an unreadable registry never blocks init."""
    from fno.agents.registry import row_owning_session_id

    assert (
        row_owning_session_id(
            "019fc87d-...", registry_path=tmp_path / "nope.json", self_binding=None
        )
        is None
    )
    corrupt = tmp_path / "agents.json"
    corrupt.write_text("{not valid json")
    assert (
        row_owning_session_id(
            "019fc87d-...", registry_path=corrupt, self_binding=None
        )
        is None
    )
    assert row_owning_session_id("", registry_path=corrupt, self_binding=None) is None


def test_resolve_owned_rejects_a_live_rows_id_via_real_collider(tmp_path):
    """AC3-ERR end-to-end: the owned resolver, wired to the real collider against
    a temp registry, refuses a foreign id a live row owns and records the owner,
    falling through to the claude marker instead of guessing codex."""
    from fno.agents.registry import row_owning_session_id

    foreign = "019fc87d-ddff-7c90-926a-6bdd7ebb186c"
    owner, reg = _register(tmp_path, foreign)

    def collide(harness, sid):
        return row_owning_session_id(sid, registry_path=reg, self_binding=None)

    env = {"CODEX_THREAD_ID": foreign, "CLAUDE_CODE_SESSION_ID": "mine"}
    owned = resolve_owned_identity(
        env, prove=lambda h, s: h == "claude", collide=collide
    )
    assert owned.harness == "claude"
    assert owned.session_id == "mine"
    assert len(owned.rejected) == 1
    assert owned.rejected[0]["session_id"] == foreign
    assert owned.rejected[0]["owner"] == owner


def test_owned_proven_marker_wins_over_self_registration_collision(tmp_path):
    """A session that has registered its OWN marker as a live row must not reject
    that marker as a collision. Proof runs before collision, so the proven claude
    marker wins even when a live row owns it (the session's own row), and an
    unregistered foreign CODEX_THREAD_ID never becomes the fallback. This is the
    inverse of the incident: without proof-first, the self-row collision would
    reject claude and leave the foreign marker as the sole survivor."""
    from fno.agents.registry import register_existing_session, row_owning_session_id

    mine = "aaaa1111-mine-mine-mine-aaaaaaaaaaaa"
    foreign = "019fc87d-ddff-7c90-926a-6bdd7ebb186c"
    reg = tmp_path / "agents.json"
    register_existing_session(
        provider="claude", session_id=mine, cwd="/x", registry_path=reg
    )

    def collide(harness, sid):
        return row_owning_session_id(sid, registry_path=reg, self_binding=None)

    env = {"CODEX_THREAD_ID": foreign, "CLAUDE_CODE_SESSION_ID": mine}
    owned = resolve_owned_identity(
        env, prove=lambda harness, sid: harness == "claude", collide=collide
    )
    assert owned.disposition == "proven"
    assert owned.harness == "claude"
    assert owned.session_id == mine
    # The session's own row was NOT rejected: proof skipped its collision check.
    assert owned.rejected == ()


def test_owned_lone_foreign_marker_is_not_stamped_when_prover_contradicts():
    """The single-family fast path used to stamp the only marker without any
    ownership check, so a claude hook carrying only an inherited
    CODEX_THREAD_ID recorded the foreign codex identity. A prover that resolves
    to a different harness (False) excludes the lone foreign marker and the
    result degrades to None rather than stamping a stranger's id."""
    env = {"CODEX_THREAD_ID": "foreign"}
    owned = resolve_owned_identity(env, prove=lambda harness, sid: False)
    assert owned.disposition == "ambiguous"
    assert owned.session_id is None
    assert owned.harness is None


def test_owned_proven_codex_family_prefers_thread_id_across_families():
    """The same preference when a foreign family rides along: the thread id
    still names this process, and the multi-family shape reads `proven`."""
    root = "019cc081-de0d-7283-97cc-751c46742a07"
    thread = "019cc082-1111-7283-97cc-751c46742a08"
    env = {
        "CODEX_SESSION_ID": root,
        "CODEX_THREAD_ID": thread,
        "CLAUDE_CODE_SESSION_ID": "inherited-claude",
    }
    owned = resolve_owned_identity(env, prove=lambda harness, _sid: harness == "codex")
    assert (owned.session_id, owned.harness, owned.disposition) == (
        thread,
        "codex",
        "proven",
    )


def test_sync_harness_aliases_unknown_suppresses_legacy_backfill():
    """An explicit harness=unknown must not backfill harness_session_id from a
    legacy marker. The manifest records an inherited CODEX_THREAD_ID additively
    for diagnosis; without this guard, _backfill_harness would resurrect it as
    the session's identity, undoing the whole fix."""
    from fno.harness_identity import sync_harness_aliases

    keys = {"claude": "claude_session_id", "codex": "codex_thread_id"}
    data = {
        "harness": "unknown",
        "harness_session_id": None,
        "codex_thread_id": "019fc87d-ddff-7c90-926a-6bdd7ebb186c",
        "claude_session_id": None,
    }
    out = sync_harness_aliases(dict(data), keys)
    assert out["harness_session_id"] in (None, "", "null")
    assert out["codex_thread_id"] == "019fc87d-ddff-7c90-926a-6bdd7ebb186c"

    # A KNOWN harness still backfills from its own legacy key (unchanged).
    known = sync_harness_aliases(
        {"harness": "codex", "harness_session_id": None, "codex_thread_id": "thread"}, keys
    )
    assert known["harness_session_id"] == "thread"


def test_current_session_helpers_refuse_mixed_and_fall_back_to_legacy():
    """One family agreeing on one id: the id resolves. Two ids of one family
    disagree: unresolved (x-0992). Mixed canonical families: the id helper
    refuses like the resolver instead of laundering the claude-legacy marker
    (x-b57a); the ids helper still enumerates every marker, its own job."""
    one_family = {"CODEX_THREAD_ID": " thread ", "CODEX_SESSION_ID": " thread "}
    assert current_session_id(one_family) == "thread"
    assert current_session_ids(one_family) == {"thread"}
    disagree = {"CODEX_THREAD_ID": " thread ", "CODEX_SESSION_ID": "session"}
    assert current_session_id(disagree) is None
    assert current_session_ids(disagree) == {"thread", "session"}
    mixed = {
        "CODEX_THREAD_ID": " thread ",
        "CLAUDE_CODE_SESSION_ID": "claude",
        "CLAUDE_SESSION_ID": "legacy",
    }
    assert current_session_id(mixed) is None
    assert current_session_ids(mixed) == {"thread", "claude", "legacy"}
    # No canonical marker at all: the pre-migration claude fallback fires.
    assert current_session_id({"CLAUDE_SESSION_ID": " legacy "}) == "legacy"
    assert current_session_ids({}) == set()


def test_legacy_suffix_handle_is_last_eight_read_only_lookup():
    """The retired last-eight address survives only as a read-only lookup so
    pre-flip handles (e.g. a lead addressed 08e8c104) still drain. It is never
    generated for new mail and is NOT the canonical address."""
    assert legacy_suffix_handle("019f48e1-5b09-72a0-9bc8-6b364bcf4ae4") == "4bcf4ae4"
    assert legacy_suffix_handle("019F48E1-5B09-72A0-9BC8-6B364BCF4AE4") == "4bcf4ae4"
    assert legacy_suffix_handle("ses_7f3a9b2cAbCd1234") == "AbCd1234"
    sid = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    assert legacy_suffix_handle(sid) != canonical_handle(sid)


def test_session_identity_key_normalizes_uuid_case_but_preserves_opencode():
    assert (
        session_identity_key("019F48E1-5B09-72A0-9BC8-6B364BCF4AE4")
        == "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    )
    assert session_identity_key("ses_7f3a9b2cAbCd1234") == "ses_7f3a9b2cAbCd1234"


def test_legacy_handle_re_matches_a_retired_provider_address():
    """The pattern recognizes a retired provider address so callers can
    refuse or report it by name - never accept one."""
    assert LEGACY_HANDLE_RE.fullmatch("codex-019f48e1")


def test_ac1_fr_registry_name_equals_canonical_handle(tmp_path):
    """The registry row name a session registers under MUST equal the handle a
    sender resolves and the drain reads, or a queued message strands. Assert the
    registry derives its name via the same shared function (drift fails CI)."""
    from fno.agents.registry import register_existing_session

    sid = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    entry = register_existing_session(
        provider="codex",
        session_id=sid,
        cwd="/tmp",
        registry_path=tmp_path / "agents.json",
    )
    assert entry.name == canonical_handle(sid) == "019f48e1"


def test_no_generating_surface_produces_a_retired_address(tmp_path, monkeypatch):
    """Every surface that MINTS an address must produce the bare form.

    The retired `<harness>-<short8>` is refused on the read side, but a refusal is
    only a backstop - the real fix is that nothing mints one. That was prose in a
    commit message until this test; now a reintroduced generator fails CI instead
    of quietly writing mail nothing can drain.
    """
    from fno.agents.registry import register_existing_session
    from fno.agents.self_stamp import stamp_from
    from fno.mail.envelope import wrap_fno_mail
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    sid = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    for marker, _ in HARNESS_SESSION_MARKERS:
        monkeypatch.delenv(marker, raising=False)
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", sid)

    minted = [
        canonical_handle(sid),
        stamp_from(None),
        register_existing_session(
            provider="claude", session_id=sid, cwd="/tmp",
            registry_path=tmp_path / "agents.json",
        ).name,
    ]
    for value in minted:
        assert not LEGACY_HANDLE_RE.match(value), f"{value!r} is a retired address"

    # Resolve the registered address to its wire identity; the delivered
    # header names the sender's short 8-hex handle and mints its own id.
    body = wrap_fno_mail(
        "hi", from_=stamp_from(None), to=canonical_handle(sid), id="fmail-" + "0" * 12,
    )
    assert body.splitlines()[0].startswith("`@019f48e1 · fmail-"), body[:80]


def test_strip_flags_keep_fno_plumbing():
    """The self-rescue strip set is foreign HARNESS families only. TARGET_SESSION_ID
    is this run's own claim linkage (the resolver never consults it), so a strip
    line that includes it would tell the operator to break their own run's claim
    matching while curing nothing."""
    from fno.harness_identity import ambient_identity_strip_flags

    env = {
        "CLAUDE_CODE_SESSION_ID": "own",
        "CODEX_THREAD_ID": "foreign",
        "CODEX_CI": "1",
        "TARGET_SESSION_ID": "my-run",
    }
    flags = ambient_identity_strip_flags("claude", env)
    assert "CODEX_THREAD_ID" in flags and "CODEX_CI" in flags
    assert "TARGET_SESSION_ID" not in flags
    assert "CLAUDE_CODE_SESSION_ID" not in flags


def test_strip_flags_skip_companion_path_embedding_keep_session_id():
    """A transcript PATH names this session by embedding its id (x-a0cd).

    The claude-side codex plugin exports CODEX_COMPANION_TRANSCRIPT_PATH
    pointing at THIS session's transcript under claude's own store. x-38c6's
    full-value equality missed it because the path embeds the id as a
    substring rather than equaling it, so whoami read a companion pointer as
    foreign lineage and prescribed a remedy that breaks the plugin's pointer.
    """
    from fno.harness_identity import ambient_identity_strip_flags

    env = {
        "CLAUDE_CODE_SESSION_ID": "own-session",
        "CODEX_COMPANION_SESSION_ID": "own-session",
        "CODEX_COMPANION_TRANSCRIPT_PATH": (
            "/Users/x/.claude/projects/-Users-x-repo/own-session.jsonl"
        ),
    }

    assert ambient_identity_strip_flags("claude", env) == []


def test_strip_flags_still_reports_foreign_value_without_keep_session_id():
    """No proven keep id: nothing reads as self-referential (fail closed)."""
    from fno.harness_identity import ambient_identity_strip_flags

    env = {
        "CODEX_COMPANION_SESSION_ID": "own-session-is-embedded-here",
    }

    assert ambient_identity_strip_flags("claude", env) == [
        "-u",
        "CODEX_COMPANION_SESSION_ID",
    ]


# --- attester identity: bound to the emitting process -------------------------


def test_attester_witness_env_only_when_no_family_carrier():
    from fno.harness_identity import _attester_witness

    # Carriers that agree but are not family processes (shells, the CLI
    # python) corroborate nothing on their own: no family carrier, env_only -
    # the daemon-carrier lane must degrade, never wedge or over-certify.
    assert (
        _attester_witness("CODEX_THREAD_ID", "sess-a", ["sess-a", None], [False, False])
        == "env_only"
    )
    assert _attester_witness("CODEX_THREAD_ID", "sess-a", [None, None], [False, False]) == "env_only"
    assert _attester_witness("CODEX_THREAD_ID", "sess-a", [], []) == "env_only"


def test_attester_witness_raises_on_differing_family_ancestor():
    from fno.harness_identity import AttesterIdentityConflict, _attester_witness

    with pytest.raises(AttesterIdentityConflict) as exc:
        _attester_witness("CODEX_THREAD_ID", "sess-forged", ["sess-true"], [True])
    # Both ids are named: the refusal is the one place a reader can see which
    # session the harness says versus which the env claims.
    assert "sess-true" in str(exc.value)
    assert "sess-forged" in str(exc.value)


def test_attester_witness_raise_outranks_the_equal_match():
    """The override shape yields BOTH an equal carrier (the shell carrying the
    assignment, not a family process) and a differing family carrier (the
    harness above it, the only kind that can mint the id). Stopping at the
    first equal carrier would certify the forgery, so the family carrier's
    disagreement raises."""
    from fno.harness_identity import AttesterIdentityConflict, _attester_witness

    with pytest.raises(AttesterIdentityConflict):
        _attester_witness(
            "CODEX_THREAD_ID",
            "sess-forged",
            ["sess-forged", None, "sess-true"],
            [False, False, True],
        )


def test_a_stale_marker_above_the_session_never_vetoes():
    """The daemon-carrier lane: the session process (a family carrier) agrees
    with the env, and a LONG-LIVED ancestor above it retains a previous
    session's marker. The nearer family carrier decided; the stale value above
    was never the minter of this id, so the witness is process, not a refusal
    that would wedge every emit under that daemon."""
    from fno.harness_identity import _attester_witness

    assert (
        _attester_witness(
            "CODEX_THREAD_ID",
            "sess-now",
            ["sess-now", "sess-old"],
            [True, True],
        )
        == "process"
    )


def test_resolve_attester_identity_refuses_mixed_family_env():
    """Markers from two harness families: one is foreign and inherited. Empty
    rather than by precedence - picking either would launder or ignore a
    provably mixed env."""
    from fno.harness_identity import resolve_attester_identity

    env = {"CODEX_THREAD_ID": "codex-id", "CLAUDE_CODE_SESSION_ID": "claude-id"}
    assert resolve_attester_identity(env) == ("", "env_only")


def _proc_environ_strictly_readable() -> bool:
    """Whether this OS GUARANTEES reading an ancestor's environment: linux
    /proc does. darwin's `ps eww` is PARTIAL - it exposed the harness chain's
    markers in a live session yet showed nothing for other processes - so the
    darwin arms below accept the recorded outcome rather than demand one."""
    return Path("/proc/self/environ").exists()


def test_resolve_attester_identity_refuses_the_command_line_override():
    """The verbatim reproduction: a command-scoped `CODEX_THREAD_ID=<foreign>`
    assignment makes the emitting env disagree with the bash parent that
    inherited the harness's own value. The resolver must raise, naming both
    ids. Runs live where the OS exposes ancestor environments (linux CI); on
    darwin the same chain honestly resolves env_only, which that arm pins."""
    import subprocess
    import sys

    probe = (
        "from fno.harness_identity import AttesterIdentityConflict, "
        "resolve_attester_identity\n"
        "try:\n"
        "    print(resolve_attester_identity())\n"
        "except AttesterIdentityConflict as exc:\n"
        "    print('CONFLICT', exc)\n"
        "    raise SystemExit(3)\n"
    )
    import fno as _fno

    src_dir = str(Path(_fno.__file__).resolve().parents[1])
    # The parent must be a FAMILY carrier (a process whose argv0 names the
    # marker's harness), because the nearest family carrier is what decides:
    # `exec -a codex` renames the intermediate bash, so its argv0 carries the
    # family token while its env still holds the harness's own id. The `rc=$?
    # ... exit $rc` tail is load-bearing: a lone command gets tail-exec'd
    # (the leaf REPLACES the renamed bash and inherits its pid), leaving the
    # walk no family carrier at all - the pre-tail shape passed only via
    # full-argv matching, the exact artifact the argv0 rule removed. The tail
    # also re-exits with the leaf's status; a bare `; :` would swallow it.
    import shlex

    leaf = (
        f"CODEX_THREAD_ID=foreign-sess {sys.executable} -c "
        "'import os; exec(os.environ[\"FNO_IDENTITY_PROBE\"])'; "
        "rc=$?; :; exit \"$rc\""
    )
    wrapped = "exec -a codex bash -c " + shlex.quote(leaf)
    r = subprocess.run(
        ["bash", "-c", wrapped],
        env={
            **os.environ,
            "CODEX_THREAD_ID": "true-sess",
            "PYTHONPATH": src_dir,
            "FNO_IDENTITY_PROBE": probe,
        },
        capture_output=True,
        text=True,
    )
    if _proc_environ_strictly_readable():
        # linux: /proc answers for every ancestor, so the family carrier is
        # readable and its disagreement with the leaf's override must raise.
        assert r.returncode == 3, r.stdout + r.stderr
        assert "true-sess" in r.stdout and "foreign-sess" in r.stdout
    else:
        # darwin: `ps eww` readability is partial by measurement, so the
        # carrier is either readable (the disagreement raises) or not
        # (env_only). Both are honest. The one forbidden outcome on every
        # OS is the forged stamp: the override resolving with a `process`
        # witness, which is what full-argv family matching shipped.
        if r.returncode == 0:
            assert "env_only" in r.stdout, r.stdout + r.stderr
        else:
            assert r.returncode == 3, r.stdout + r.stderr
            assert "true-sess" in r.stdout and "foreign-sess" in r.stdout


def _replace_registry(path, document):
    from tests._table_seed import seed_registry

    document = dict(document)
    seed_registry(document.pop("agents"), path=path, replace=True, **document)
