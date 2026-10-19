"""Integration tests for the retro-sentinel consumer (Wave 4.2, US6)."""
from __future__ import annotations

import json
from pathlib import Path

from fno.retro.cli import process_sentinel_file


def _write_sentinel(tmp_path: Path, node_id="ab-12345678", pr=343, mode=None) -> Path:
    payload = {
        "node_id": node_id,
        "pr_number": pr,
        "pr_url": f"https://github.com/o/r/pull/{pr}",
        "merged_at": "2026-05-24T00:00:00Z",
        "plan_path": None,
        "closed_by": "backlog-reconcile",
        "closed_at": "2026-05-24T00:01:00Z",
    }
    if mode:
        payload["mode"] = mode
    sd = tmp_path / "retro-pending"
    sd.mkdir(parents=True, exist_ok=True)
    p = sd / f"{node_id}.json"
    p.write_text(json.dumps(payload), encoding="utf-8")
    return p


class _Rec:
    def __init__(self):
        self.created, self.queued, self.inbox = [], [], []
        self._n = 0

    def create(self, **kw):
        self._n += 1
        nid = f"ab-new{self._n}"
        self.created.append({"id": nid, **kw})
        return nid

    def inbox_append(self, candidate):
        self.inbox.append(candidate)


def test_ac6_edge_sentinel_consumed_and_removed(tmp_path: Path):
    """AC6-EDGE: a reconcile retro sentinel is consumed; a node is filed; sentinel removed."""
    sentinel = _write_sentinel(tmp_path)
    rec = _Rec()
    comments = [{"id": "c1", "body": "![high] real declined finding", "reviewer": "gemini[bot]"}]
    report, removed = process_sentinel_file(
        sentinel,
        repo_root=tmp_path,
        existing_nodes=[],
        comments=comments,
        create_fn=rec.create,
        inbox_fn=rec.inbox_append,
    )
    assert len(rec.created) == 1  # node filed from the declined finding
    # default mode (no mode in sentinel) is interactive -> queued
    assert rec.created[0].get("queued") is True
    assert removed is True
    assert not sentinel.exists()


def test_land_failure_retains_sentinel(tmp_path: Path):
    """A land failure RETAINS the sentinel (crash mid-run re-triages)."""
    sentinel = _write_sentinel(tmp_path)

    def boom(**kw):
        raise TimeoutError("lock timeout")

    comments = [{"id": "c1", "body": "![high] x", "reviewer": "gemini[bot]"}]
    report, removed = process_sentinel_file(
        sentinel,
        repo_root=tmp_path,
        existing_nodes=[],
        comments=comments,
        create_fn=boom,
        inbox_fn=lambda s: None,
    )
    assert report.failed is True
    assert removed is False
    assert sentinel.exists()


def test_dedup_against_existing_nodes(tmp_path: Path):
    """AC6-FR/AC5-HP: a finding already filed (trailer on a live node) is not re-created."""
    from fno.retro.dedup import content_hash, trailer

    sentinel = _write_sentinel(tmp_path)
    rec = _Rec()
    body = "![high] already filed finding"
    # The live node carries the trailer for this finding's hash on PR 343.
    h = content_hash(body)
    existing = [{"id": "ab-old", "details": f"x\n\n{trailer(343, h)}"}]
    comments = [{"id": "c1", "body": body, "reviewer": "gemini[bot]"}]
    report, removed = process_sentinel_file(
        sentinel,
        repo_root=tmp_path,
        existing_nodes=existing,
        comments=comments,
        create_fn=rec.create,
        inbox_fn=rec.inbox_append,
    )
    assert rec.created == []          # deduped: nothing new filed
    assert report.skipped_dupes == 1
    assert removed is True            # clean run, sentinel consumed


def test_autonomous_mode_creates_active(tmp_path: Path):
    """A sentinel carrying mode=autonomous lands active (no queue)."""
    sentinel = _write_sentinel(tmp_path, mode="autonomous")
    rec = _Rec()
    comments = [{"id": "c1", "body": "![high] x", "reviewer": "gemini[bot]"}]
    report, removed = process_sentinel_file(
        sentinel,
        repo_root=tmp_path,
        existing_nodes=[],
        comments=comments,
        create_fn=rec.create,
        inbox_fn=rec.inbox_append,
    )
    assert len(rec.created) == 1
    assert rec.created[0].get("queued") is False  # autonomous -> active
    assert removed is True


# --- derive resolved/skipped from real PR data ------------------------------


def _dispatch_gh(*, resolved_ids, inline, issue_comments="[[]]"):
    """A gh runner that answers graphql / inline-comments / issue-comments calls.

    `resolved_ids` -> databaseIds returned in a single RESOLVED reviewThread.
    `inline` -> the REST pulls/N/comments payload (list of comment dicts).
    """
    threads = [
        {
            "isResolved": True,
            "path": "src/x.py",
            "comments": {"nodes": [{"databaseId": int(i), "body": "b"} for i in resolved_ids]},
        }
    ]
    graphql = json.dumps(
        {"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": False, "endCursor": None}, "nodes": threads}}}}}
    )

    def gh(args):
        if args[:2] == ["api", "graphql"]:
            return 0, graphql, ""
        if "issues" in (args[1] if len(args) > 1 else ""):
            return 0, issue_comments, ""
        if "pulls" in (args[1] if len(args) > 1 else ""):
            return 0, json.dumps(inline), ""
        return 0, "[]", ""

    return gh


def test_resolved_findings_not_refiled(tmp_path: Path):
    """ab-bb7fa74f: an IMPLEMENTED (resolved-thread) finding is NOT filed; a
    declined one IS. Proves resolved_ids is derived from real PR data."""
    sentinel = _write_sentinel(tmp_path, mode="autonomous")
    rec = _Rec()
    inline = [
        {"id": 111, "body": "![high] implemented later", "html_url": "u1",
         "user": {"login": "gemini[bot]"}},
        {"id": 222, "body": "![high] genuinely declined", "html_url": "u2",
         "user": {"login": "gemini[bot]"}},
    ]
    gh = _dispatch_gh(resolved_ids=["111"], inline=inline)
    report, removed = process_sentinel_file(
        sentinel,
        repo_root=tmp_path,
        existing_nodes=[],
        comments=None,            # force live derivation
        gh_runner=gh,
        create_fn=rec.create,
        inbox_fn=rec.inbox_append,
    )
    assert report.partial is False
    assert len(rec.created) == 1                       # only the declined one
    assert "declined" in rec.created[0]["details"].lower() or \
           "declined" in (rec.created[0].get("title", "").lower())
    assert report.source_counts["reviews"] == 1        # 111 filtered out
    assert removed is True


def test_graphql_down_rest_up_files_no_review_nodes(tmp_path: Path):
    """Codex P1: GraphQL (resolved state) fails but REST inline comments
    succeed. We must NOT harvest reviews (can't tell implemented from
    declined), so zero review nodes are filed and the sentinel is retained."""
    sentinel = _write_sentinel(tmp_path, mode="autonomous")
    rec = _Rec()
    inline = json.dumps([{"id": 222, "body": "![high] would-be candidate",
                           "html_url": "u", "user": {"login": "gemini[bot]"}}])

    def gh(args):
        if args[:2] == ["api", "graphql"]:
            return 1, "", "HTTP 403 (GraphQL permission)"   # resolved unreadable
        return 0, inline, ""                                # REST still works

    report, removed = process_sentinel_file(
        sentinel, repo_root=tmp_path, existing_nodes=[], comments=None,
        gh_runner=gh, create_fn=rec.create, inbox_fn=rec.inbox_append,
    )
    assert rec.created == []                # no false re-file despite REST up
    assert report.source_counts["reviews"] == 0
    assert removed is False and sentinel.exists()
