"""Unit tests for fno.graph.slug - title-derived node handles (ab-f82e8083).

Pure functions; no I/O. Covers slug derivation, the empty-title hex fallback,
collision suffixing, the idempotent backfill pass, and display formatting,
mapped to the design's Failure Modes + Acceptance Criteria.
"""
from __future__ import annotations

from tests.fixtures.graph_seed import seed_graph

from fno.graph.slug import (
    assign_unique_slug,
    derive_base_slug,
    ensure_slugs,
    format_handle,
)


# -- derive_base_slug --------------------------------------------------------


def test_derive_lowercases_and_collapses_punctuation():
    # Runs of non-alphanumerics collapse to a single hyphen; leading/trailing
    # hyphens are stripped.
    assert derive_base_slug("  /agents: spawn!!  ") == "agents-spawn"


def test_derive_drops_small_stopwords_but_never_empties():
    # "for" is a stopword and is dropped; the meaningful words remain.
    assert derive_base_slug("Resolution for agents") == "resolution-agents"
    # A title made ENTIRELY of stopwords keeps the words rather than emptying.
    assert derive_base_slug("the of and") == "the-of-and"


# -- assign_unique_slug ------------------------------------------------------


def test_assign_suffixes_on_collision():
    taken = {"docs-refactor"}
    assert assign_unique_slug("docs-refactor", "ab-11111111", taken) == "docs-refactor-2"
    taken.add("docs-refactor-2")
    assert assign_unique_slug("docs-refactor", "ab-22222222", taken) == "docs-refactor-3"


# -- ensure_slugs (backfill pass) --------------------------------------------


def test_ensure_is_idempotent_and_immutable():
    # An entry already carrying a slug is NEVER changed, even if its title
    # would now derive a different slug (immutability invariant).
    entries = [{"id": "ab-aaaaaaaa", "title": "Renamed Title", "slug": "original-handle"}]
    count = ensure_slugs(entries)
    assert count == 0
    assert entries[0]["slug"] == "original-handle"
    # Re-running changes nothing.
    assert ensure_slugs(entries) == 0


def test_ensure_two_fresh_collisions_get_distinct_slugs():
    # Concurrency invariant (single-pass form): two fresh nodes with the same
    # base slug both get assigned under one pass -> distinct slugs.
    entries = [
        {"id": "ab-aaaaaaaa", "title": "Docs Refactor"},
        {"id": "ab-bbbbbbbb", "title": "Docs Refactor"},
    ]
    ensure_slugs(entries)
    assert entries[0]["slug"] == "docs-refactor"
    assert entries[1]["slug"] == "docs-refactor-2"
    assert entries[0]["slug"] != entries[1]["slug"]


def test_ensure_empty_title_node_gets_hex_handle():
    entries = [{"id": "ab-1234abcd", "title": ""}]
    ensure_slugs(entries)
    assert entries[0]["slug"] == "node-1234abcd"


# -- format_handle -----------------------------------------------------------


def test_format_handle_leads_with_slug():
    node = {"id": "ab-994222ee", "slug": "dashless-spawn"}
    assert format_handle(node) == "dashless-spawn (ab-994222ee)"


# -- integration: commit_rows_via_store assigns slugs --------------------------


def test_locked_mutate_assigns_slugs_to_all_nodes(tmp_path):
    """Every persisted mutation slugs both legacy and freshly-appended nodes,
    and a re-mutation does not rewrite the already-assigned handles."""
    from fno.graph.store import commit_rows_via_store

    p = tmp_path / "graph.json"
    seed_graph(p, [{"id": "ab-aaaaaaaa", "title": "Hello World"}])

    def add_one(entries):
        entries.append({"id": "ab-bbbbbbbb", "title": "Second Node"})
        return entries

    out = commit_rows_via_store(p, add_one)
    by_id = {e["id"]: e for e in out}
    assert by_id["ab-aaaaaaaa"]["slug"] == "hello-world"
    assert by_id["ab-bbbbbbbb"]["slug"] == "second-node"

    # A later no-op mutation leaves the handles immutable.
    out2 = commit_rows_via_store(p, lambda e: e)
    by_id2 = {e["id"]: e for e in out2}
    assert by_id2["ab-aaaaaaaa"]["slug"] == "hello-world"
    assert by_id2["ab-bbbbbbbb"]["slug"] == "second-node"
