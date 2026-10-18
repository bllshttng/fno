"""Unit tests for fno.graph.fuzzy - resolve_id and suggest_domain.

Pure functions; no I/O. Each test builds a list[dict] fixture and calls
the function directly, per the BDD scenarios in Phase 02 of the plan.
"""
from __future__ import annotations


from fno.graph.fuzzy import (
    _branch_tokens,
    resolve_id,
    suggest_domain,
)


# -- helpers --


def _entry(id: str, title: str, *, status: str = "ready", domain: str = "code") -> dict:
    return {
        "id": id,
        "title": title,
        "status": status,
        "domain": domain,
    }


# -- resolve_id: exact match --


def test_resolve_id_refuses_flag_shaped_query_before_the_graph():
    """A leaked flag is never a node id, and must not title-fuzzy-match."""
    entries = [_entry("ab-00000001", "make --strict the default")]
    for q in ("--strict", "-strict", " -strict"):
        result = resolve_id(q, entries)
        assert result.kind == "none", q
        assert result.id is None


def test_exact_configured_prefix_id_resolves():
    """ab-bbfccb8f: an exact id under a configured (non-ab) prefix/width resolves
    via the format-agnostic exact match (a graph lookup, not an ab- regex)."""
    entries = [_entry("fno-a3f9", "Configured node"), _entry("ab-55ba9adb", "Legacy")]
    result = resolve_id("fno-a3f9", entries)
    assert result.kind == "exact"
    assert result.id == "fno-a3f9"
    # And a legacy id still resolves in the same mixed-format graph.
    assert resolve_id("ab-55ba9adb", entries).kind == "exact"


def test_configured_id_not_an_entry_does_not_fuzzy_collapse():
    """A configured-shaped query that is NOT a graph id must not fuzzy-collapse
    onto an unrelated title (it has no exact hit and is not an ab- prefix)."""
    entries = [_entry("fno-a3f9", "Configured node")]
    result = resolve_id("xy-dead", entries)
    assert result.kind == "none"


# -- resolve_id: ab-id prefix matching (4-7 hex chars after ab-) --


def test_resolve_id_prefix_unique():
    """Partial ab-id (4-7 hex chars) with one match resolves as kind='fuzzy'."""
    entries = [
        _entry("ab-9728b70b", "Provider rotation: failover"),
        _entry("ab-aaaa0001", "Other work"),
    ]
    result = resolve_id("ab-9728", entries)
    assert result.kind == "fuzzy"
    assert result.id == "ab-9728b70b"


def test_resolve_id_prefix_ambiguous():
    """Partial ab-id with multiple matches yields kind='ambiguous'."""
    entries = [
        _entry("ab-abcd1111", "Plan A"),
        _entry("ab-abcd2222", "Plan B"),
        _entry("ab-ffff0000", "Unrelated"),
    ]
    result = resolve_id("ab-abcd", entries)
    assert result.kind == "ambiguous"
    assert len(result.candidates) == 2
    ids = {e["id"] for e in result.candidates}
    assert ids == {"ab-abcd1111", "ab-abcd2222"}


def test_resolve_id_malformed_ab_returns_none_not_title_fuzzy():
    """Malformed ab- queries (trailing comma, non-hex chars, etc.) explicitly
    return kind='none' rather than falling through to title fuzzy.

    Catches the silent-failure case where 'ab-9728b70b,' could fuzzy-match
    onto a title that mentions the literal id substring. The 'ab-' prefix
    is a strong user signal that they want id resolution; a malformed
    suffix should be a clear error, not an implicit re-routing.
    """
    entries = [
        _entry("ab-9728b70b", "Failover plan referencing ab-9728b70b inline"),
    ]
    result = resolve_id("ab-9728b70b,", entries)
    assert result.kind == "none"
    assert "malformed" in (result.note or "").lower()


# -- resolve_id: fuzzy match --


def test_fuzzy_prefers_non_done_matches():
    """Non-done matches preferred over done matches when both exist."""
    entries = [
        _entry("ab-done00001", "Ingestion framework pilot", status="done"),
        _entry("ab-open00002", "Ingestion framework rollout"),
    ]
    result = resolve_id("ingestion framework", entries)
    assert result.kind == "fuzzy"
    assert result.id == "ab-open00002"


# -- resolve_id: branch derivation --


def test_scenario3_edge_branch_derivation_picks_obvious_match():
    """Scenario 3 (EDGE): git-branch tokens resolve to the obvious match."""
    entries = [
        _entry("ab-tot000001", "Implement tot init command"),
        _entry("ab-foo000001", "Unrelated work"),
    ]
    result = resolve_id(None, entries, git_branch="feat/tot-init-first-run-ux")
    assert result.kind == "branch_derived"
    assert result.id == "ab-tot000001"


def test_branch_derivation_drops_short_and_numeric_tokens():
    """Tokens <= 2 chars or all-digits are dropped."""
    assert _branch_tokens("feat/ab-cd-ef-ux-long-name") == ["long", "name"]
    assert _branch_tokens("fix/123-very-long-story") == ["very", "long", "story"]


# -- resolve_id: ambiguity and no-match --


def test_scenario4_err_ambiguous_returns_candidates():
    """Scenario 4 (ERR): ambiguous query returns all candidates."""
    entries = [
        _entry("ab-aa000001", "Plan 01: one"),
        _entry("ab-bb000002", "Plan 02: two"),
    ]
    result = resolve_id("plan", entries)
    assert result.kind == "ambiguous"
    assert len(result.candidates) == 2
    ids = {e["id"] for e in result.candidates}
    assert ids == {"ab-aa000001", "ab-bb000002"}


# -- suggest_domain --


def test_scenario6_hp_suggest_domain_new_with_history():
    """Scenario 6 (HP): non-matching query yields new + history preserved."""
    entries = [
        _entry("ab-aa000001", "T1", domain="code"),
        _entry("ab-aa000002", "T2", domain="code"),
    ]
    result = suggest_domain("research", entries)
    assert result.confidence == "new"
    assert result.match == "research"
    assert "code" in result.history


def test_scenario7_edge_suggest_domain_fuzzy_prefix():
    """Scenario 7 (EDGE): unique prefix match resolves fuzzy."""
    entries = [
        _entry("ab-aa000001", "T1", domain="research"),
        _entry("ab-aa000002", "T2", domain="code"),
    ]
    result = suggest_domain("res", entries)
    assert result.confidence == "fuzzy"
    assert result.match == "research"


def test_scenario8_edge_suggest_domain_ambiguous_prefix_stays_new():
    """Scenario 8 (EDGE): ambiguous prefix falls back to 'new' with input verbatim."""
    entries = [
        _entry("ab-aa000001", "T1", domain="research"),
        _entry("ab-aa000002", "T2", domain="retrospective"),
    ]
    result = suggest_domain("re", entries)
    assert result.confidence == "new"
    assert result.match == "re"


def test_suggest_domain_empty_history_empty_query_defaults_code():
    """Empty history and empty query -> safe default 'code' as new."""
    result = suggest_domain("", [])
    assert result.match == "code"
    assert result.confidence == "new"
    assert result.history == ()


# -- Dataclass sanity --
