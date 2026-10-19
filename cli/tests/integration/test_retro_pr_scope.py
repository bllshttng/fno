"""Retro harvest hardening (ab-158ab951): --pr scope + project attribution.

Two reported failures of `fno retro run --pr N`:
  - the global pending-sentinel sweep ran ALONGSIDE the --pr harvest, pulling an
    unrelated repo's carve-outs into the same run (cross-PR spillover);
  - every filed node landed with project=None (retro's create path does not
    auto-derive project the way `fno backlog idea` does).
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest
import typer

from fno.carveout.core import CARVEOUTS_NAME as _CARVEOUTS_NAME
from fno.paths import project_log as _project_log
from fno.retro.cli import _resolve_pr_session_ids, process_sentinel_file
from fno.retro.routine import triage_pr


def _ledger(root: Path) -> Path:
    """The carve-out ledger where every product accessor resolves it."""
    return _project_log(_CARVEOUTS_NAME, project_root=root)


class _Rec:
    def __init__(self):
        self.created: list = []
        self._n = 0

    def create(self, **kw):
        self._n += 1
        nid = f"ab-new{self._n}"
        self.created.append({"id": nid, **kw})
        return nid

    def inbox_append(self, candidate):
        pass


def _write_sentinel(sd: Path, node_id: str, pr: int) -> Path:
    sd.mkdir(parents=True, exist_ok=True)
    p = sd / f"{node_id}.json"
    p.write_text(json.dumps({
        "node_id": node_id,
        "pr_number": pr,
        "pr_url": f"https://github.com/o/r/pull/{pr}",
    }), encoding="utf-8")
    return p


# ── #3: filed nodes carry the derived project ─────────────────────────


def test_cross_repo_sentinel_not_attributed_to_cwd(tmp_path, monkeypatch):
    """A sentinel for repo B processed from repo A must NOT inherit repo A's
    project; it lands project=None (codex P2)."""
    sentinel = _write_sentinel(tmp_path / "retro-pending", "ab-33333333", 50)  # repo o/r
    rec = _Rec()
    comments = [{"id": "c1", "body": "![high] real declined finding", "reviewer": "gemini[bot]"}]
    import fno.graph._intake as intake
    monkeypatch.setattr(intake, "detect_project_from_settings", lambda cwd=None: "repo-A-proj")
    process_sentinel_file(
        sentinel, repo_root=tmp_path, existing_nodes=[], comments=comments,
        create_fn=rec.create, inbox_fn=rec.inbox_append,
        current_repo_slug="someone/else",  # foreign repo -> do not attribute
    )
    assert len(rec.created) == 1
    assert rec.created[0].get("project") is None, rec.created[0]


# ── #2: --pr is strictly scoped to that PR ────────────────────────────


def test_pr_flag_scopes_by_repo(tmp_path, monkeypatch):
    """Same PR number in two repos: --pr N --repo o/r processes only o/r's (codex P2)."""
    import fno.carveout.core as cocore
    import fno.graph.store as store
    import fno.paths as paths
    import fno.retro.cli as rcli

    sd = tmp_path / "retro-pending"
    sd.mkdir(parents=True, exist_ok=True)
    (sd / "ab-local.json").write_text(json.dumps(
        {"node_id": "ab-local", "pr_number": 77, "pr_url": "https://github.com/o/r/pull/77"}),
        encoding="utf-8")
    (sd / "ab-foreign.json").write_text(json.dumps(
        {"node_id": "ab-foreign", "pr_number": 77, "pr_url": "https://github.com/x/y/pull/77"}),
        encoding="utf-8")
    (tmp_path / "graph.json").write_text("[]", encoding="utf-8")

    monkeypatch.setattr(paths, "resolve_repo_root", lambda: tmp_path)
    monkeypatch.setattr(paths, "retro_pending_dir", lambda: sd)
    monkeypatch.setattr(paths, "graph_json", lambda: tmp_path / "graph.json")
    monkeypatch.setattr(cocore, "resolve_carveout_root", lambda: tmp_path)
    monkeypatch.setattr(store, "read_graph_strict", lambda p: [])

    processed: list = []

    def fake_psf(sentinel_path, **kw):
        processed.append(Path(sentinel_path).name)
        return (object(), True)

    monkeypatch.setattr(rcli, "process_sentinel_file", fake_psf)
    monkeypatch.setattr(rcli, "_process_payload", lambda payload, **kw: (object(), True))

    try:
        rcli.run(node=None, pr=77, session=None, repo="o/r")
    except typer.Exit:
        pass

    assert processed == ["ab-local.json"], (
        f"--pr 77 --repo o/r must process only the o/r sentinel, got {processed}"
    )


# ── ab-b4da4664: filed nodes root at CANONICAL, not the worktree ──────


def test_filed_node_cwd_and_project_root_at_canonical(tmp_path, monkeypatch):
    """The filed node's cwd + project derive from the CANONICAL root (node_root),
    not the worktree (repo_root). A node outlives the worktree it was captured in,
    and detect_project_from_settings only matches canonical roots."""
    worktree = tmp_path / "wt"
    canonical = tmp_path / "canonical"
    worktree.mkdir()
    canonical.mkdir()
    sentinel = _write_sentinel(worktree / "retro-pending", "ab-44444444", 410)
    rec = _Rec()
    comments = [{"id": "c1", "body": "![high] real declined finding", "reviewer": "gemini[bot]"}]

    import fno.graph._intake as intake
    seen = {}

    def fake_detect(cwd=None):
        seen["cwd"] = cwd
        return "canon-proj"

    monkeypatch.setattr(intake, "detect_project_from_settings", fake_detect)

    report, removed = process_sentinel_file(
        sentinel,
        repo_root=worktree,    # worktree: used only for worktree-local artifacts
        node_root=canonical,   # canonical: the node's durable home + attribution
        existing_nodes=[],
        comments=comments,
        create_fn=rec.create,
        inbox_fn=rec.inbox_append,
        current_repo_slug="o/r",  # matches the sentinel's repo -> attribute
    )
    assert len(rec.created) == 1, rec.created
    assert rec.created[0].get("cwd") == str(canonical), rec.created[0]
    assert rec.created[0].get("project") == "canon-proj", rec.created[0]
    # project must be derived from the CANONICAL root, not the worktree.
    assert seen.get("cwd") == str(canonical), seen


# ── x-90b8: --pr-number with no owning session is carve-out READ-ONLY ──
#
# A hotfix / manual PR has no session<->PR ledger link. Without a guard, the
# synthetic `retro run --pr-number N` harvests EVERY unconsumed carve-out,
# stamps it `Source: PR #N`, files a node under the wrong lineage, and consumes
# it - exactly cv-0932fa60 minted onto PR #522. The fix mirrors the post-merge
# Step 4b guard: no owning session -> carve-outs are listed read-only, never
# filed or consumed. Reviews/COMPLETION (inherently PR-scoped) are untouched.

requires_rust = pytest.mark.dev_build


def _write_carveout(root: Path, **rec) -> None:
    led = _ledger(root)
    led.parent.mkdir(parents=True, exist_ok=True)
    base = {
        "id": "cv-stray",
        "description": "stray deferred work from another session",
        "session_id": "other-sess",
        "kind": "deferred",
    }
    base.update(rec)
    led.write_text(json.dumps(base) + "\n", encoding="utf-8")


@requires_rust
def test_resolve_pr_session_ids_matches_and_misses(tmp_path):
    led = tmp_path / "ledger.json"
    led.write_text(json.dumps({"entries": [
        {"session_id": "s1", "pr": 522, "pr_url": "https://github.com/o/r/pull/522"},
        {"session_id": "s2", "pr": 999, "pr_url": "https://github.com/o/r/pull/999"},
        {"sessions": ["s3", "s4"], "pr": 522, "pr_url": "https://github.com/o/r/pull/522"},
    ]}), encoding="utf-8")
    # matched entries: session_id + the sessions[] list, order-preserving + deduped
    assert _resolve_pr_session_ids(led, 522, "o/r") == ["s1", "s3", "s4"]
    # no entry for this PR -> [] (the read-only case)
    assert _resolve_pr_session_ids(led, 777, "o/r") == []
    # same number in a different repo -> [] (the ledger is global, slug-scoped)
    assert _resolve_pr_session_ids(led, 522, "x/y") == []
    # missing / unreadable ledger -> [] (degrades to read-only, never crashes)
    assert _resolve_pr_session_ids(tmp_path / "nope.json", 522, "o/r") == []


@requires_rust
def test_resolve_pr_session_ids_requires_repo_scope(tmp_path):
    """No resolvable repo (repo_slug=None) -> [] even when a global-ledger entry's
    url ends in /pull/<pr>. The ledger is global and PR numbers collide across
    repos, so an unscoped url match could consume a foreign PR's session (codex
    P2); the safe answer is read-only."""
    led = tmp_path / "ledger.json"
    led.write_text(json.dumps({"entries": [
        {"session_id": "s1", "pr": 522, "pr_url": "https://github.com/o/r/pull/522"},
        {"session_id": "s2", "pr": 522},  # bare number is also cross-repo-ambiguous
    ]}), encoding="utf-8")
    assert _resolve_pr_session_ids(led, 522, None) == []
    # ... but a known slug still resolves the url-carrying entry. s2 stays out:
    # a bare number attributes nothing even once the repo IS known.
    assert _resolve_pr_session_ids(led, 522, "o/r") == ["s1"]


def test_triage_carveouts_readonly_not_landed_or_consumed(tmp_path):
    """carveouts_readonly=True: the stray carve-out is surfaced read-only, NOT
    landed and NOT in harvested_carveout_ids (so the caller never consumes it).
    A real reviewer finding still lands."""
    _write_carveout(tmp_path)
    rec = _Rec()
    comments = [{"id": "c1", "body": "![high] real declined finding", "reviewer": "gemini[bot]"}]
    report = triage_pr(
        repo_root=tmp_path,
        pr_number=522,
        mode="autonomous",
        comments=comments,
        carveout_root=tmp_path,
        carveouts_readonly=True,
        create_fn=rec.create,
        inbox_fn=rec.inbox_append,
    )
    titles = [c.get("title") or "" for c in rec.created]
    assert any("real declined finding" in t for t in titles), rec.created
    assert not any("stray" in t.lower() for t in titles), rec.created
    assert report.harvested_carveout_ids == [], report.harvested_carveout_ids
    assert report.readonly_carveout_count == 1, report.readonly_carveout_count


def _run_synthetic_pr(tmp_path, monkeypatch, *, ledger_entries, local_slug="o/r"):
    """Drive run() for a synthetic --pr-number 522 with no --session-id, capturing
    the payload handed to _process_payload."""
    import fno.carveout.core as cocore
    import fno.graph.store as store
    import fno.paths as paths
    import fno.retro.cli as rcli

    (tmp_path / "graph.json").write_text("[]", encoding="utf-8")
    (tmp_path / "ledger.json").write_text(
        json.dumps({"entries": ledger_entries}), encoding="utf-8"
    )
    sd = tmp_path / "retro-pending"
    sd.mkdir(parents=True, exist_ok=True)

    monkeypatch.setattr(paths, "resolve_repo_root", lambda: tmp_path)
    monkeypatch.setattr(paths, "resolve_canonical_repo_root", lambda: tmp_path)
    monkeypatch.setattr(paths, "retro_pending_dir", lambda: sd)
    monkeypatch.setattr(paths, "graph_json", lambda: tmp_path / "graph.json")
    monkeypatch.setattr(paths, "ledger_json", lambda: tmp_path / "ledger.json")
    monkeypatch.setattr(cocore, "resolve_carveout_root", lambda: tmp_path)
    monkeypatch.setattr(store, "read_graph_strict", lambda p: [])
    monkeypatch.setattr(rcli, "_current_repo_slug", lambda *a, **k: local_slug)

    captured: dict = {}

    def fake_pp(payload, **kw):
        captured.update(payload)
        return (object(), True)

    monkeypatch.setattr(rcli, "_process_payload", fake_pp)
    try:
        rcli.run(node=None, pr=522, session=None, repo=None)
    except typer.Exit:
        pass
    return captured


def test_run_pr_no_owning_session_sets_readonly(tmp_path, monkeypatch):
    """No ledger entry for the PR -> payload marks carve-outs read-only and
    carries NO session scope (so triage_pr suppresses the carve-out source)."""
    captured = _run_synthetic_pr(tmp_path, monkeypatch, ledger_entries=[])
    assert captured.get("carveouts_readonly") is True, captured
    assert "session_ids" not in captured, captured


# ── x-23c0: the SENTINEL harvest path resolves owning session(s) too ──
#
# The synthetic --pr-number path (above) resolves + guards, but a
# reconcile-dropped sentinel (fno backlog reconcile, for merges outside the ship
# gate) arrives with NO session scope. Before the fix, process_sentinel_file ran
# harvest_carveouts with session_ids=None and DRAINED the shared ledger, stamping
# another in-flight session's carve-outs onto the merging PR (cv-5e4b9f4d,
# recorded in #123's session, swept by #121's resume reconcile -> node x-8d19
# mis-attributed). The fix gives the sentinel path the SAME resolve-or-readonly
# fallback, keyed off the ledger_path threaded in from run().


def _seed_two_session_carveouts(root: Path) -> None:
    """Two sessions drop deferred carve-outs into ONE shared ledger."""
    lines = [
        {"id": "cv-A", "description": "A deferred work", "session_id": "sess-A", "kind": "deferred"},
        {"id": "cv-B", "description": "B deferred work", "session_id": "sess-B", "kind": "deferred"},
    ]
    led = _ledger(root)
    led.parent.mkdir(parents=True, exist_ok=True)
    led.write_text(
        "".join(json.dumps(x) + "\n" for x in lines), encoding="utf-8"
    )


def test_sentinel_harvest_scopes_to_owning_session(tmp_path):
    """The original repro: sentinel for PR #501 (owned by sess-A) harvests ONLY
    A's carve-out and consumes it; B's stays in the shared ledger for its own
    PR's harvest. Before the fix the sentinel path drained both."""
    _seed_two_session_carveouts(tmp_path)
    led = tmp_path / "ledger.json"
    led.write_text(json.dumps({"entries": [
        {"session_id": "sess-A", "pr": 501, "pr_url": "https://github.com/o/r/pull/501"},
    ]}), encoding="utf-8")
    sentinel = _write_sentinel(tmp_path / "retro-pending", "ab-aaaa1111", 501)
    rec = _Rec()
    report, _removed = process_sentinel_file(
        sentinel, repo_root=tmp_path, existing_nodes=[], comments=[],
        carveout_root=tmp_path, ledger_path=led,
        create_fn=rec.create, inbox_fn=rec.inbox_append,
    )
    assert report.harvested_carveout_ids == ["cv-A"], report.harvested_carveout_ids
    assert report.readonly_carveout_count == 0
    # B's carve-out survives untouched in the ledger; A's was consumed.
    remaining = _ledger(tmp_path).read_text(encoding="utf-8")
    assert "cv-B" in remaining and "cv-A" not in remaining, remaining


def test_sentinel_harvest_no_ledger_is_readonly_not_drain(tmp_path):
    """Fail-SAFE: a caller that omits ledger_path (the internal-caller footgun)
    must route to read-only, NOT the old unscoped drain. Without a ledger the
    owner can't be resolved, so carve-outs are surfaced read-only and left in
    place - the drain precondition never reaches harvest_carveouts unscoped."""
    _seed_two_session_carveouts(tmp_path)
    sentinel = _write_sentinel(tmp_path / "retro-pending", "ab-dddd4444", 404)
    rec = _Rec()
    report, _removed = process_sentinel_file(
        sentinel, repo_root=tmp_path, existing_nodes=[], comments=[],
        carveout_root=tmp_path,  # NOTE: ledger_path deliberately omitted
        create_fn=rec.create, inbox_fn=rec.inbox_append,
    )
    assert report.harvested_carveout_ids == [], report.harvested_carveout_ids
    assert report.readonly_carveout_count == 2, report.readonly_carveout_count
    remaining = _ledger(tmp_path).read_text(encoding="utf-8")
    assert "cv-A" in remaining and "cv-B" in remaining, remaining
