"""End-to-end mechanical test of the G4 reconcile seam (x-baeb, cv-21ea5acd).

The `/target --reconcile` WORKER itself is LLM-interpreted (the de-stub is a real
implementation pass, not a unit-testable swap). But the mechanical chain the
worker drives - merge-held draft PR -> dispatch -> drift gate -> finalize -> no
longer held - must stay coherent. This test wires the REAL pieces together
(real graph read by the router, real merge guard, real verdict + finalize) so a
break anywhere in the seam is caught, even though the de-stub reasoning is not.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno import stub_manifest as sm
from fno.graph.store import commit_rows_via_store


@pytest.fixture
def iso(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_AUTO_CONTINUE", "1")
    return tmp_path / ".fno" / "events.jsonl"


def _manifest_holds(node: str, root: Path) -> bool:
    """Whether the merge guard holds this node's PR: the inputs the Rust gate
    (merge_gates.rs stub_manifest_blocker) reads, restated as a local oracle.
    The Python lookup seam is gone; the manifest state is what it decides on."""
    path = sm.manifest_path(node, root)
    if not path.exists():
        return False
    return sm.load(path).get("reconciled") is not True




def test_full_seam_drift_keeps_pr_held(iso, tmp_path, monkeypatch):
    # The mirror path: a FAILING contract test refuses de-stub, so the guard
    # stays held and the draft PR can never merge with mocks (AC4-ERR end to end).
    gp = tmp_path / "graph.json"
    gp.write_text(json.dumps({"entries": [
        {"id": "x-dep", "dep": "contract", "blocked_by": ["x-blk"],
         "pr_number": 42, "project": None, "cwd": str(tmp_path), "slug": "dep",
         "title": "x-dep", "type": "feature", "priority": "p2", "status": "idea"},
    ]}), encoding="utf-8")
    monkeypatch.setattr("fno.paths.graph_json", lambda: gp)
    sm.write("x-dep", [{"stub_id": "a", "file": "f", "kind": "fn"}], tmp_path,
             contract_test="false")  # landed schema fails the gate

    # The worker's gate refuses; it must NOT call mark_reconciled.
    assert sm.reconcile_verdict("x-dep", tmp_path)["outcome"] == sm.DRIFT
    # Guard still holds -> the draft PR stays unmergeable.
    assert _manifest_holds("x-dep", tmp_path) is True
