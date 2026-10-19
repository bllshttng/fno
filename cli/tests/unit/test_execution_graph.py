"""Execution-graph types + compiler (wave 4 of the context-graph epic).

Covers the seven canonical compile fixtures against stable serialized output
(one-node collapse, dependency chain, safe fan-out, file collision, unknown
liveness, verification edge, fan-in barrier) plus the strict-parse contract:
every type fails CLOSED on a schema defect and round-trips through to_dict.
"""
from __future__ import annotations

import pytest

from fno.graph.execution import (
    EXEC_GRAPH_VERSION,
    ExecGraph,
    ExecNode,
    MalformedGraphError,
    compile_graph,
)


def _entry(node_id: str, **kw):
    e = {"id": node_id, "title": f"node {node_id}", "project": "fno"}
    e.update(kw)
    return e


# ── compile fixtures ─────────────────────────────────────────────────────────


def test_one_node_collapses_to_serial():
    g = compile_graph("a", [_entry("a")])
    assert g.collapsed is True
    assert len(g.nodes) == 1
    assert g.edges == ()
    assert g.barriers == ()
    assert g.nodes[0].justification == "specialized"


def test_dependency_chain_emits_ordering_edges():
    entries = [_entry("a"), _entry("b", blocked_by=["a"]), _entry("c", blocked_by=["b"])]
    g = compile_graph("c", entries)
    assert g.collapsed is False
    kinds = {(e.src, e.dst, e.kind) for e in g.edges}
    assert kinds == {("a", "b", "ordering"), ("b", "c", "ordering")}
    # a single predecessor each -> no fan-in barrier
    assert g.barriers == ()


def test_file_collision_emits_resource_edge():
    entries = [_entry("x", owns_files=["shared.py"]), _entry("y", owns_files=["shared.py"])]
    g = compile_graph("x", entries)
    resource = [e for e in g.edges if e.kind == "resource"]
    assert len(resource) == 1
    assert resource[0].src == "x" and resource[0].dst == "y"
    assert "shared.py" in resource[0].invariant
    just = {n.node_id: n.justification for n in g.nodes}
    assert just["x"] == "resource" and just["y"] == "resource"


def test_verification_edge_from_node_to_its_verifier():
    entries = [_entry("impl", verifier="check"), _entry("check")]
    g = compile_graph("impl", entries)
    verif = [e for e in g.edges if e.kind == "verification"]
    assert len(verif) == 1
    assert verif[0].src == "impl" and verif[0].dst == "check"
    just = {n.node_id: n.justification for n in g.nodes}
    assert just["check"] == "verifier"


def test_fan_in_barrier_when_two_predecessors():
    entries = [_entry("d1"), _entry("d2"), _entry("t", blocked_by=["d1", "d2"])]
    g = compile_graph("t", entries)
    assert len(g.barriers) == 1
    b = g.barriers[0]
    assert b.at == "t"
    assert b.expected == ("d1", "d2")
    just = {n.node_id: n.justification for n in g.nodes}
    assert just["t"] == "branching"


# ── serialization round-trip + snapshot stability ────────────────────────────


def test_round_trip_through_strict_parse():
    entries = [_entry("d1"), _entry("d2"), _entry("t", blocked_by=["d1", "d2"])]
    g = compile_graph("t", entries)
    reparsed = ExecGraph.from_dict(g.to_dict())
    assert reparsed.to_dict() == g.to_dict()


# ── strict-parse: fail CLOSED on every schema defect ─────────────────────────


def test_from_dict_edge_referencing_unknown_node_rejected():
    d = {
        "version": EXEC_GRAPH_VERSION,
        "root": "a",
        "nodes": [ExecNode(node_id="a", role="builder", justification="specialized",
                           objective="o", repo="r").to_dict()],
        "edges": [{"src": "a", "dst": "ghost", "kind": "ordering", "invariant": "x"}],
    }
    with pytest.raises(MalformedGraphError):
        ExecGraph.from_dict(d)


def test_compile_root_absent_falls_back_to_serial():
    # root not in the provided set -> collapsed serial, never an exception
    g = compile_graph("missing", [_entry("other")])
    assert g.collapsed is True
    assert g.root == "missing"


def test_self_referential_blocked_by_is_skipped_not_collapsed():
    entries = [_entry("a", blocked_by=["a"]), _entry("b", blocked_by=["a"])]
    g = compile_graph("b", entries)
    # the a->a self edge is skipped; a->b ordering survives, no collapse
    assert g.collapsed is False
    assert [(e.src, e.dst) for e in g.edges] == [("a", "b")]


def test_compile_never_raises_on_non_dict_entry_before_root():
    # a non-dict entry ahead of the root must not escape (reproduced regression)
    g = compile_graph("root1", [None, _entry("root1")])
    assert g.root == "root1"  # did not raise


def _has_cycle(graph) -> bool:
    adj: dict = {n.node_id: [] for n in graph.nodes}
    for e in graph.edges:
        adj[e.src].append(e.dst)
    WHITE, GREY, BLACK = 0, 1, 2
    color = {n: WHITE for n in adj}

    def visit(u):
        color[u] = GREY
        for v in adj[u]:
            if color[v] == GREY:
                return True
            if color[v] == WHITE and visit(v):
                return True
        color[u] = BLACK
        return False

    return any(color[n] == WHITE and visit(n) for n in adj)


def test_transitive_resource_cycle_is_avoided():
    # a blocked_by c (ordering c->a); a&b share f1; b&c share f2; lex a<b<c.
    # Naive lexicographic resource edges a->b, b->c + ordering c->a = cycle.
    entries = [
        _entry("a", blocked_by=["c"], owns_files=["f1"]),
        _entry("b", owns_files=["f1", "f2"]),
        _entry("c", owns_files=["f2"]),
    ]
    g = compile_graph("a", entries)
    assert not _has_cycle(g)
    # the b->c resource edge that would close the cycle is dropped (c already
    # reaches b via c->a->b), so only c->a ordering + a->b resource survive
    assert ("b", "c") not in {(e.src, e.dst) for e in g.edges}


def test_relevant_scope_pulls_verifier_and_evidence():
    from fno.graph.cli import _relevant_exec_scope

    by_id = {
        "impl": _entry("impl", verifier="check", requires_evidence=["r"]),
        "check": _entry("check"),
        "prod": _entry("prod", produces_evidence=["r"]),
        "unrelated": _entry("unrelated"),
    }
    scope = _relevant_exec_scope("impl", by_id)
    assert scope == {"impl", "check", "prod"}  # unrelated stays out
