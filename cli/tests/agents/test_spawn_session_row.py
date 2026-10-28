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

from tests._native_door import run_native

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

    def _load_graph(path, tokens):
        return {"entries": [dict(row)], "missing": []}

    monkeypatch.setattr("fno.graph.store.read_nodes_by_ids", _load_graph)
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
    from fno.graph.store import read_graph_strict

    return next(e for e in read_graph_strict(paths.graph_json()) if e["id"] == NODE).get(
        "sessions", []
    )


@pytest.fixture
def graph_cli_home(tmp_path: Path, monkeypatch) -> Path:
    """Graph-side CLI tests: tmp home, seeded node, and the CLI graph path
    pinned to that tmp graph.

    The pin matters: sibling suites setattr the lazy GRAPH_JSON module attr,
    whose monkeypatch teardown plants a concrete value in the module dict and
    silences __getattr__ for the rest of the process - without the pin, these
    tests read whatever graph the previous file pinned (changed-smoke runs
    its whole changed subset in one process).
    """
    import fno.graph.cli as graph_cli
    from fno import paths

    use_tmpdir(monkeypatch, tmp_path)
    _seed_graph()
    monkeypatch.setattr(graph_cli, "_graph_path", lambda: paths.graph_json())
    # The native door reads FNO_STATE_DIR first; name the same tmp root the
    # settings file serves python, so door-driving tests see the same graph.
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
    return tmp_path


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


def test_roster_renders_review_between_do_and_ship(graph_cli_home) -> None:
    """The lifecycle roster gains a review slot between do and ship."""
    for phase, harness, sid, start, end in [
        ("do", "codex", "c" * 32, "2026-08-23T09:00:00Z", "2026-08-23T10:00:00Z"),
        ("review", "claude", FULL_UUID, "2026-08-23T10:00:00Z", "2026-08-23T11:00:00Z"),
        ("ship", "codex", "d" * 32, "2026-08-23T11:00:00Z", "2026-08-23T12:00:00Z"),
    ]:
        code, out, err = run_native(
            "backlog", "session", "add", NODE, "--phase", phase,
            "--harness", harness, "--session-id", sid,
            "--started-at", start, "--ended-at", end,
        )
        assert code == 0, f"{out}{err}"

    import fno.graph.cli as graph_cli

    lines, summary = graph_cli._lifecycle_roster(_node_rows())
    text = "\n".join(lines)
    assert "review" in text and "claude" in text
    assert text.index("execute") < text.index("review") < text.index("ship")
    phases = [p["phase"] for p in summary["phases"] if isinstance(p, dict)] if isinstance(
        summary.get("phases"), list) else []
    assert "review" in phases


# ---------------------------------------------------------------------------
# Closing: reap-open fills ended_at for review, removes only for do
# ---------------------------------------------------------------------------


def test_reap_open_all_closes_every_open_row(graph_cli_home) -> None:
    """The death-cascade spelling: one session holding a do window AND a review
    window settles both - each row filled and kept."""
    for phase in ("execute", "review"):
        run_native(
            "backlog", "session", "add", NODE, "--phase", phase,
            "--harness", "claude", "--session-id", FULL_UUID,
            "--started-at", "2026-08-23T10:00:00Z",
        )

    code, out, err = run_native(
        "backlog", "session", "reap-open", NODE, "--harness", "claude",
        "--session-id", FULL_UUID, "--phase", "all", "--json",
    )
    assert code == 0, f"{out}{err}"
    receipt = json.loads(out)
    assert receipt["row_removed"] is False and receipt["row_closed"] is True

    rows = _node_rows()
    assert len(rows) == 2, f"both rows survive filled, got {rows!r}"
    assert all(row["ended_at"] for row in rows)


# ---------------------------------------------------------------------------
# The spawner records the durable merge grant on the do row
# ---------------------------------------------------------------------------
