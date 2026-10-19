"""Integration tests for `fno done` race collision and force-overwrite.

Covers:
- #28: done_race_collision event emitted when second fno done call sees status already done.
       User-supplied --link/--note/--pr still applied. status/completed_at NOT overwritten.
- #30: --force-overwrite flag causes _apply_rollup to overwrite even non-null rollup fields.
       Default (fill-if-null) behavior is the control case.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


# -- shared fixtures/helpers --


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    """Fresh graph.json + ledger.json routed via monkeypatch."""
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    ledger = tmp_path / "ledger.json"
    ledger.write_text('{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs
    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "LEDGER_JSON", ledger)
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Seam readers resolve fno.paths.graph_json at call time; pin the
    # resolver to the same hermetic file (module-attr pins do not reach it).
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    monkeypatch.delenv("CLAUDECODE_SESSION_ID", raising=False)
    return g


@pytest.fixture
def tmp_ledger(tmp_path) -> Path:
    return tmp_path / "ledger.json"


@pytest.fixture
def tmp_events(tmp_path, monkeypatch) -> Path:
    """Route append_event to a temp events.jsonl."""
    events_file = tmp_path / "events.jsonl"
    import fno.events as ev_mod
    monkeypatch.setattr(ev_mod, "append_event", _make_append_event_to(events_file))
    return events_file


def _make_append_event_to(events_file: Path):
    """Return a patched append_event that writes to a specific events_file."""
    import fno.events as ev_mod

    def _append(event, path=None, *, lock_timeout_seconds=30):
        # Always write to our temp file regardless of the path argument.
        ev_mod.validate(event)
        events_file.parent.mkdir(parents=True, exist_ok=True)
        import json
        with events_file.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(event, separators=(",", ":")) + "\n")

    return _append


def _seed(g: Path, entries: list[dict]) -> None:
    seed_graph(g, json.dumps({"entries": entries}, indent=2) + "\n")


def _read(g: Path) -> list[dict]:
    # The store owns state; graph.json is a frozen export, so read-backs
    # come from store rows.
    from fno.graph.store import read_graph_strict

    return read_graph_strict(g)


def _seed_ledger(ledger: Path, entries: list[dict]) -> None:
    ledger.write_text(json.dumps({"entries": entries}, indent=2) + "\n")


def _stub_subprocess_no_git(monkeypatch):
    """Stub out git and gh so they return nothing (no branch/PR detection)."""
    from fno.done import cli as done_cli

    class _Res:
        def __init__(self, stdout="", rc=1):
            self.stdout = stdout
            self.returncode = rc
            self.stderr = ""

    real_run = done_cli.subprocess.run

    def fake_run(*a, **kw):
        cmd = a[0] if a else kw.get("cmd")
        if cmd and {"doctor", "event"} <= {str(p) for p in cmd}:
            # Event emission rides the same seam; the real binary answers.
            return real_run(*a, **kw)
        return _Res()

    monkeypatch.setattr(done_cli.subprocess, "run", fake_run)

    # `--pr` now demands gh-resolved merge evidence; without a stub
    # these collision tests would exit 4 on the gh outage instead of reaching
    # the collision path they are about.
    from fno.graph._reconcile import PrMergeState

    monkeypatch.setattr(
        done_cli,
        "_gh_query",
        lambda n, **kw: PrMergeState(
            number=n, state="MERGED", url=None, merged_at="2026-01-01T00:00:00Z"
        ),
    )


# ---- #28: done_race_collision ----


def test_done_race_collision_emits_event_and_preserves_metadata(
    tmp_graph, tmp_ledger, tmp_events, monkeypatch
):
    """#28: second fno done on an already-done node:
    - emits done_race_collision event
    - applies user-supplied --link
    - does NOT overwrite status or completed_at
    """
    first_completed_at = "2026-05-15T10:00:00+00:00"
    _seed(tmp_graph, [{
        "id": "ab-race001",
        "title": "Race test node",
        "status": "done",
        "completed_at": first_completed_at,
        "domain": "research",
        "artifact_url": None,
    }])
    _stub_subprocess_no_git(monkeypatch)

    result = runner.invoke(app, ["done", "ab-race001", "--link", "https://example.com/artifact"])
    # Should succeed (exit 0) with a diagnostic to stderr
    assert result.exit_code == 0, f"Exit {result.exit_code}: {result.output}"

    # status and completed_at must NOT change
    entry = next(e for e in _read(tmp_graph) if e["id"] == "ab-race001")
    assert entry["status"] == "done"
    assert entry["completed_at"] == first_completed_at

    # --link should still be applied
    assert entry.get("artifact_url") == "https://example.com/artifact"

    # done_race_collision event must be in events.jsonl
    assert tmp_events.exists(), "events.jsonl was not created"
    events = [json.loads(line) for line in tmp_events.read_text().splitlines() if line.strip()]
    collision_events = [e for e in events if e.get("type") == "done_race_collision"]
    assert collision_events, f"No done_race_collision event found. Events: {events}"

    ev = collision_events[0]
    data = ev.get("data", {})
    assert data.get("node_id") == "ab-race001"
    assert data.get("first_completed_at") == first_completed_at
    assert data.get("second_attempt_at")  # non-empty timestamp


# ---- x-43e4: the done path derives the same cwd guess update does ----


def test_done_named_repo_overrides_a_disagreeing_recorded_url(
    tmp_graph, tmp_ledger, tmp_events, monkeypatch
):
    """A named --repo is an assertion: it stamps the url for THAT repo even
    when the node's recorded pr_url names a different one (the repair flow
    that previously required commit_rows_via_store)."""
    _seed(tmp_graph, [{
        "id": "ab-repo0001",
        "title": "cross-repo repair",
        "status": "done",
        "completed_at": "2026-05-15T10:00:00+00:00",
        "domain": "code",
        "pr_number": 5,
        "pr_url": "https://github.com/real/repo/pull/5",
    }])
    _stub_subprocess_no_git(monkeypatch)

    result = runner.invoke(app, ["done", "ab-repo0001", "--pr", "42", "--repo", "other/repo"])

    assert result.exit_code == 0, result.output
    assert "stamped --repo other/repo" in result.output
    entry = next(e for e in _read(tmp_graph) if e["id"] == "ab-repo0001")
    assert entry["pr_url"] == "https://github.com/other/repo/pull/42"
    assert entry["pr_number"] == 42


def test_done_derived_url_refused_when_it_contradicts_the_recorded_repo(
    tmp_graph, tmp_ledger, tmp_events, monkeypatch
):
    """The cwd derivation is a guess; when it names a repo the node's recorded
    pr_url disagrees with, done must refuse naming both repos and the --repo
    remedy, leaving the recorded link untouched (x-43e4, x-5764 shape)."""
    from fno.done import cli as done_cli

    _seed(tmp_graph, [{
        "id": "ab-repo0002",
        "title": "cross-repo trap",
        "status": "done",
        "completed_at": "2026-05-15T10:00:00+00:00",
        "domain": "code",
        "pr_number": 5,
        "pr_url": "https://github.com/real/repo/pull/5",
    }])
    _stub_subprocess_no_git(monkeypatch)
    monkeypatch.setattr(
        done_cli,
        "pr_url_for_repo",
        lambda pr, cwd=None: f"https://github.com/wrong/repo/pull/{pr}",
    )

    result = runner.invoke(app, ["done", "ab-repo0002", "--pr", "911"])

    assert result.exit_code != 0
    assert "wrong/repo" in result.output and "real/repo" in result.output
    assert "--repo" in result.output
    entry = next(e for e in _read(tmp_graph) if e["id"] == "ab-repo0002")
    assert entry["pr_url"] == "https://github.com/real/repo/pull/5"
    assert entry["pr_number"] == 5


# ---- #30: --force-overwrite ----


def test_done_force_overwrite_backfill_preserves_historical_session(
    tmp_graph, tmp_ledger, monkeypatch
):
    """#30 + Codex P1 regression: --backfill --force-overwrite from an active
    session must trust the ledger, NOT CLAUDECODE_SESSION_ID.

    Without this guard, a backfill sweep from a live session would mass-rewrite
    every reconciled node's session_id to the CURRENT session id - the exact
    opposite of "reconcile historical attribution from the ledger." Codex P1
    on PR #279.
    """
    plan_path = "/some/plan.md"
    ledger_session = "ledger-historical-aabbccdd"
    current_env_session = "current-env-99887766"

    _seed(tmp_graph, [{
        "id": "ab-codex-p1-001",
        "title": "Codex P1 regression",
        "status": "done",
        "completed_at": "2026-05-15T10:00:00+00:00",
        "domain": "research",
        "plan_path": plan_path,
        "session_id": "stale-existing-11111111",
    }])
    _seed_ledger(tmp_ledger, [{
        "plan_path": plan_path,
        "sessions": [ledger_session],
        "cost_usd": 0.05,
        "completed": "2026-05-15T12:00:00Z",
    }])
    monkeypatch.setenv("CLAUDECODE_SESSION_ID", current_env_session)
    _stub_subprocess_no_git(monkeypatch)

    result = runner.invoke(app, ["done", "ab-codex-p1-001", "--backfill", "--force-overwrite"])
    assert result.exit_code == 0, f"Exit {result.exit_code}: {result.output}"

    entry = next(e for e in _read(tmp_graph) if e["id"] == "ab-codex-p1-001")
    # With --force-overwrite, ledger session must win over env session.
    assert entry.get("session_id") == ledger_session, (
        f"Expected ledger session {ledger_session!r} to win over env "
        f"{current_env_session!r}; got {entry.get('session_id')!r}. "
        "If this fails, --backfill --force-overwrite mass-rewrites history."
    )


def test_done_force_overwrite_cost_dedup(tmp_graph, tmp_ledger, monkeypatch):
    """#30 AC-EDGE: --force-overwrite --backfill must NOT double-count cost.

    Dedup key is (session_id, timestamp). When the ledger row matches an existing
    cost_sessions entry on both fields, the merge loop must skip it regardless
    of force_overwrite. The total cost_usd must stay at the existing value.
    """
    session = "shared-session-77889900"
    plan_path = "/some/plan.md"
    timestamp = "2026-05-15T12:00:00Z"

    _seed(tmp_graph, [{
        "id": "ab-dedup001",
        "title": "Cost-dedup node",
        "status": "done",
        "completed_at": "2026-05-15T10:00:00+00:00",
        "domain": "research",
        "plan_path": plan_path,
        "session_id": session,
        "cost_usd": 0.10,
        "cost_sessions": [{
            "session_id": session,
            "timestamp": timestamp,
            "cost_usd": 0.10,
        }],
    }])

    # Ledger row that, after rollup, produces the same (session_id, timestamp)
    # pair already present in cost_sessions. The dedup must skip the duplicate.
    _seed_ledger(tmp_ledger, [{
        "plan_path": plan_path,
        "sessions": [session],
        "cost_usd": 0.10,
        "completed": timestamp,
    }])
    _stub_subprocess_no_git(monkeypatch)

    result = runner.invoke(app, ["done", "ab-dedup001", "--backfill", "--force-overwrite"])
    assert result.exit_code == 0, f"Exit {result.exit_code}: {result.output}"

    entry = next(e for e in _read(tmp_graph) if e["id"] == "ab-dedup001")
    assert len(entry["cost_sessions"]) == 1, (
        f"Expected dedup to keep one row, got {entry['cost_sessions']!r}"
    )
    assert entry["cost_usd"] == 0.10, (
        f"Expected cost_usd preserved at 0.10, got {entry['cost_usd']!r}"
    )


def test_done_force_overwrite_empty_ledger_noop(tmp_graph, tmp_ledger, monkeypatch):
    """#30 AC-ERR: --force-overwrite with empty rollup is a no-op.

    Even with --force-overwrite, missing ledger data must not null out the
    existing session_id or points (the `if new_sid` and `if rollup.get('points')
    is not None` guards keep null overwrites from happening).
    """
    existing_session = "existing-session-99887766"
    plan_path = "/some/missing/plan.md"

    _seed(tmp_graph, [{
        "id": "ab-empty001",
        "title": "Empty ledger node",
        "status": "done",
        "completed_at": "2026-05-15T10:00:00+00:00",
        "domain": "research",
        "plan_path": plan_path,
        "session_id": existing_session,
        "points": 5,
    }])

    # Empty ledger - no matching entries for plan_path
    _seed_ledger(tmp_ledger, [])
    _stub_subprocess_no_git(monkeypatch)

    result = runner.invoke(app, ["done", "ab-empty001", "--backfill", "--force-overwrite"])
    assert result.exit_code == 0, f"Exit {result.exit_code}: {result.output}"

    entry = next(e for e in _read(tmp_graph) if e["id"] == "ab-empty001")
    assert entry.get("session_id") == existing_session, (
        f"Expected existing session_id preserved on empty rollup, got {entry.get('session_id')!r}"
    )
    assert entry.get("points") == 5, (
        f"Expected existing points preserved on empty rollup, got {entry.get('points')!r}"
    )
