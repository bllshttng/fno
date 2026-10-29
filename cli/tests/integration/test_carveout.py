"""Integration tests for `fno carveout add` (Wave 2.1, US1).

Covers AC1-HP / AC1-ERR / AC1-UI / AC1-EDGE / AC1-FR from the retro-auto-triage
design. Capture must never be silently lost: a missing session degrades to an
unscoped record (exit 0 + warn), while a FAILED write exits non-zero.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


def _ledger(repo_root: Path) -> Path:
    from fno.paths import project_log

    return project_log("carveouts.jsonl", project_root=repo_root)


def _read_carveouts(repo_root: Path) -> list[dict]:
    f = _ledger(repo_root)
    assert f.exists(), f"expected carveouts ledger at {f}"
    return [json.loads(ln) for ln in f.read_text(encoding="utf-8").splitlines() if ln.strip()]


def _write_state(repo_root: Path, session_id: str) -> None:
    (repo_root / ".fno").mkdir(parents=True, exist_ok=True)
    (repo_root / ".fno" / "target-state.md").write_text(
        f"---\nstatus: IN_PROGRESS\nsession_id: {session_id}\n---\nbody\n",
        encoding="utf-8",
    )


@pytest.fixture(autouse=True)
def _repo(tmp_path: Path, monkeypatch):
    """Pin tmp_path as repo root and clear any inherited session env."""
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    monkeypatch.delenv("CLAUDECODE_SESSION_ID", raising=False)
    return tmp_path


def test_ac1_hp_active_session(_repo: Path):
    """AC1-HP: scoped record with all fields; id to stdout; exit 0."""
    sid = "20260524T120000Z-11111-abcdef"
    _write_state(_repo, sid)
    result = runner.invoke(
        app,
        ["carveout", "add", "--kind", "deferred", "--need", "which auth provider",
         "skipped SSO wiring pending provider choice"],
    )
    assert result.exit_code == 0, result.output
    entries = _read_carveouts(_repo)
    assert len(entries) == 1
    e = entries[0]
    assert e["kind"] == "deferred"
    assert e["need"] == "which auth provider"
    assert e["description"] == "skipped SSO wiring pending provider choice"
    assert e["session_id"] == sid
    assert e["ts"].endswith("Z")
    assert e["id"].startswith("cv-")
    # The new id is the value on stdout.
    assert e["id"] in result.stdout


def test_ac1_err_invalid_kind(_repo: Path):
    """AC1-ERR (variant): an unknown --kind value -> exit 2, nothing appended."""
    result = runner.invoke(app, ["carveout", "add", "--kind", "bogus", "desc"])
    assert result.exit_code == 2
    assert not _ledger(_repo).exists()


def test_ac1_ui_no_session(_repo: Path):
    """AC1-UI: no resolvable session -> unscoped record, stderr warn, exit 0."""
    # No target-state.md and no CLAUDECODE_SESSION_ID (cleared in fixture).
    result = runner.invoke(app, ["carveout", "add", "--kind", "oos-bug", "found a leak"])
    assert result.exit_code == 0, result.output
    entries = _read_carveouts(_repo)
    assert len(entries) == 1
    assert entries[0]["session_id"] is None
    # CliRunner merges stderr into output by default; the warning must surface.
    assert "no active session" in result.output


def test_ac1_edge_oversize_truncated(_repo: Path):
    """AC1-EDGE: a 50KB description is truncated to the cap with a marker, still appended."""
    from fno.carveout.core import DESCRIPTION_CAP

    big = "x" * 50_000
    result = runner.invoke(app, ["carveout", "add", "--kind", "deferred", big])
    assert result.exit_code == 0, result.output
    entries = _read_carveouts(_repo)
    assert len(entries) == 1
    e = entries[0]
    assert e["truncated"] is True
    assert "[truncated" in e["description"]
    # Body retained is the cap plus the marker; well under the original 50KB.
    assert len(e["description"]) < 50_000
    assert e["description"].startswith("x" * DESCRIPTION_CAP)


def test_ac1_fr_unwritable_ledger(_repo: Path):
    """AC1-FR: an unwritable ledger -> non-zero exit + stderr (no silent success)."""
    # Make the ledger path a directory so the append open() raises OSError.
    ledger = _ledger(_repo)
    ledger.parent.mkdir(parents=True, exist_ok=True)
    ledger.mkdir()
    result = runner.invoke(app, ["carveout", "add", "--kind", "deferred", "desc"])
    assert result.exit_code != 0
    assert "failed to record" in result.output


# -- carve-outs are durable across the worktree boundary --


def test_cli_ledger_climbs_to_canonical_root(tmp_path: Path, monkeypatch):
    """End-to-end: `fno carveout add` from a linked worktree writes the ledger
    under canonical while reading the session from the worktree."""
    import fno.paths as _paths

    worktree = tmp_path / "worktree"
    canonical = tmp_path / "canonical"
    (worktree / ".fno").mkdir(parents=True)
    (canonical / ".fno").mkdir(parents=True)
    (worktree / ".fno" / "target-state.md").write_text(
        "---\nstatus: IN_PROGRESS\nsession_id: wt-sess\n---\nbody\n", encoding="utf-8"
    )
    monkeypatch.setattr(_paths, "resolve_repo_root", lambda: worktree)
    monkeypatch.setattr(_paths, "resolve_canonical_repo_root", lambda: canonical)
    monkeypatch.delenv("CLAUDECODE_SESSION_ID", raising=False)

    result = runner.invoke(app, ["carveout", "add", "--kind", "deferred", "x"])
    assert result.exit_code == 0, result.output

    canon_ledger = _ledger(canonical)
    assert canon_ledger.exists()
    assert not _ledger(worktree).exists()
    entry = json.loads(canon_ledger.read_text(encoding="utf-8").splitlines()[0])
    assert entry["session_id"] == "wt-sess"


# -- backfill carve-out kind + list/resolve surface --
#
# A `backfill` carve-out declares a data backfill the merged PR enables; --need
# carries its PRECONDITION (not an open question). /fno:ship pr merged's backfill slot
# reads surviving backfill entries (`list`) and removes handled ones (`resolve`).


def test_carveout_list_filters_by_kind_json(_repo: Path):
    """`fno carveout list --kind backfill --json` emits ONLY backfill rows as JSONL."""
    ledger = _ledger(_repo)
    ledger.parent.mkdir(parents=True, exist_ok=True)
    ledger.write_text(
        '{"id":"cv-d","kind":"deferred","need":"q","description":"a deferred thing"}\n'
        '{"id":"cv-b","kind":"backfill","need":"mig X","description":"backfill cmd"}\n'
        '{"id":"cv-o","kind":"oos-bug","description":"a bug"}\n',
        encoding="utf-8",
    )
    result = runner.invoke(app, ["carveout", "list", "--kind", "backfill", "--json"])
    assert result.exit_code == 0, result.output
    rows = [json.loads(ln) for ln in result.stdout.splitlines() if ln.strip().startswith("{")]
    assert [r["id"] for r in rows] == ["cv-b"]
    assert rows[0]["kind"] == "backfill"


def test_carveout_list_unreadable_ledger_fails_loud(_repo: Path):
    """A present-but-unreadable ledger is a FAILED read, NOT "no carve-outs":
    exit non-zero + stderr (never a silent empty that drops a real backfill)."""
    # Make the ledger PATH a directory: it exists() but read_text raises OSError.
    ledger = _ledger(_repo)
    ledger.parent.mkdir(parents=True, exist_ok=True)
    ledger.mkdir()
    result = runner.invoke(app, ["carveout", "list", "--kind", "backfill", "--json"])
    assert result.exit_code != 0
    assert "failed to read" in result.output


def test_carveout_resolve_removes_handled_ids(_repo: Path):
    """`fno carveout resolve <id>` removes handled backfill entries so a re-offer never repeats them."""
    ledger = _ledger(_repo)
    ledger.parent.mkdir(parents=True, exist_ok=True)
    ledger.write_text(
        '{"id":"cv-b","kind":"backfill","description":"b"}\n'
        '{"id":"cv-keep","kind":"backfill","description":"k"}\n',
        encoding="utf-8",
    )
    result = runner.invoke(app, ["carveout", "resolve", "cv-b"])
    assert result.exit_code == 0, result.output
    assert "resolved 1 carve-out" in result.stdout
    remaining = {
        json.loads(ln)["id"]
        for ln in ledger.read_text(encoding="utf-8").splitlines()
        if ln.strip()
    }
    assert remaining == {"cv-keep"}


# -- `carveout list --pr-number` ledger join --


def _pin_ledger(monkeypatch, path: Path, slug="bllshttng/fno") -> None:
    import fno.graph._reconcile as R
    import fno.paths as P
    monkeypatch.setattr(P, "ledger_json", lambda: path)
    monkeypatch.setattr(R, "resolve_current_repo_slug", lambda *a, **k: slug)


def _write_backfills(repo: Path) -> None:
    ledger = _ledger(repo)
    ledger.parent.mkdir(parents=True, exist_ok=True)
    ledger.write_text(
        '{"id":"cv-a","kind":"backfill","session_id":"S1","description":"mine"}\n'
        '{"id":"cv-b","kind":"backfill","session_id":"S2","description":"also mine"}\n'
        '{"id":"cv-c","kind":"backfill","session_id":"S9","description":"theirs"}\n',
        encoding="utf-8",
    )


def test_list_pr_number_resolves_both_sessions(_repo: Path, monkeypatch):
    """AC2-HP: a PR-480-shaped entry resolves BOTH session ids, and the listed
    carve-outs are exactly theirs - not the whole ledger."""
    lj = _repo / "ledger.json"
    lj.write_text(json.dumps({"entries": [
        {"pr_number": 480, "sessions": ["S1"], "session_id": "S2",
         "pr_url": "https://github.com/bllshttng/fno/pull/480"},
        {"pr_number": 999, "session_id": "S9"},
    ]}), encoding="utf-8")
    _pin_ledger(monkeypatch, lj)
    _write_backfills(_repo)

    result = runner.invoke(
        app, ["carveout", "list", "--kind", "backfill", "--pr-number", "480", "--json"]
    )
    assert result.exit_code == 0, result.output
    out = json.loads(result.stdout.strip())
    assert sorted(out["sessions_resolved"]) == ["S1", "S2"]
    assert out["reason"] is None and out["consumable"] is True
    assert [c["id"] for c in out["carveouts"]] == ["cv-a", "cv-b"]


def test_list_pr_number_unreadable_ledger_is_not_no_match(_repo: Path, monkeypatch):
    """The bug this command replaces: a broken read must not read as 'no owning
    session'. The reason distinguishes them."""
    lj = _repo / "ledger.json"
    lj.write_text("{not json", encoding="utf-8")
    _pin_ledger(monkeypatch, lj)
    _write_backfills(_repo)

    out = json.loads(runner.invoke(
        app, ["carveout", "list", "--kind", "backfill", "--pr-number", "480", "--json"]
    ).stdout.strip())
    assert out["consumable"] is False
    assert "unreadable" in out["reason"]


def test_list_pr_number_unresolvable_repo_refuses_to_attribute(_repo: Path, monkeypatch):
    """No repo slug -> PR numbers collide across repos, so ownership is refused."""
    lj = _repo / "ledger.json"
    lj.write_text(json.dumps({"entries": [{"pr_number": 480, "session_id": "S1"}]}),
                  encoding="utf-8")
    _pin_ledger(monkeypatch, lj, slug=None)
    _write_backfills(_repo)

    out = json.loads(runner.invoke(
        app, ["carveout", "list", "--kind", "backfill", "--pr-number", "480", "--json"]
    ).stdout.strip())
    assert out["sessions_resolved"] == [] and out["consumable"] is False
    assert "repo slug unresolved" in out["reason"]


def test_list_pr_number_refuses_url_less_ledger_rows(_repo: Path, monkeypatch):
    """A url-less ledger row carries no repo, and the ledger is GLOBAL - matching
    it on the bare number could claim a foreign PR's sessions and then CONSUME
    (destroy) their backfills. The consuming caller must refuse."""
    lj = _repo / "ledger.json"
    lj.write_text(json.dumps({"entries": [{"pr_number": 480, "session_id": "S1"}]}),
                  encoding="utf-8")
    _pin_ledger(monkeypatch, lj)
    _write_backfills(_repo)

    out = json.loads(runner.invoke(
        app, ["carveout", "list", "--kind", "backfill", "--pr-number", "480", "--json"]
    ).stdout.strip())
    assert out["sessions_resolved"] == [] and out["consumable"] is False
