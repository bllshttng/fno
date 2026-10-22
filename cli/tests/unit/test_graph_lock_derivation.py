"""The graph flock is derived from the graph path, not a global /tmp constant.

Proves the invariant x-fb80 establishes: same graph file <-> one resolved
sibling lock, so concurrent writers to one graph serialize (no lost update)
and a scratch graph never contends with the real ~/.fno/graph.json.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import threading
import time
from pathlib import Path


from fno.graph.store import commit_rows_via_store
from fno.graph.store import read_graph_strict


# --- AC2: the lock sits beside graph.json, resolved, never in /tmp ---


# --- AC5: a resolve failure degrades to the raw path, never crashes ---
# A symlink loop raises OSError (ELOOP) or RuntimeError depending on the Python
# version (3.13 raises neither, so a real loop can't exercise this branch there);
# mock both so the degrade path is proven portably.


# --- AC1: two concurrent writers to the SAME graph both land (no lost update) ---

def test_concurrent_writers_serialize(tmp_path):
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')

    def append(node_id):
        def mutator(entries):
            # Widen the read-modify-write window: without a mutually exclusive
            # lock, the second writer reads before the first writes and clobbers
            # it. Under the derived sibling lock the whole block is serialized.
            base = list(entries)
            time.sleep(0.05)
            base.append({"id": node_id, "title": node_id, "status": "idea"})
            return base
        return mutator

    # A writer that exhausts its retries raises in ITS thread; capture that
    # or the failure reads as a lost update instead of the real conflict.
    failures: list[tuple[str, BaseException]] = []

    def run(i: int) -> None:
        try:
            commit_rows_via_store(g, append(f"x-{i}"))
        except BaseException as exc:  # noqa: BLE001 - re-raised below
            failures.append((f"x-{i}", exc))

    threads = [threading.Thread(target=run, args=(i,)) for i in range(2)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    ids = {e["id"] for e in read_graph_strict(g)}
    assert not failures, f"writer died: {failures}"
    assert ids == {"x-0", "x-1"}, f"lost update: {ids}"
