"""Tests for fno.agents.harnesses._claude_session_registry.

Helpers that read claude's session registry and jobs directory. All paths
derive from ``Path.home()`` so the tests monkeypatch ``HOME`` to a tmp dir.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest


def _claude_home_setup(tmp_path: Path, monkeypatch) -> Path:
    """Point HOME at tmp_path and prepare ~/.claude/{sessions,jobs}.

    ``FNO_CONFIG`` is pinned to an empty tmp file so a developer's real
    ``.fno/config.toml`` never leaks an account root into a test, and an
    ambient ``CLAUDE_CONFIG_DIR`` is dropped for the same reason.
    """
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("CLAUDE_CONFIG_DIR", raising=False)
    config = tmp_path / "config.toml"
    config.touch()
    monkeypatch.setenv("FNO_CONFIG", str(config))
    (tmp_path / ".claude" / "sessions").mkdir(parents=True)
    (tmp_path / ".claude" / "jobs").mkdir(parents=True)
    return tmp_path


def _write_session_file(home: Path, pid: int, **fields) -> Path:
    """Write a ~/.claude/sessions/<pid>.json with the given fields."""
    base = {
        "messagingSocketPath": fields.get("messagingSocketPath", f"/tmp/sock-{pid}"),
        "jobId": fields["jobId"],
        "kind": fields.get("kind", "bg"),
        "sessionId": fields.get("sessionId", f"sess-{pid}"),
        "cwd": fields.get("cwd", "/tmp"),
        "status": fields.get("status", "running"),
        "state": fields.get("state", "idle"),
        "detail": fields.get("detail", ""),
        "tempo": fields.get("tempo", 1),
    }
    path = home / ".claude" / "sessions" / f"{pid}.json"
    path.write_text(json.dumps(base), encoding="utf-8")
    return path


def _write_timeline_jsonl(home: Path, short_id: str, rows: list[dict]) -> Path:
    jobs_dir = home / ".claude" / "jobs" / short_id
    jobs_dir.mkdir(parents=True, exist_ok=True)
    path = jobs_dir / "timeline.jsonl"
    path.write_text("\n".join(json.dumps(r) for r in rows) + "\n", encoding="utf-8")
    return path


# ---------------------------------------------------------------------------
# locate_session
# ---------------------------------------------------------------------------


def test_locate_session_returns_locator_when_jobid_matches(tmp_path, monkeypatch):
    """jobId match + kind=bg + non-null socket -> SessionLocator returned."""
    from fno.agents.harnesses._claude_session_registry import locate_session

    home = _claude_home_setup(tmp_path, monkeypatch)
    _write_session_file(home, pid=12345, jobId="7c5dcf5d", kind="bg",
                        messagingSocketPath="/tmp/sock.msg",
                        sessionId="claude-sess-42", cwd="/work/project")

    loc = locate_session("7c5dcf5d")
    assert loc is not None
    assert loc.pid == 12345
    assert loc.messaging_socket_path == "/tmp/sock.msg"
    assert loc.jobs_dir == home / ".claude" / "jobs" / "7c5dcf5d"
    assert loc.session_id == "claude-sess-42"
    assert loc.cwd == "/work/project"


def test_locate_session_returns_none_when_socket_null(tmp_path, monkeypatch):
    """Match exists but messagingSocketPath=null (suspended) -> None."""
    from fno.agents.harnesses._claude_session_registry import locate_session

    home = _claude_home_setup(tmp_path, monkeypatch)
    _write_session_file(home, pid=99, jobId="abc12345",
                        messagingSocketPath=None)

    assert locate_session("abc12345") is None


# ---------------------------------------------------------------------------
# read_state_json
# ---------------------------------------------------------------------------


def test_read_state_json_retries_once_on_json_decode_error(tmp_path, monkeypatch):
    """Atomic-rename window: read_state_json retries once on JSONDecodeError."""
    from fno.agents.harnesses import _claude_session_registry as mod

    home = _claude_home_setup(tmp_path, monkeypatch)
    jobs_dir = home / ".claude" / "jobs" / "race0001"
    jobs_dir.mkdir(parents=True, exist_ok=True)
    state_path = jobs_dir / "state.json"
    # First read sees an empty file (caught mid-rename); second sees valid JSON.
    state_path.write_text("", encoding="utf-8")

    real_read_text = Path.read_text
    call_count = {"n": 0}

    def fake_read_text(self, *args, **kwargs):
        if self == state_path:
            call_count["n"] += 1
            if call_count["n"] == 1:
                return ""  # invalid
            return json.dumps(
                {"state": "done", "updatedAt": "T1", "output": {"result": "x"}}
            )
        return real_read_text(self, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", fake_read_text)
    # Make retry instantaneous so the test stays fast.
    monkeypatch.setattr(mod, "_RETRY_BACKOFF_SEC", 0.0)

    snap = mod.read_state_json(jobs_dir)
    assert call_count["n"] == 2
    assert snap.state == "done"
    assert snap.output_result == "x"


def test_read_state_json_rejects_non_object_json(tmp_path, monkeypatch):
    """A valid-but-non-object state.json (list/primitive) raises JSONDecodeError,
    not AttributeError, so callers' `except json.JSONDecodeError` degrades cleanly."""
    from fno.agents.harnesses._claude_session_registry import read_state_json

    home = _claude_home_setup(tmp_path, monkeypatch)
    jobs_dir = home / ".claude" / "jobs" / "abc12345"
    jobs_dir.mkdir(parents=True)
    (jobs_dir / "state.json").write_text("[]", encoding="utf-8")

    with pytest.raises(json.JSONDecodeError):
        read_state_json(jobs_dir)


# ---------------------------------------------------------------------------
# read_timeline_tail
# ---------------------------------------------------------------------------


def test_read_timeline_tail_concatenates_terminal_state_text(tmp_path, monkeypatch):
    """Only lines with state in {done, completed, failed, needs-input} contribute text."""
    from fno.agents.harnesses._claude_session_registry import read_timeline_tail

    home = _claude_home_setup(tmp_path, monkeypatch)
    jobs_dir = home / ".claude" / "jobs" / "mixed001"
    _write_timeline_jsonl(home, "mixed001", [
        {"at": "T0", "state": "running", "text": "should-be-ignored"},
        {"at": "T1", "state": "done", "text": "first part"},
        {"at": "T2", "state": "completed", "text": "second part"},
        {"at": "T3", "state": "idle", "text": "not terminal"},
    ])
    # offset=0 reads everything appended since baseline.
    tail = read_timeline_tail(jobs_dir, offset=0)
    assert "first part" in tail
    assert "second part" in tail
    assert "should-be-ignored" not in tail
    assert "not terminal" not in tail


def test_read_timeline_tail_starts_at_offset(tmp_path, monkeypatch):
    """Tail reads only bytes >= offset; earlier rows are excluded."""
    from fno.agents.harnesses._claude_session_registry import read_timeline_tail

    home = _claude_home_setup(tmp_path, monkeypatch)
    jobs_dir = home / ".claude" / "jobs" / "offset01"

    # Write a baseline row, capture its size, then append a post-baseline row.
    timeline = _write_timeline_jsonl(home, "offset01", [
        {"at": "T0", "state": "done", "text": "pre-baseline"},
    ])
    baseline = timeline.stat().st_size

    with open(timeline, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(
            {"at": "T1", "state": "done", "text": "post-baseline"}
        ) + "\n")

    tail = read_timeline_tail(jobs_dir, offset=baseline)
    assert "post-baseline" in tail
    assert "pre-baseline" not in tail


# ---------------------------------------------------------------------------
# account roots
# ---------------------------------------------------------------------------


def _register_claude_account(tmp_path: Path, monkeypatch, acct: Path) -> None:
    """Write a config naming ``acct`` as a managed claude account's config_dir."""
    config = tmp_path / "accounts-config.toml"
    config.write_text(
        "[[accounts.records]]\n"
        'id = "makers"\n'
        'name = "makers"\n'
        'harness = "claude"\n'
        'auth = "managed"\n'
        f'config_dir = "{acct}"\n',
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CONFIG", str(config))


def test_locate_session_reads_a_registered_account_root(tmp_path, monkeypatch):
    """A claude account record's config_dir joins the root set: a record
    written under <config_dir>/sessions is found, its jobs_dir is the
    account's once that job dir exists, and the full UUID resolves."""
    from fno.agents.harnesses._claude_session_registry import (
        locate_session,
        resolve_session_uuid,
    )

    home = _claude_home_setup(tmp_path, monkeypatch)
    acct = tmp_path / "acct"
    (acct / "sessions").mkdir(parents=True)
    _register_claude_account(tmp_path, monkeypatch, acct)
    (acct / "sessions" / "4242.json").write_text(
        json.dumps(
            {
                "messagingSocketPath": "/tmp/sock-acct",
                "jobId": "feedc0de",
                "kind": "bg",
                "sessionId": "sess-feedc0de",
                "cwd": "/tmp",
            }
        ),
        encoding="utf-8",
    )

    loc = locate_session("feedc0de")
    assert loc is not None
    assert loc.messaging_socket_path == "/tmp/sock-acct"
    assert loc.session_id == "sess-feedc0de"
    (acct / "jobs" / "feedc0de").mkdir(parents=True)
    assert locate_session("feedc0de").jobs_dir == acct / "jobs" / "feedc0de"
    assert resolve_session_uuid("feedc0de") == "sess-feedc0de"
    assert home is not None


def test_account_roots_fail_open_on_unreadable_config(tmp_path, monkeypatch):
    """A config that fails to parse leaves the ambient roots: HOME's workers
    stay visible and nothing raises."""
    from fno.agents.harnesses._claude_session_registry import (
        locate_session,
        session_dirs,
    )

    home = _claude_home_setup(tmp_path, monkeypatch)
    _write_session_file(home, pid=111, jobId="aaaa1111")
    config = tmp_path / "broken.toml"
    config.write_text("[[accounts.records\nid = never-closed", encoding="utf-8")
    monkeypatch.setenv("FNO_CONFIG", str(config))

    assert session_dirs() == [tmp_path / ".claude" / "sessions"]
    assert locate_session("aaaa1111") is not None


# ---------------------------------------------------------------------------
# resolve_session_uuid (socket-independent full-UUID resolution)
# ---------------------------------------------------------------------------


def test_resolve_session_uuid_returns_uuid_for_idle_socket_null(tmp_path, monkeypatch):
    """An IDLE bg session (messagingSocketPath=null) still resolves its full
    UUID. This is the whole point: locate_session SKIPS socket-null sessions,
    but an idle session is exactly the stream-json resume target, so the
    resolver must read sessionId regardless of socket state."""
    from fno.agents.harnesses._claude_session_registry import resolve_session_uuid

    home = _claude_home_setup(tmp_path, monkeypatch)
    full = "019e7157-4236-7bb1-b274-ebbac6040ace"
    _write_session_file(
        home, pid=4242, jobId="7c5dcf5d", kind="bg",
        messagingSocketPath=None, sessionId=full,
    )

    assert resolve_session_uuid("7c5dcf5d") == full


def test_resolve_session_uuid_prefers_live_socket_supervisor(tmp_path, monkeypatch):
    """After a supervisor auto-update respawn, the dead pid's file (socket-null)
    and the live pid's file can share a jobId. The resolver prefers the live
    supervisor's sessionId but still resolves if only the stale one remains."""
    from fno.agents.harnesses._claude_session_registry import resolve_session_uuid

    home = _claude_home_setup(tmp_path, monkeypatch)
    stale = "00000000-0000-0000-0000-000000000000"
    live = "019e7157-4236-7bb1-b274-ebbac6040ace"
    _write_session_file(
        home, pid=100, jobId="dup00000", kind="bg",
        messagingSocketPath=None, sessionId=stale,
    )
    _write_session_file(
        home, pid=200, jobId="dup00000", kind="bg",
        messagingSocketPath="/tmp/live.msg", sessionId=live,
    )

    assert resolve_session_uuid("dup00000") == live
