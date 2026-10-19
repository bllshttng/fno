"""Integration tests for retro land hybrid-by-mode (Wave 4.1, US4)."""
from __future__ import annotations

from pathlib import Path

from fno.retro.land import (
    MODE_AUTONOMOUS,
    MODE_INTERACTIVE,
    land_candidates,
    resolve_mode,
)
from fno.retro.types import TIER_INBOX, TIER_NODE, Candidate


def _node(title="t", tier=TIER_NODE, pr=343, sid="c1", chash="abc") -> Candidate:
    return Candidate(
        title=title, body="reasoning", tier=tier, priority="p1",
        source_pr=pr, source_id=sid, content_hash=chash,
    )


class _Recorder:
    def __init__(self):
        self.created = []
        self.inbox = []
        self._n = 0

    def create(self, *, title, details, priority, difficulty, project, cwd, domain="code", queued=False):
        self._n += 1
        nid = f"ab-{self._n:08d}"
        self.created.append(
            {
                "id": nid,
                "title": title,
                "details": details,
                "cwd": cwd,
                "difficulty": difficulty,
                "queued": queued,
            }
        )
        return nid

    def inbox_append(self, candidate):
        self.inbox.append(candidate)


def test_ac4_edge_nit_goes_to_inbox(tmp_path: Path):
    """AC4-EDGE: a low/nit (tier=inbox) candidate -> inbox line, no node."""
    rec = _Recorder()
    results = land_candidates(
        [_node(tier=TIER_INBOX)], mode=MODE_INTERACTIVE, repo_root=tmp_path,
        create_fn=rec.create, inbox_fn=rec.inbox_append,
    )
    assert rec.created == []
    assert len(rec.inbox) == 1
    assert results[0].outcome == "inbox"


def test_ac4_mode_absent_defaults_interactive():
    """AC4-MODE: sentinel without a readable mode -> interactive (safe)."""
    assert resolve_mode(None) == MODE_INTERACTIVE
    assert resolve_mode({}) == MODE_INTERACTIVE
    assert resolve_mode({"mode": "garbage"}) == MODE_INTERACTIVE
    assert resolve_mode({"mode": "autonomous"}) == MODE_AUTONOMOUS


def test_ac4_fr_partial_progress_persists(tmp_path: Path):
    """AC4-FR: when one of several fails, the others still land (re-run dedups the rest)."""
    rec = _Recorder()
    n_calls = {"i": 0}

    def flaky(**kwargs):
        n_calls["i"] += 1
        if n_calls["i"] == 2:
            raise TimeoutError("lock timeout on #2")
        return rec.create(**kwargs)

    cands = [_node(sid="a"), _node(sid="b"), _node(sid="c")]
    results = land_candidates(
        cands, mode=MODE_AUTONOMOUS, repo_root=tmp_path,
        create_fn=flaky, inbox_fn=rec.inbox_append,
    )
    outcomes = [r.outcome for r in results]
    assert outcomes.count("active") == 2
    assert outcomes.count("failed") == 1


# ---------------------------------------------------------------------------
# v2 A1 - the retro-harvest birth path routes through on_node_born
# (the exact gap this epic fixes: x-7c38 / x-6e23 filed with no /think).
# ---------------------------------------------------------------------------


def test_a1_birth_hook_fires_per_created_node(tmp_path, monkeypatch):
    """Every retro-filed node routes through the birth hook once; the hook
    itself is the native door forwarder, so the batch needs no shared state."""
    import fno.provenance.spawn_think as st

    calls: list[str] = []
    monkeypatch.setattr(st, "on_node_born", lambda node, **k: calls.append(node["id"]))
    rec = _Recorder()
    land_candidates(
        [_node(title="a", chash="h1"), _node(title="b", chash="h2")],
        mode=MODE_AUTONOMOUS, repo_root=tmp_path,
        create_fn=rec.create, inbox_fn=rec.inbox_append,
    )
    assert calls == ["ab-00000001", "ab-00000002"]
