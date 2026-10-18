"""Integration tests for `fno graph` subcommands via the typer CLI.

Each test verifies behavior matches the legacy roadmap-tasks.py script.
Uses typer.testing.CliRunner for speed; a seeded temporary store keeps live
state out of the test process.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.cli import app
from tests.fixtures.graph_seed import seed_graph
from tests.goldens._door import native_backlog

runner = CliRunner()

REPO_ROOT = Path(__file__).parent.parent.parent.parent


def _write_plan(dirpath: Path, name: str, title: str) -> Path:
    p = dirpath / name
    p.write_text(f"---\ntitle: {title}\n---\n# {title}\n")
    return p


def _recent_iso(days_ago: int = 1) -> str:
    """A created_at within the G1 stale-ready window (x-3236). Ordering fixtures
    that predate the guard used fixed 2026-01 dates that now read as abandoned;
    selection tests must anchor to 'now' so an incidental old date does not
    quarantine a node under test. Relative order is preserved via days_ago."""
    from datetime import datetime, timezone, timedelta

    return (datetime.now(timezone.utc) - timedelta(days=days_ago)).isoformat()


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    """A fresh graph store; its graph.json path is only the stable anchor."""
    g = tmp_path / "graph.json"
    # The native door resolves the anchor through the layout ladder, and a
    # fresh root would answer the db/ spelling. The empty legacy twin makes
    # the ladder answer this root spelling, so every side of the seam reads
    # one store.
    import sqlite3

    sqlite3.connect(tmp_path / "graph.db").close()
    # Patch the module-level constants so all operations hit this temp file
    import fno.graph._constants as gc
    import fno.graph.store as gs
    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "GRAPH_HTML", tmp_path / "graph.html")
    monkeypatch.setattr(gc, "GRAPH_ARCHIVE_JSON", tmp_path / "graph-archive.json")
    # Also patch the store module's imported names
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Seam readers (sidecar projection, guarded metadata/display reads)
    # resolve through paths.graph_json at call time; pin the resolver too.
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    # The native binary (which the get read-backs exec) resolves the store
    # through FNO_CONFIG's state_dir; point it at this same tmp dir so both
    # sides of the seam read one store.
    (tmp_path / "config.toml").write_text(f'state_dir = "{tmp_path}"\n')
    monkeypatch.setenv("FNO_CONFIG", str(tmp_path / "config.toml"))
    return g


def _native_get(*args) -> str:
    """Read one node back through the native binary (the graph-mode get
    ladder is the binary's now). Returns captured stdout; asserts exit 0."""
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    proc = _sp.run(
        [str(binary), "backlog", "get", *args],
        capture_output=True,
        text=True,
        env={**_os.environ, "FNO_TRACKER_BACKEND": "graph"},
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout


def _seed_graph_text(path: Path, payload: str, **_kwargs) -> None:
    seed_graph(path, payload)


def _invoke(*args, input=None):
    """Invoke the fno CLI and return the result."""
    return runner.invoke(app, list(args), input=input, catch_exceptions=False)


def _door_graph(g: Path, *args: str) -> _NativeResult:
    """The native door against the fixture's store (selection verbs answer
    natively now). Claims stay hermetic via FNO_CLAIMS_ROOT and the roster
    stub answers an empty listing, so the fleet read never leaves the sandbox."""
    from tests.goldens._door import door, roster_stub

    code, out, err = door(g.parent, list(args), path_prepend=roster_stub(g.parent, []))
    return _NativeResult(code, out, err)


class _NativeResult:
    """The CliRunner-shaped face of a native-door run."""

    def __init__(self, code: int, out: str, err: str):
        self.exit_code = code
        self.output = out
        self.stdout = out
        self.stderr = err


def _native_verb(verb: str, *args: str) -> _NativeResult:
    """The native backlog door: execs the binary against the same store the
    fixture wired through FNO_CONFIG. The difficulty default rides only the
    create verbs, where the retired add shim auto-appended it."""
    from tests._native_door import run_native

    argv = list(args)
    if verb in ("add", "idea") and "--difficulty" not in argv:
        argv.extend(["--difficulty", "medium"])
    code, out, err = run_native("backlog", verb, *argv)
    return _NativeResult(code, out, err)


def _native_verb_at(cwd, verb: str, *args: str) -> _NativeResult:
    """The same door with a pinned child cwd (config candidates start at
    cwd/.fno, so isolation from the runner's real config needs the pin)."""
    from tests._native_door import run_native

    argv = list(args)
    if verb in ("add", "idea") and "--difficulty" not in argv:
        argv.extend(["--difficulty", "medium"])
    code, out, err = run_native("backlog", verb, *argv, cwd=cwd)
    return _NativeResult(code, out, err)


def _native_session(*args: str) -> _NativeResult:
    """The Rust-owned session lifecycle door, migrated out of the Python app."""
    from tests._native_door import run_native

    code, out, err = run_native("backlog", "session", *args)
    return _NativeResult(code, out, err)


def _read_graph(g: Path) -> list[dict]:
    # The store owns state now; graph.json is a frozen export mirror, so a
    # post-command read-back must come from the store, not the file.
    from fno.graph.store import read_graph_strict

    return read_graph_strict(g)


def test_session_reap_open_returns_positive_settled_receipt(tmp_graph):
    """AC3: observer reap fills the exact open row and reads it back.

    Seeded in_progress with nothing else open: the settle rolls the node off
    in_progress (the x-9657 red), so status_after reads idea."""
    _seed_graph_text(tmp_graph, json.dumps({
        "entries": [{
            "id": "x-reap0001",
            "title": "Reap me",
            "status": "in_progress",
            "sessions": [{
                "phase": "execute",
                "harness": "codex",
                "session_id": "dead-session",
                "started_at": "2026-08-20T00:00:00Z",
            }],
        }]
    }) + "\n")

    result = _native_session(
        "reap-open", "x-reap0001",
        "--harness", "codex", "--session-id", "dead-session", "--json",
    )

    assert result.exit_code == 0, result.output
    receipt = json.loads(result.output)
    assert receipt["settled"] is True
    assert receipt["row_removed"] is False
    assert receipt["row_closed"] is True
    assert receipt["status_after"] == "idea"
    assert receipt["remaining_open_do"] == 0
    saved = _read_graph(tmp_graph)[0]
    assert saved["sessions"][0]["ended_at"], "the settled row is filled, never erased"
    assert saved["status"] == "idea"


def test_difficulty_prompt_value_proc_reasks_on_bad_band():
    """A bad band at the difficulty prompt raises click.UsageError (click's
    re-ask signal); the old bare ValueError surfaced as a traceback instead."""
    import click
    import pytest as _pytest

    import fno.graph.cli as gcli

    with _pytest.raises(click.UsageError):
        gcli._prompt_difficulty_value("hard")
    assert gcli._prompt_difficulty_value("high") == "high"
def test_legacy_id_resolves_under_configured_install(tmp_graph, monkeypatch):
    """AC3-HP/AC3-EDGE: a configured install still resolves a historical ab- id
    (mixed-format graph), because resolution honors both the configured and the
    legacy prefix."""
    # Seed a legacy 8-hex node directly.
    g = tmp_graph
    data = {"entries": _read_graph(g)}
    data["entries"].append({
        "id": "ab-55ba9adb", "title": "Legacy node", "priority": "p2",
        "status": "ready", "blocked_by": [], "type": "feature",
    })
    _seed_graph_text(g, json.dumps(data))

    from fno.config import SettingsModel
    model = SettingsModel(config={"backlog": {"id_prefix": "xy-", "id_hex_width": 4}})
    monkeypatch.setattr("fno.config.load_settings", lambda: model)

    r = _invoke("backlog", "get", "ab-55ba9adb")
    assert r.exit_code == 0, r.output


# --- ready ---


def test_ac1_hp_undispatched_external_backend_uses_tracker_join(tmp_graph, monkeypatch):
    import fno.graph.cli as graph_cli

    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_graph.parent / "claims"))
    seed_graph(tmp_graph, [])
    monkeypatch.setattr(
        graph_cli,
        "_joined_open_candidates",
        lambda: [{
            "id": "x-external-undispatched",
            "status": "ready",
            "plan_path": "/plans/external.md",
            "priority": "p0",
            "domain": "code",
            "blocked_by": [],
        }],
    )

    r = _invoke("backlog", "undispatched", "--all", "--json")

    assert r.exit_code == 0, r.output
    data = json.loads(r.output)
    assert any(row["id"] == "x-external-undispatched" for row in data["rows"])


def test_wheel_selection_verbs_tombstone_on_the_graph_backend(tmp_graph, monkeypatch):
    """The wheel keeps only the external-tracker bodies: on the graph backend
    both selection verbs refuse, naming the native door (exit 2)."""
    monkeypatch.delenv("FNO_TRACKER_BACKEND", raising=False)
    seed_graph(tmp_graph, [])

    r = _invoke("backlog", "next", "--all")
    assert r.exit_code == 2, r.output
    assert "the selection is served by the native door" in r.output

    r = _invoke("backlog", "undispatched", "--all", "--json")
    assert r.exit_code == 2, r.output
    assert "the observer is served by the native door" in r.output


def test_queue_accepts_multiple_ids_space_and_comma_separated(tmp_graph):
    """fno backlog queue ab-X,ab-Y ab-Z queues all three atomically."""
    ids = []
    for title in ("Multi-A", "Multi-B", "Multi-C"):
        r = _native_verb("add", title)
        # raw_decode tolerates trailing stderr (Multi-B/C resemble Multi-A, so the
        # filing-time dedup receipt mixes into r.output via CliRunner).
        ids.append(json.JSONDecoder().raw_decode(r.output)[0]["id"])
    # Mix comma and space separators.
    r = _native_verb("queue", f"{ids[0]},{ids[1]}", ids[2], "--reason", "batch")
    assert r.exit_code == 0, r.output + r.stderr
    queued_ids = {x["id"] for x in json.loads(_native_verb("queued").output)}
    assert queued_ids == set(ids)
    # Same reason on all three.
    for tid in ids:
        data = json.loads(_native_get(tid))
        assert data["queued_reason"] == "batch"


def test_queue_batch_is_atomic_on_unknown_id(tmp_graph):
    """If any ID is unknown, no nodes are queued."""
    r = _native_verb("add", "Real")
    real_id = json.loads(r.output)["id"]
    r = _native_verb("queue", f"{real_id},ab-deadbeef")
    assert r.exit_code != 0
    # Real node was NOT queued because the batch aborted.
    data = json.loads(_native_get(real_id))
    assert data.get("queued_at") is None


def test_done_audit_tags_operator_when_driving(tmp_graph, monkeypatch):
    """cv-9def52a7: `done` during a drive window emits backlog_done_operator_initiated."""
    from fno import drive_authority as da

    captured: dict = {}
    monkeypatch.setattr(da, "is_drive_authority_active", lambda *a, **k: True)
    monkeypatch.setattr(
        da,
        "emit_operator_initiated",
        lambda action_type, **kw: captured.update(type=action_type, kw=kw),
    )
    nid = json.loads(_native_verb("add", "DriveDone").output)["id"]
    _invoke("backlog", "done", nid, "--note", "drive fixture")
    assert captured.get("type") == "backlog_done_operator_initiated"
    assert captured["kw"]["task_id"] == nid
    assert captured["kw"]["source"] == "backlog"


class _SnapshotFakeTracker:
    """A NodeTracker fake carrying sentinels distinct from any graph value."""

    name = "fake-external"

    def __init__(self):
        from fno.tracker.types import TrackerCandidate, TrackerState

        self._TrackerCandidate = TrackerCandidate
        self._TrackerState = TrackerState

    def read(self, id):
        if id == "EXT-done":
            return self._TrackerCandidate(
                id=id, title="Closed blocker", state=self._TrackerState.closed
            )
        from fno.tracker.types import NodeNotFound

        raise NodeNotFound(id)

    def list_open(self):
        T, S = self._TrackerCandidate, self._TrackerState
        return [
            T(id="EXT-1", title="Free work", state=S.open, priority="p1",
              created_at="2026-01-02T00:00:00Z", blocked_by=["EXT-done"]),
            T(id="EXT-2", title="Waiting", state=S.open, priority="p2",
              created_at="2026-01-03T00:00:00Z"),
        ]

    def close(self, id):
        raise AssertionError("close is not part of the snapshot read path")


# --- rank (curated intra-lane ordering) ---

def _add(title: str, *, project: str, priority: str) -> str:
    r = _native_verb("add", title, "--project", project, "--priority", priority)
    assert r.exit_code == 0, r.output
    return json.loads(r.output)["id"]


def _rank_of(g: Path, node_id: str):
    for e in _read_graph(g):
        if e["id"] == node_id:
            return e.get("rank")
    raise AssertionError(f"{node_id} not in graph")


def _rank(*args: str):
    """The native rank door; the tmp_graph fixture's FNO_CONFIG pins the store.

    Every pin passes --operator, the fence's documented escape hatch, so the
    pins are deterministic in a runner descended from an agent session; the
    fence itself is covered by test_rank_operator_only.py.
    """
    import os
    import subprocess

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    env = {**os.environ, "FNO_TRACKER_BACKEND": "graph"}
    return subprocess.run(
        [str(binary), "backlog", "rank", *args],
        capture_output=True, text=True, env=env, timeout=120,
    )


def test_rank_partial_id_resolves_and_guards_self(tmp_graph):
    """A partial id fuzzy-resolves; the resolved id (not the raw partial) is
    used for self-exclusion and the self-anchor guard."""
    a = _add("PartialCard", project="fno", priority="p1")
    partial = a[:7]  # 'ab-' + 4 hex, unique with a single node
    # Partial resolves and ranks the full node.
    r = _rank(partial, "--top", "--operator")
    assert r.returncode == 0, r.stderr
    assert _rank_of(tmp_graph, a) is not None
    # Partial self-anchor is still caught (resolved id == resolved anchor id).
    r2 = _rank(partial, "--after", partial, "--operator")
    assert r2.returncode != 0
    assert "itself" in r2.stderr


# --- archive ---

def test_ac1_hp_graph_archive(tmp_graph):
    """AC1-HP: fno graph archive moves done nodes (#23).

    Replaces the prior exit-code-only assertion with a state round-trip.
    The archive lives in the same store as the working graph, so both
    halves read through the store rows: the node must be GONE from the
    live rows AND PRESENT among the archived residents. Checking only
    one half would miss "moved but not removed" or the reverse.

    The sweep is now dry-run by default with a 30-day age filter, so a
    freshly-completed node needs `--apply --older-than-days 0` to move.
    """
    from fno.graph.store import commit_rows_via_store, read_archive_entries

    r = _native_verb("add", "ToArchive")
    node_id = json.loads(r.output)["id"]
    # Seed completed_at through the store rather than a CLI verb: closing is
    # merge-gated now, and archive only cares that the node reads done.
    commit_rows_via_store(tmp_graph, lambda rows: [
        {**row, "completed_at": "2026-01-01T00:00:00+00:00",
         "artifact_url": "https://example.test/artifact"} if row["id"] == node_id else row
        for row in rows
    ])

    # Dry-run default: nothing moves.
    r = native_backlog("backlog", "archive")
    assert r.exit_code == 0
    assert "dry-run" in r.output
    assert read_archive_entries(tmp_graph) == []

    r = native_backlog("backlog", "archive", "--apply", "--older-than-days", "0")
    assert r.exit_code == 0

    archived_ids = {e["id"] for e in read_archive_entries(tmp_graph)}
    assert node_id in archived_ids, f"{node_id} should be in archive"

    live_ids = {e["id"] for e in _read_graph(tmp_graph)}
    assert node_id not in live_ids, f"{node_id} should be removed from live graph"


# --- priority vocabulary migration (p0/p1/p2/p3) ---


def test_new_p0_requires_breaking_acknowledgment(tmp_graph):
    """Every CLI birth path applies the p0 acknowledgment before mutation."""
    refused = _native_verb("new", "Not actually broken", "--priority", "p0")
    assert refused.exit_code != 0
    assert "p0 blocks everything else, usually a bug" in refused.output + refused.stderr
    assert _read_graph(tmp_graph) == []

    accepted = _native_verb(
        "new", "Broken service", "--priority", "p0", "--blocks-everything"
    )
    assert accepted.exit_code == 0, accepted.output + accepted.stderr
    assert _read_graph(tmp_graph)[0]["blocks_everything"] is True


def test_priority_migration_on_mutation(tmp_graph):
    """Legacy high/medium/low values are backfilled to p1/p2/p3 on the
    next graph mutation (recompute_statuses runs inside commit_rows_via_store).
    """
    _seed_graph_text(tmp_graph, json.dumps({
        "entries": [
            {"id": "ab-old00001", "title": "Was high", "priority": "high",
             "plan_path": "x.md", "status": "ready",
             "created_at": "2026-01-01T00:00:00Z"},
            {"id": "ab-old00002", "title": "Was medium", "priority": "medium",
             "plan_path": "x.md", "status": "ready",
             "created_at": "2026-01-01T00:00:00Z"},
            {"id": "ab-old00003", "title": "Was low", "priority": "low",
             "plan_path": "x.md", "status": "ready",
             "created_at": "2026-01-01T00:00:00Z"},
        ]
    }))

    # Trigger a mutation; commit_rows_via_store runs recompute_statuses
    # which contains the backfill loop.
    r = _native_verb("add", "Trigger mutation")
    assert r.exit_code == 0, r.output

    entries = _read_graph(tmp_graph)
    by_id = {e["id"]: e for e in entries}
    assert by_id["ab-old00001"]["priority"] == "p1"
    assert by_id["ab-old00002"]["priority"] == "p2"
    assert by_id["ab-old00003"]["priority"] == "p3"
    # No old vocabulary survives.
    assert all(
        e["priority"] in {"p0", "p1", "p2", "p3"}
        for e in entries
    )


# --- note evidence: a note is a fact on the node ─────────────────────────────


# The unpatched transport, captured at collection: the note-evidence stubs
# below delegate every non-gate verb to it.
from fno.rust_binary import verb_call as _real_verb_call  # noqa: E402


def _note_node():
    node_id = json.loads(_native_verb("add", "NoteTarget").output)["id"]
    return node_id


def test_note_citing_a_contradicted_line_refuses_before_append(tmp_graph, monkeypatch):
    """AC18-ERR: exit 1, nothing appended, nothing mailed."""
    node_id = _note_node()
    monkeypatch.setattr(
        "fno.rust_binary.verb_call",
        lambda verb, payload, **kwargs: (
            {
                "ok": False,
                "kind": "citation",
                "message": "cli/src/fno/law.py:99999: the file has 250 lines.",
            }
            if verb == "evidence-gate"
            else _real_verb_call(verb, payload, **kwargs)
        ),
    )

    r = _invoke(
        "backlog", "note", node_id,
        "cli/src/fno/law.py:99999 is the classifier",
    )

    assert r.exit_code == 1, r.output
    node = json.loads(_native_get(node_id))
    assert not node.get("progress_notes")


def test_note_with_an_unmeasured_claim_appends_and_warns(tmp_graph, monkeypatch):
    """AC19-HP: the note verb advises, never refuses a body."""
    node_id = _note_node()
    monkeypatch.setattr(
        "fno.rust_binary.verb_call",
        lambda verb, payload, **kwargs: (
            {"ok": True, "rows": None, "claims": ["167 lines"]}
            if verb == "evidence-gate"
            else _real_verb_call(verb, payload, **kwargs)
        ),
    )

    r = _invoke("backlog", "note", node_id, "the drain loop is 167 lines", "-q")

    assert r.exit_code == 0, r.output
    assert "unmeasured code fact" in r.stderr, r.stderr
    assert "--read" in r.stderr, r.stderr
    node = json.loads(_native_get(node_id))
    notes = node["progress_notes"]
    assert notes[-1]["text"] == "the drain loop is 167 lines"


def test_note_with_a_read_stores_rows_and_prints_no_warning(tmp_graph, monkeypatch):
    """AC20-HP: executed reads land beside the note they measure."""
    node_id = _note_node()
    monkeypatch.setattr(
        "fno.rust_binary.verb_call",
        lambda verb, payload, **kwargs: (
            {
                "ok": True,
                "rows": [
                    {"cmd": "echo measured", "exit": 0, "out_head": "measured",
                     "ts": "2026-09-10T00:00:00Z", "head_sha": ""}
                ],
                "claims": None,
            }
            if verb == "evidence-gate"
            else _real_verb_call(verb, payload, **kwargs)
        ),
    )

    r = _invoke(
        "backlog", "note", node_id, "advance.py is 200 lines",
        "--read", "echo measured", "--json", "-q",
    )

    assert r.exit_code == 0, r.output
    assert "unmeasured" not in r.stderr, r.stderr
    assert json.loads(r.stdout)["routed"] == "thread"
    node = json.loads(_native_get(node_id))
    reads = node["progress_notes"][-1]["reads"]
    assert reads[0]["cmd"] == "echo measured"
    assert reads[0]["exit"] == 0


def test_note_replace_refusal_suggested_command_runs(tmp_graph):
    """--replace is retired with a refusal that names its real command,
    and that command runs on this surface. Nothing is written."""
    node_id = _note_node()

    r = _invoke("backlog", "note", node_id, "clobber", "--replace")

    assert r.exit_code == 3, r.output
    assert "retired" in r.stderr, r.stderr
    assert f"fno backlog note comment {node_id} --list" in r.stderr, r.stderr

    feed = _invoke("backlog", "note", "comment", node_id, "--list")

    assert feed.exit_code == 0, feed.output
    node = json.loads(_native_get(node_id))
    assert not node.get("progress_notes"), "the retired flag wrote nothing"


def test_quiet_still_refuses_a_contradicted_citation(tmp_graph, monkeypatch):
    """AC21-EDGE: a silent annotation is still a fact on the node."""
    node_id = _note_node()
    monkeypatch.setattr(
        "fno.rust_binary.verb_call",
        lambda verb, payload, **kwargs: (
            {
                "ok": False,
                "kind": "citation",
                "message": "cli/src/fno/law.py:99999: the file has 250 lines.",
            }
            if verb == "evidence-gate"
            else _real_verb_call(verb, payload, **kwargs)
        ),
    )

    r = _invoke(
        "backlog", "note", node_id,
        "cli/src/fno/law.py:99999 is the classifier",
        "--quiet",
    )

    assert r.exit_code == 1, r.output
    node = json.loads(_native_get(node_id))
    assert not node.get("progress_notes")


def _by_id(tmp_graph):
    return {e["id"]: e for e in _read_graph(tmp_graph)}


def test_done_cascade_closes_all_done_parent_epic(tmp_graph):
    """x-33b2: closing the LAST open child of an epic auto-closes the epic (it is
    a container with no PR of its own; it is done when its children are). Replaces
    the old 'walker closes the epic via next' path."""
    entries = [
        # No plan_path: an epic carrying its own plan work is not auto-closed
        # any more, and this test pins the container auto-close, not that hold.
        {"id": "ab-epic0000", "title": "Epic", "status": "ready", "project": "p",
         "blocked_by": []},
        {"id": "ab-cdone001", "title": "Done child", "status": "done", "project": "p",
         "parent": "ab-epic0000", "completed_at": "2026-01-01T00:00:00Z", "blocked_by": []},
        # Last open child, no PR refs -> `done` closes it with no gh cross-check.
        {"id": "ab-clast002", "title": "Last child", "status": "ready", "project": "p",
         "parent": "ab-epic0000", "blocked_by": [],
         "artifact_url": "https://example.test/artifact"},
    ]
    _seed_graph_text(tmp_graph, json.dumps({"entries": entries}) + "\n")
    r = _native_verb("done", "ab-clast002")
    assert r.exit_code == 0, r.output + r.stderr
    nodes = _by_id(tmp_graph)
    assert nodes["ab-clast002"]["completed_at"]               # child closed
    assert nodes["ab-epic0000"]["completed_at"]                # epic auto-closed
    assert "auto-closed" in (nodes["ab-epic0000"].get("completion_note") or "")


# ---------------------------------------------------------------------------
# _resolved_cwd derivation in cmd_get
# ---------------------------------------------------------------------------

def _make_node_with_project_cwd(project: str, cwd: str) -> dict:
    return {
        "id": "ab-resolvetest",
        "title": "Resolve Test",
        "status": "ready",
        "project": project,
        "cwd": cwd,
    }


def test_resolved_cwd_uses_work_map_root_when_project_mapped(tmp_graph, monkeypatch, tmp_path):
    """AC1: node with project mapped in settings -> _resolved_cwd == work-map root."""
    import textwrap

    node = _make_node_with_project_cwd("myproject", "/recorded/other")
    _seed_graph_text(tmp_graph, json.dumps({"entries": [node]}) + "\n")

    # The native get resolves the work map through the settings candidates:
    # isolate the cwd and provide the map at the `<cwd>/.fno` candidate.
    fno_dir = tmp_path / ".fno"
    fno_dir.mkdir()
    (fno_dir / "settings.yaml").write_text(textwrap.dedent("""\
        work:
          workspaces:
            main:
              projects:
                - name: myproject
                  path: /mapped/root
    """))
    monkeypatch.chdir(tmp_path)

    data = json.loads(_native_get("ab-resolvetest"))
    assert data["_resolved_cwd"] == "/mapped/root", (
        f"Expected /mapped/root, got {data.get('_resolved_cwd')!r}"
    )


# ---------------------------------------------------------------------------
# Task 1.2: Filing-site cwd derivation from explicit --project via work-map
# ---------------------------------------------------------------------------

def _settings_yaml_for_project(settings_path: Path, project: str, root: str) -> None:
    """Write a minimal settings.yaml mapping project -> root."""
    import textwrap
    settings_path.write_text(textwrap.dedent(f"""\
        work:
          projects:
            {project}:
              path: {root}
    """))


def test_ac2_new_explicit_project_unscoped_derives_cwd(tmp_graph, tmp_path):
    """AC2: new --project <mapped> --unscoped -> cwd derived from work-map despite --unscoped."""
    work_root = str(tmp_path / "new-root")
    settings_path = tmp_graph.parent / "settings.yaml"
    _settings_yaml_for_project(settings_path, "fno", work_root)

    # cwd pins the child into the tmp dir so its config walk (cwd/.fno first)
    # cannot pick up a runner's real work-map ahead of the fixture's.
    r = _native_verb_at(
        tmp_path,
        "new", "New unscoped with explicit project",
        "--project", "fno",
        "--unscoped",
        "--force-domain",
    )

    assert r.exit_code == 0, r.output
    entries = _read_graph(tmp_graph)
    assert len(entries) == 1
    assert entries[0]["project"] == "fno"
    assert entries[0]["cwd"] == work_root


# --- US3: per-node dispatch verb + brief -----------------------------------


def test_update_dispatch_verb_null_clears(tmp_graph):
    r = _native_verb("add", "Verb node")
    nid = json.loads(r.output)["id"]
    _invoke("backlog", "update", nid, "--dispatch-verb", "/think")
    _invoke("backlog", "update", nid, "--dispatch-verb", "null")
    assert _read_graph(tmp_graph)[0]["dispatch_verb"] is None


# ---------------------------------------------------------------------------
# Guarded selection + starvation receipts + triage pile
# ---------------------------------------------------------------------------


def _seed(g: Path, entries: list[dict]) -> None:
    _seed_graph_text(g, json.dumps({"entries": entries}))


def test_next_claims_with_lockfile_without_writing_graph_owner(tmp_graph, monkeypatch):
    from fno.claims.core import claim_status, release_claim

    recent = _recent_iso(1)
    plan = _write_plan(tmp_graph.parent, "claimed-next.md", "Claimed next")
    node_id = "ab-next0001"
    _seed_graph_text(tmp_graph, json.dumps({"entries": [{
        "id": node_id, "title": "Claimed next", "project": "fno",
        "plan_path": str(plan), "created_at": recent, "priority": "p2",
    }]}) + "\n")
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_graph.parent / "claims"))
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "next-session")

    result = _door_graph(tmp_graph, "next", "--project", "fno", "--claim", "next-session")

    assert result.exit_code == 0, result.output
    assert f'"id": "{node_id}"' in result.stdout
    # The acquirer is the door process, already exited: the lockfile holds
    # with our holder and reads suspect (a dead pid never reads live).
    verdict = claim_status(f"node:{node_id}")
    assert verdict["holder"] == "next-session"
    assert verdict["state"] == "suspect"
    # The served owner is the read-time claim projection: it follows the
    # lockfile and nothing persists it. Release the claim; the owner is gone.
    from fno.graph.store import read_graph

    served = read_graph(tmp_graph)[0]
    assert served.get("locked_by") == "next-session"
    release_claim(key=f"node:{node_id}", holder="next-session")
    served_after = read_graph(tmp_graph)[0]
    assert served_after.get("locked_by") is None


def test_maintain_apply_defers_stale_ready(tmp_graph):
    _seed(tmp_graph, [
        {
            "id": "ab-old", "title": "old", "project": "fno",
            "plan_path": "/nonexistent/plan.md", "priority": "p2",
            "created_at": "2026-01-01T00:00:00+00:00",
        },
        {
            "id": "ab-folded", "title": "folded", "project": "fno",
            "contained_in": "ab-old", "created_at": _recent_iso(1),
        },
    ])
    r = _door_graph(tmp_graph, "maintain", "--apply", "--json", "--no-validity")
    data = json.loads(r.stdout)
    applied = [x["node_id"] for x in data["stale_ready"]["applied"]]
    assert "ab-old" in applied
    # Reversible defer landed with the quarantine reason.
    entries = _read_graph(tmp_graph)
    node = next(e for e in entries if e["id"] == "ab-old")
    assert node["deferred_reason"] == "stale-quarantine (guard)"
    child = next(e for e in entries if e["id"] == "ab-folded")
    assert child["contained_in"] == "ab-old"


def test_maintain_apply_skips_in_review_node(tmp_graph):
    # An old ready node that already carries a PR is in-review (movement); the
    # stale-ready leg must never defer it into the pile.
    _seed(tmp_graph, [{
        "id": "ab-inrev", "title": "in review", "project": "fno",
        "plan_path": "/nonexistent/plan.md", "priority": "p2",
        "created_at": "2026-01-01T00:00:00+00:00", "pr_number": 42,
    }])
    r = _door_graph(tmp_graph, "maintain", "--apply", "--json", "--no-validity")
    data = json.loads(r.stdout)
    applied = [x["node_id"] for x in data["stale_ready"]["applied"]]
    assert "ab-inrev" not in applied
    entries = _read_graph(tmp_graph)
    node = next(e for e in entries if e["id"] == "ab-inrev")
    assert not node.get("deferred_at")  # never quarantined


class _GetFakeTracker(_SnapshotFakeTracker):
    """Extends the snapshot fake: read() answers the open sentinels too."""

    def read(self, id):
        if id == "EXT-1":
            T, S = self._TrackerCandidate, self._TrackerState
            return T(id=id, title="Free work", state=S.open, blocked_by=["EXT-done"])
        if id == "EXT-2":
            T, S = self._TrackerCandidate, self._TrackerState
            return T(id=id, title="Waiting", state=S.open)
        return super().read(id)


def test_get_external_reads_tracker_and_sidecar_sentinels(
    tmp_graph, tmp_path, monkeypatch
):
    """AC2-HP (display path): `backlog get` under an external backend resolves
    the OPAQUE id exactly (no prefix-hex grammar), renders the five tracker
    fields plus sidecar sentinels, derives status at read time, and never
    returns the contradictory graph values."""
    _seed_graph_text(tmp_graph,
        json.dumps({"entries": [{
            "id": "EXT-1", "title": "graph-title-sentinel",
            "cwd": "/graph-cwd-sentinel", "pr_number": 999,
        }]}),
        encoding="utf-8",
    )
    sidecars = tmp_path / "sidecars"
    sidecars.mkdir()
    (sidecars / "EXT-1.json").write_text(
        json.dumps({"id": "EXT-1", "cwd": "/external-cwd", "pr_number": 7}),
        encoding="utf-8",
    )
    monkeypatch.setattr("fno.tracker.get_tracker", lambda *a, **k: _GetFakeTracker())
    monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_graph)
    import fno.tracker.sidecar as sidecar_store

    monkeypatch.setattr(sidecar_store, "sidecar_path",
                        lambda i: sidecars / f"{i}.json")
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")

    r = _invoke("backlog", "get", "EXT-1")
    assert r.exit_code == 0, r.output
    doc = json.loads(r.output)
    assert doc["title"] == "Free work"  # tracker sentinel, not graph-title-sentinel
    assert doc["cwd"] == "/external-cwd"
    assert doc["pr_number"] == 7
    assert doc["state"] == "open"
    assert doc["status"] == "in_review"  # read-time derivation from pr evidence
    assert doc["_resolved_cwd"] == "/external-cwd"
    # Field mode reads through the same halves.
    r = _invoke("backlog", "get", "EXT-1", "--field", "cwd")
    assert r.exit_code == 0 and r.output.strip() == "/external-cwd"
    # A local-grammar spelling never resolves externally (dash-free so typer
    # does not read it as a flag).
    r = _invoke("backlog", "get", "1")
    assert r.exit_code == 1
    # EXT-2: open, no PR, no plan (no sidecar file written for it) - the same
    # three-way split selection filters on, not "any open node is ready".
    r = _invoke("backlog", "get", "EXT-2")
    assert r.exit_code == 0, r.output
    assert json.loads(r.output)["status"] == "idea"


def test_provenance_external_reads_sidecar_edges(
    tmp_graph, tmp_path, monkeypatch
):
    """AC2-HP (provenance path): `backlog provenance` under an external backend
    reads birth/spawn edges from the sidecar, joins the origin title from the
    tracker, and never reports the graph file's rows."""
    _seed_graph_text(tmp_graph,
        json.dumps({"entries": [{
            "id": "EXT-1", "source_session_id": "graph-sess",
            "sessions": [{"phase": "graph-only"}],
        }]}),
        encoding="utf-8",
    )
    sidecars = tmp_path / "sidecars"
    sidecars.mkdir()
    (sidecars / "EXT-1.json").write_text(
        json.dumps({"id": "EXT-1", "source_session_id": "ext-sess",
                    "source_harness": "claude",
                    "sessions": [{"phase": "do", "session_id": "ext-sess"}],
                    "source_node_id": "EXT-done"}),
        encoding="utf-8",
    )
    monkeypatch.setattr("fno.tracker.get_tracker", lambda *a, **k: _GetFakeTracker())
    monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_graph)
    import fno.tracker.sidecar as sidecar_store

    monkeypatch.setattr(sidecar_store, "sidecar_path",
                        lambda i: sidecars / f"{i}.json")
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")

    r = _invoke("backlog", "provenance", "EXT-1", "--json")
    assert r.exit_code == 0, r.output
    doc = json.loads(r.output)
    assert doc["node_id"] == "EXT-1"
    assert doc["title"] == "Free work"
    assert doc["sessions"] == [{"phase": "do", "session_id": "ext-sess"}]
    assert doc["source_node_id"] == "EXT-done"
    assert doc["source_node_title"] == "Closed blocker"
