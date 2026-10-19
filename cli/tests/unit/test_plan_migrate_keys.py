"""Tests for fno do plan migrate-keys (x-f34f US7).

Byte-preserving synonym-key collapse: graph_node_id->node, created_at->created,
depends_on->blocked_by, kind->type, and drop claims where it equals node.
"""
from __future__ import annotations

from pathlib import Path

from fno.plan.migrate_keys import migrate, migrate_text


def _plan(fm_lines: str, body: str = "\n# Title\n\nbody text\n") -> str:
    return f"---\n{fm_lines}\n---{body}"


def test_renames_all_legacy_keys_body_intact():
    text = _plan("graph_node_id: x-1\ncreated_at: 2026-07-08\ndepends_on: [x-2]\nkind: feature")
    out, notes = migrate_text(text)
    assert out is not None
    assert "\nnode: x-1\n" in out
    assert "\ncreated: 2026-07-08\n" in out
    assert "\nblocked_by: [x-2]\n" in out
    assert "\ntype: feature\n" in out
    assert "graph_node_id" not in out and "created_at" not in out
    assert out.endswith("\n# Title\n\nbody text\n")  # body byte-intact


def test_keeps_claims_that_differs_from_node():
    text = _plan("node: x-1\nclaims: x-2\nstatus: done")
    out, notes = migrate_text(text)
    # claims != node -> preserved, flagged for review, no rename churn -> no write.
    assert out is None
    assert any("review" in n for n in notes)


def test_no_rename_when_canonical_target_present():
    """kind + type both present -> keep kind (never create a duplicate type key)."""
    text = _plan("type: feature\nkind: task\nnode: x-1")
    out, notes = migrate_text(text)
    assert out is None  # nothing safely renamable
    assert any("kind (type also present)" in n for n in notes)


def test_sweep_apply_writes_and_summarizes(tmp_path: Path):
    (tmp_path / "a.md").write_text(_plan("graph_node_id: x-1\nkind: feature"))
    (tmp_path / "b.md").write_text(_plan("node: x-2\nclaims: x-2"))
    (tmp_path / "c.md").write_text(_plan("node: x-3\ntype: feature"))  # already canonical
    res = migrate(tmp_path, apply=True)
    assert res.migrated == 2 and res.skipped == 1
    assert "\nnode: x-1\n" in (tmp_path / "a.md").read_text()
    assert "claims" not in (tmp_path / "b.md").read_text()
    # Re-run is idempotent.
    res2 = migrate(tmp_path, apply=True)
    assert res2.migrated == 0
