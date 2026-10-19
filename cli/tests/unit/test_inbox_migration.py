"""Tests for scripts/migrate-inbox-flat-to-threads.py round-tripping."""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest


def _load_migration_module():
    repo_root = Path(__file__).resolve().parents[3]
    script_path = repo_root / "scripts" / "migrate-inbox-flat-to-threads.py"
    spec = importlib.util.spec_from_file_location("inbox_migration", script_path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["inbox_migration"] = mod
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture
def inbox_root(tmp_path, monkeypatch):
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    return tmp_path


_FLAT_FIXTURE = """\
---
created: 2026-05-07T11:18
updated: 2026-05-07T11:37
---
# Inbox: alice

This file holds messages addressed to the alice agent.
Edit only via `fno inbox` commands.

## msg-c74627 · 2026-05-07 18:18 · from:bob · kind:heads-up
status: read
ref_pr: 409
triaged_into: ab-feedface

Backend now exposes region column on /api/records; web should plumb it.

## msg-d11111 · 2026-05-07 18:25 · from:alice · kind:notification · reply_to:msg-c74627
status: read
triaged_into: null

Got it; will plumb today.

## msg-e22222 · 2026-05-07 19:00 · from:carol · kind:question
status: unread
triaged_into: null

Should we roll the rotation queue back to 5 swaps before 3am?

## msg-f33333 · 2026-05-07 20:00 · from:dave · kind:lesson
status: read
triaged_into: null

filelock 3.x and fcntl.flock are wire-compatible on macOS; reuse the lock path.
"""


def _seed_flat(inbox_root: Path, project: str, content: str) -> Path:
    proj = inbox_root / project
    proj.mkdir(parents=True, exist_ok=True)
    flat = proj / "inbox.md"
    flat.write_text(content, encoding="utf-8")
    return flat


def test_migration_round_trips_kinds_and_threads(inbox_root):
    migration = _load_migration_module()
    flat = _seed_flat(inbox_root, "alice", _FLAT_FIXTURE)

    result = migration.migrate_project("alice", flat, dry_run=False)
    assert result["errors"] == []
    assert result["threads_written"] == 3  # heads-up + question + lesson roots
    assert result["messages_migrated"] == 4

    # inbox.md was moved to safety-net path
    assert not flat.exists()
    assert (inbox_root / "alice" / "inbox-pre-migration.md").exists()

    # Three thread files in inbox/
    thread_files = sorted((inbox_root / "alice" / "inbox").glob("*.md"))
    assert len(thread_files) == 3
