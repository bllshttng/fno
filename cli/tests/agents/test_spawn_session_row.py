"""A node-bearing spawn opens a contributor session row.

A worker that never holds the claim crosses no stamping chokepoint (claim
acquire/release, plan-bind, PR-link). Reviews stay inline in the builder's
session and do not create review-worker rows. Coverage:

  - AC1: `spawn --node X --substrate bg` with a resolvable worker uuid opens a
    row carrying the WORKER's harness session id (never the spawner's, never
    the 8-hex short id). Phase: the message's command labels the row via the
    spawn_phase.toml table - do, blueprint, think, ship stamp their
    spellings. Review labels and seeds refuse before any worker launches.
    An unlabeled --node spawn is refused (x-007c), so rows stay labeled.
  - AC2: `session add --phase review` stamps (exit 2 before the enum gained
    review), and the roster renders the review slot between do and ship.
  - Closing: `session reap-open --phase review` fills ended_at and KEEPS the
    row; `--phase all` settles a dead session's do AND review windows in one
    call (the daemon observer's spelling).
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import os
from pathlib import Path

import pytest


from fno.paths_testing import use_tmpdir


NODE = "x-4ab1"
FULL_UUID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"


@pytest.fixture(autouse=True)
def _readable_node_row(monkeypatch):
    """A graph row for NODE, so the command seam's `--node` consult passes.

    The seam refuses a `--node` spawn whose row it cannot read; these tests
    drive the door for the session ROW, not the command contract, so every test
    sees one target-deriving row unless it stubs its own (the blueprint tests
    re-stub with a medium row so the derived command agrees with their seed)."""
    row = {"id": NODE, "slug": "sess", "dispatch_command": "/target", "difficulty": "low"}

    yield row


@pytest.fixture(autouse=True)
def _isolated_claims_root(tmp_path, monkeypatch):
    """Pin the global claims root at the tmp home.

    node:/dispatch: claims root at $FNO_CLAIMS_ROOT regardless of the tmp fno
    home, so without this the spawn guard's reservation lands in the machine's
    live claims store (and a run killed mid-flight leaves it held). Pinned
    here, both directions stay in tmp; the release sweep below is belt-only.
    """
    claims = tmp_path / "claims"
    claims.mkdir()
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(claims))
    yield
    from fno.claims.core import claim_status, release_claim

    for key in (f"node:{NODE}", f"dispatch:{NODE}"):
        try:
            holder = claim_status(key).get("holder")
            if holder:
                release_claim(key, holder=holder)
        except Exception:
            pass


def _seed_graph() -> None:
    """One scratch node in the tmp-home graph the whole suite resolves to."""
    from fno import paths

    g = paths.graph_json()
    g.parent.mkdir(parents=True, exist_ok=True)
    seed_graph(g, json.dumps({"entries": [{
            "id": NODE, "title": "scratch provenance target",
            "type": "feature", "project": "fno", "status": "ready",
        }]}))


def _node_rows() -> list[dict]:
    from fno import paths
    from fno.rust_binary import backlog_rows

    return next(e for e in backlog_rows(path=paths.graph_json()) if e["id"] == NODE).get(
        "sessions", []
    )




@pytest.fixture
def workdir_claude(tmp_path: Path, monkeypatch) -> Path:
    """Isolated fno home + graph with a scratch node + fake claude on PATH."""
    from tests.agents._fake_claude import install_fake_claude

    use_tmpdir(monkeypatch, tmp_path)
    _seed_graph()
    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    # Keep the native event-store writer reachable alongside the fake provider.
    monkeypatch.setenv("PATH", os.pathsep.join((str(bin_dir), os.environ.get("PATH", ""))))
    return tmp_path


# ---------------------------------------------------------------------------
# AC1: --node opens the row with the worker's own session id
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC2: the enum and the roster
# ---------------------------------------------------------------------------




# ---------------------------------------------------------------------------
# Closing: reap-open fills ended_at for review, removes only for do
# ---------------------------------------------------------------------------




# ---------------------------------------------------------------------------
# The spawner records the durable merge grant on the do row
# ---------------------------------------------------------------------------
