"""A codex thread worker proves its identity from its own spawn record.

The process-tree walk cannot answer for the thread lane: every thread worker
is a WebSocket client of the ONE shared ``codex app-server`` daemon, so N
workers share one pid and ancestry cannot name which thread a shell belongs
to. The spawn record can. These tests pin the cwd-keyed read of that record.
"""

import json

from fno.harness_identity import live_thread_row_for_cwd
from fno.paths import agents_registry_path
from fno.paths_testing import use_tmpdir


def _thread_row(name, cwd, session_id, *, status="live", substrate="thread"):
    return {
        "name": name,
        "status": status,
        "substrate": substrate,
        "harness": "codex",
        "harness_session_id": session_id,
        "cwd": cwd,
    }


def _write_registry(tmp_path, monkeypatch, rows, *, raw=None):
    use_tmpdir(monkeypatch, tmp_path)
    path = agents_registry_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    if raw is not None:
        path.write_bytes(raw)
    else:
        path.write_text(json.dumps({"schema_version": 23, "agents": rows}))
    return path


def test_duplicate_cwd_refuses_rather_than_picks(tmp_path, monkeypatch):
    cwd_a = tmp_path / "worker-a"
    cwd_a.mkdir()
    rows = [
        _thread_row("worker-a", str(cwd_a), "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"),
        _thread_row("worker-a-2", str(cwd_a), "019f48e1-5b09-72a0-9bc8-6b364bcf4ae5"),
    ]
    _write_registry(tmp_path, monkeypatch, rows)
    assert live_thread_row_for_cwd(str(cwd_a)) is None


def test_exited_rows_own_no_identity(tmp_path, monkeypatch):
    cwd_a = tmp_path / "worker-a"
    cwd_a.mkdir()
    rows = [
        _thread_row(
            "worker-a",
            str(cwd_a),
            "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4",
            status="exited",
        )
    ]
    _write_registry(tmp_path, monkeypatch, rows)
    assert live_thread_row_for_cwd(str(cwd_a)) is None
