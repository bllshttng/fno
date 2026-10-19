"""Unit tests for fno.graph._intake._find_node, including the
prefix-aware wrapper added by ab-7651a2c6 (fuzzy resolver).

The wrapper routes partial ab- IDs (less than 11 chars total: 'ab-' + <8 hex)
through resolve_id so callers like `fno backlog done ab-9728` work without
the user having to type the full 8 hex chars. Full ab-XXXXXXXX queries keep
the fast equality path; non-ab inputs are unchanged.
"""
from __future__ import annotations

from fno.graph._intake import _find_node


def _entry(id: str, title: str = "T", *, status: str = "ready") -> dict:
    return {"id": id, "title": title, "status": status}


# -- regression: full-id behavior must not change --


def test_find_node_full_id_match_returns_entry():
    entries = [_entry("ab-9728b70b"), _entry("ab-aaaa0001")]
    assert _find_node(entries, "ab-9728b70b") == entries[0]


# -- new: prefix-aware lookup for partial ab- IDs --


def test_find_node_prefix_unique_match_returns_entry():
    entries = [
        _entry("ab-9728b70b", "Failover"),
        _entry("ab-aaaa0001", "Other"),
    ]
    assert _find_node(entries, "ab-9728") == entries[0]


def test_find_node_prefix_ambiguous_writes_stderr(capsys):
    """Ambiguous prefix should name the candidate IDs on stderr so the user
    can disambiguate, rather than seeing only the caller's 'no such node'.
    """
    entries = [
        _entry("ab-abcd1111"),
        _entry("ab-abcd2222"),
    ]
    result = _find_node(entries, "ab-abcd")
    assert result is None
    captured = capsys.readouterr()
    assert "ambiguous" in captured.err
    assert "ab-abcd1111" in captured.err
    assert "ab-abcd2222" in captured.err
