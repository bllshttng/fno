"""collision.partition: one file-overlap grouping at task grain.

The union-find under the Python collision engine died with that engine
(the graph CLI's collisions sub-app and triage health went native; the
decision lives in crates/fno-agents/src/backlog/collision.rs). The
orchestrator's ``partition_edges`` (task grain) is the live consumer.
"""
from __future__ import annotations

from fno.graph.collision import partition


class TestPartition:
    def test_shared_path_groups_overlap(self):
        # AC1-HP: A and C share x.py; B is disjoint.
        groups, unevaluated = partition(
            [("A", {"x.py"}), ("B", {"y.py"}), ("C", {"x.py", "z.py"})]
        )
        assert groups == [{"A", "C"}, {"B"}]
        assert unevaluated == set()

    def test_empty_path_set_is_singleton_and_unevaluated(self):
        # AC1-ERR: no parseable file list is its own verdict, never a silent pass.
        groups, unevaluated = partition([("D", set()), ("E", {"a.py"})])
        assert groups == [{"D"}, {"E"}]
        assert unevaluated == {"D"}


    def test_shared_output_root_groups_disjoint_paths(self):
        groups, _ = partition(
            [("A", {".codex/agents/target.toml"}), ("B", {".codex/agents/review.toml"})],
            shared_roots=(".codex/agents/",),
        )
        assert groups == [{"A", "B"}]


    def test_repeated_item_id_keeps_its_unions(self):
        # Two path batches for one id: the second must extend the first's
        # group, not reset the item back to a singleton.
        groups, unevaluated = partition(
            [("A", {"x.py"}), ("B", {"y.py"}), ("A", {"y.py"})]
        )
        assert groups == [{"A", "B"}]
        assert unevaluated == set()
