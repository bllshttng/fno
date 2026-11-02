"""Harness-store fallback for unregistered sessions.

The registry is a cache of reality: a real session with no roster row must be
resolvable, adopted, and then addressable -- without ever guessing on ambiguity
or claiming a dead session is live.
"""
from __future__ import annotations

import json
import os
import time

import pytest

from fno.agents import discover, store_fallback
from fno.agents.registry import AgentResolutionError

CLAUDE_UUID = "c655c326-1111-2222-3333-444455556666"
CODEX_UUID = "c655c326-aaaa-bbbb-cccc-ddddeeeeffff"


@pytest.fixture(autouse=True)
def _registry_home(tmp_path, monkeypatch):
    """Point the registry + every harness store at scratch dirs.

    The suite's $HOME redirect is session-scoped, so the registry would otherwise
    accumulate rows across tests in this file.
    """
    (tmp_path / "agents").mkdir()
    registry = tmp_path / "agents" / "registry.json"
    monkeypatch.setattr("fno.paths.agents_registry_path", lambda: registry)
    projects = tmp_path / "projects"
    codex = tmp_path / "codex"
    projects.mkdir()
    codex.mkdir()
    # Through the real env seams discover owns, so the test exercises the same
    # resolution path production does.
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(projects))
    monkeypatch.setenv(discover.CODEX_SESSIONS_DIR_ENV, str(codex))
    # Run from a cwd that is NOT a known project: project confinement (defect 1)
    # is N/A outside a project, so these adoption/ambiguity-mechanic tests
    # proceed as before. The confined path (same-project adopt, foreign refuse)
    # is covered by test_store_fallback_confinement.py.
    monkeypatch.chdir(tmp_path)
    return tmp_path


def _write_claude_session(root, uuid, cwd="/repo/one", project="-repo-one"):
    pdir = root / "projects" / project
    pdir.mkdir(parents=True, exist_ok=True)
    lines = [
        json.dumps({"type": "summary", "sessionId": uuid}),
        json.dumps({"type": "user", "sessionId": uuid, "cwd": cwd}),
    ]
    (pdir / f"{uuid}.jsonl").write_text("\n".join(lines) + "\n", encoding="utf-8")


def _write_codex_session(root, uuid, cwd="/repo/two"):
    d = root / "codex" / "2026" / "07" / "20"
    d.mkdir(parents=True, exist_ok=True)
    meta = json.dumps({"type": "session_meta", "payload": {"id": uuid, "cwd": cwd}})
    (d / f"rollout-2026-07-20T10-00-00-{uuid}.jsonl").write_text(
        meta + "\n", encoding="utf-8"
    )


def _write_turn_named_codex_session(root, uuid, cwd="/repo/two"):
    d = root / "codex" / "2026" / "07" / "20"
    d.mkdir(parents=True, exist_ok=True)
    meta = json.dumps({"type": "session_meta", "payload": {"id": uuid, "cwd": cwd}})
    (d / "rollout-2026-07-20T10-00-00-turn_12345.jsonl").write_text(
        meta + "\n", encoding="utf-8"
    )


def test_default_codex_sessions_dir_honors_codex_home(
    monkeypatch, tmp_path
):
    monkeypatch.delenv(discover.CODEX_SESSIONS_DIR_ENV)
    monkeypatch.setenv("CODEX_HOME", str(tmp_path / "custom-codex"))

    assert discover.default_codex_sessions_dir() == tmp_path / "custom-codex" / "sessions"


# --- shape gate ------------------------------------------------------------


# --- US1: claude attach on an unregistered session --------------------------


def test_real_unreadable_directory_refuses_partial_store_answer(
    _registry_home, monkeypatch
):
    """Filesystem traversal errors are coverage failures, not empty directories."""
    matching = "019fb417-1111-7222-8333-4444deadbeef"
    _write_codex_session(_registry_home, matching)
    blocked = _registry_home / "projects" / "blocked"
    blocked.mkdir()
    (blocked / "unrelated.jsonl").write_text("{}\n")
    blocked.chmod(0)
    try:
        if os.access(blocked, os.R_OK | os.X_OK):
            pytest.skip("test user can still enumerate mode-000 directories")
        with pytest.raises(AgentResolutionError, match="claude") as exc:
            store_fallback.complete_store_hits("deadbeef")
    finally:
        blocked.chmod(0o700)

    assert matching in str(exc.value)


def test_turn_named_codex_rollout_resolves_from_session_metadata(_registry_home):
    session_id = "019fb417-1111-7222-8333-4444deadbeef"
    _write_turn_named_codex_session(_registry_home, session_id)

    for token in (session_id, "deadbeef", "019fb417"):
        hits = store_fallback.complete_store_hits(token)
        assert [(hit.harness, hit.session_id) for hit in hits] == [
            ("codex", session_id)
        ]


# --- command wiring: the heal must reach every resolution surface --------------


# --- review fixes: ambiguity must not fall through, healed rows must survive --


def test_invalid_utf8_transcript_does_not_crash(_registry_home):
    """UnicodeDecodeError is a ValueError, so it would bypass an OSError-only
    guard and crash resolution from the iteration itself."""
    pdir = _registry_home / "projects" / "-repo-one"
    pdir.mkdir(parents=True, exist_ok=True)
    (pdir / f"{CLAUDE_UUID}.jsonl").write_bytes(
        b'{"type":"summary"}\n\xff\xfe not utf-8 \xff\n'
    )

    hits = store_fallback.probe_stores("c655c326")

    assert [h.session_id for h in hits] == [CLAUDE_UUID]
    assert hits[0].cwd == ""


def test_canonical_tail_resolves_mixed_case_opencode_store(tmp_path, monkeypatch):
    import sqlite3

    storage = tmp_path / "opencode" / "storage"
    storage.mkdir(parents=True)
    db = storage.parent / "opencode.db"
    con = sqlite3.connect(db)
    con.execute("CREATE TABLE session (id TEXT, directory TEXT)")
    sid = "ses_7f3a9b2cAbCd1234"
    con.execute("INSERT INTO session VALUES (?, ?)", (sid, "/x"))
    con.commit()
    con.close()
    monkeypatch.setenv("FNO_OPENCODE_STORAGE_DIR", str(storage))
    hits = store_fallback.probe_stores("AbCd1234")
    assert [hit.session_id for hit in hits] == [sid]
    assert store_fallback.probe_stores("abcd1234") == []


# --- rm tombstone: a just-removed session is not re-adopted ------------------


def _write_tombstone(root, harness, session_id, *, age_s=0):
    """Stage the tombstone file `fno agents rm` writes beside the registry."""
    removed_at = int(time.time()) - age_s
    (root / "agents" / "rm_tombstones.json").write_text(
        json.dumps(
            [{"harness": harness, "session_id": session_id, "removed_at": removed_at}]
        ),
        encoding="utf-8",
    )


