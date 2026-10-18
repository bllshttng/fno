"""Unit tests for the deterministic relatedness map (node x-c2e9)."""

from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph


import pytest

from fno.graph import relatedness as R
from tests.conftest import run_native_create


def _node(nid, title="", domain=None, details="", slug="", **extra):
    e = {"id": nid, "title": title, "slug": slug}
    if details:
        e["details"] = details
    if domain is not None:
        e["domain"] = domain
    e.update(extra)
    return e


# --- AC1-HP: build writes a best-first map, zero-signal pairs absent ---


def test_build_map_scores_and_orders(tmp_path):
    entries = [
        _node("a", title="nightly backlog groomer relatedness", domain="code"),
        _node("b", title="nightly backlog groomer rank pass", domain="code"),
        _node("c", title="unrelated billing invoice export", domain="billing"),
    ]
    m = R.build_map(entries)
    # a and b share domain + many tokens -> related, both directions.
    assert any(r["id"] == "b" for r in m["a"])
    assert any(r["id"] == "a" for r in m["b"])
    # c shares nothing with a -> absent (zero-signal dropped).
    assert all(r["id"] != "c" for r in m["a"])
    # rows carry id/score/reason, best-first.
    top = m["a"][0]
    assert set(top) == {"id", "score", "reason"} and top["score"] > 0


# --- AC4-EDGE: empty / single-node graph is a clean no-op ---


# --- AC1-HP / AC2-EDGE: similar_nodes passes its floor into _score ---


def test_similar_nodes_floor_below_min_score_reaches_scorer():
    # No domain on either side: jac = 1/7 = 0.1429, below _MIN_SCORE (0.15).
    # Before this fix, _score's own default `minimum=_MIN_SCORE` clamped this
    # to 0.0 regardless of the caller's floor, so similar_nodes(floor=0.05)
    # dropped it too. Passing the floor through as `minimum` lets it stand.
    a = _node("aaa", title="alpha bravo charlie")
    b = _node("bbb", title="alpha delta echo foxtrot golf")
    zeroed = R._score(a, b, R._tokens(a), R._tokens(b), include_epic=False)[0]
    assert zeroed == 0.0
    raw = R._score(a, b, R._tokens(a), R._tokens(b), include_epic=False, minimum=0.0)[0]
    assert 0.05 < raw < 0.15
    assert [r[0] for r in R.similar_nodes(a, [b], floor=0.05)] == ["bbb"]


# --- AC3-ERR: no-map vs no-edges are distinguishable ---


def test_get_related_no_map_raises(tmp_path):
    with pytest.raises(R.NoMapError):
        R.get_related(tmp_path / "missing.json", "a")


def test_get_related_no_edges_returns_empty(tmp_path):
    p = tmp_path / "relatedness.json"
    R.write_map(p, {"a": []})
    assert R.get_related(p, "a") == []
    # unknown id in a present map is also just "no edges".
    assert R.get_related(p, "zzz") == []


# --- atomic write round-trips ---


# --- CLI boundary: build then get round-trip + exit-code contract (AC2/AC3) ---
# The wheel sub-app retired with the relatedness port; these exec the native
# binary over a fixture store, asserting over both streams like the
# create-port tests.


def _seed_store(tmp_path, entries):
    import json as _json

    graph = tmp_path / "graph.json"
    seed_graph(graph, _json.dumps({"entries": entries}))
    return graph


def test_cli_build_then_get(tmp_path):
    graph = _seed_store(
        tmp_path,
        [
            _node("aa-0001", title="nightly groomer relatedness", domain="code"),
            _node("aa-0002", title="nightly groomer rank", domain="code"),
        ],
    )
    build = run_native_create(graph, "relatedness", "build")
    assert build.exit_code == 0, build.output + build.stderr
    res = run_native_create(graph, "relatedness", "get", "aa-0001", "--json")
    assert res.exit_code == 0, res.output + res.stderr
    assert '"id": "aa-0002"' in (res.output + res.stderr)


def test_cli_get_no_map_exits_nonzero_empty(tmp_path):
    _seed_store(tmp_path, [])  # no build run -> no sidecar
    res = run_native_create(tmp_path / "graph.json", "relatedness", "get", "a")
    assert res.exit_code != 0
    assert res.output.strip() == ""
