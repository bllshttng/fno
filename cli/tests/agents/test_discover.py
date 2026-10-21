"""Tests for fno.agents.discover — P1 live-session discovery.

Covers AC1-HP (legible handle + status), AC1-ERR (malformed skipped),
AC1-EDGE (sync-conflict + transcripts ignored, strict pattern), AC1-EDGE2
(worktree -> parent repo; retired stale alias), AC1-FR (vanished mid-scan is
not-live), AC5-FR (all-malformed -> zero rows, no crash).

Liveness is controlled via a fake psutil so the suite is deterministic and
host-independent.
"""

from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import builtins
import fcntl
import json
import subprocess
import time
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import pytest

from fno.agents import discover
from fno.paths_testing import use_tmpdir


@pytest.fixture(autouse=True)
def _isolate_native_harness_stores(monkeypatch):
    """Keep resolver tests independent of the operator's real session stores."""
    monkeypatch.setattr(
        discover,
        "_reachable_from_harness_stores",
        lambda _token: ([], True),
    )


class _FakeProc:
    def __init__(self, create_time: float):
        self._ct = create_time

    def create_time(self) -> float:
        return self._ct


class _FakePsutil:
    """Minimal psutil stand-in: only the PIDs in ``alive`` exist."""

    class NoSuchProcess(Exception):
        pass

    def __init__(self, alive: dict[int, float]):
        self._alive = alive

    def Process(self, pid: int):  # noqa: N802 (mirror psutil API)
        if pid not in self._alive:
            raise self.NoSuchProcess(pid)
        return _FakeProc(self._alive[pid])


def _write_session(
    sdir: Path,
    pid: int,
    *,
    session_id: str,
    job_id: str,
    cwd: str,
    status: str = "idle",
    proc_start: str | None = None,
    create_time: float | None = None,
) -> float:
    """Write a <pid>.json registry file; return its create_time epoch.

    ``proc_start`` defaults to the ctime string of ``create_time`` so the
    reuse-safe liveness check matches by construction.
    """
    ct = create_time if create_time is not None else time.time() - 30
    sdir.mkdir(parents=True, exist_ok=True)
    payload = {
        "pid": pid,
        "sessionId": session_id,
        "cwd": cwd,
        "procStart": proc_start if proc_start is not None else time.ctime(ct),
        "kind": "bg",
        "name": job_id,
        "jobId": job_id,
        "agent": "claude",
        "status": status,
    }
    (sdir / f"{pid}.json").write_text(json.dumps(payload), encoding="utf-8")
    return ct


def _run(sdir, alive, tmp_path, **kw):
    return discover.discover_live_sessions(
        sessions_dir=sdir,
        name_map_path=tmp_path / ".fno" / "session-names.json",
        psutil_mod=_FakePsutil(alive),
        project_resolver=kw.pop("project_resolver", lambda c: None),
        **kw,
    )


def test_discover_walks_a_registered_account_root(tmp_path, monkeypatch):
    """With no FNO_CLAUDE_SESSIONS_DIR, discovery walks every root: a record
    under a registered claude account's sessions dir is a candidate. The env
    override still wins, so the test seam stays hermetic."""
    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.delenv("CLAUDE_CONFIG_DIR", raising=False)
    monkeypatch.delenv(discover.SESSIONS_DIR_ENV, raising=False)
    acct = tmp_path / "acct"
    acct.mkdir()
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
    sdir = acct / "sessions"
    ct = _write_session(sdir, 4242, session_id="uuid-acct", job_id="feedc0de", cwd="/tmp")
    kwargs = dict(
        name_map_path=tmp_path / ".fno" / "session-names.json",
        psutil_mod=_FakePsutil({4242: ct}),
        project_resolver=lambda c: None,
        registry_path=tmp_path / "registry.json",
    )
    sessions = discover.discover_live_sessions(**kwargs)
    assert "feedc0de" in [s.short_id for s in sessions]

    override = tmp_path / "override"
    monkeypatch.setenv(discover.SESSIONS_DIR_ENV, str(override))
    assert discover.discover_live_sessions(**kwargs) == []


def test_pid_reuse_rejected_by_proc_start_mismatch(tmp_path, monkeypatch):
    """A reused PID remains only an unclassified enumeration candidate."""
    use_tmpdir(monkeypatch, tmp_path)
    sdir = tmp_path / "sessions"
    # File records procStart for an OLD create time; the PID is alive now but
    # with a DIFFERENT (newer) create time -> reused -> not live.
    old_ct = time.time() - 10_000
    _write_session(
        sdir,
        501,
        session_id="uuid-reused",
        job_id="reuse000",
        cwd="/Users/x/code/proj",
        proc_start=time.ctime(old_ct),
    )
    new_ct = time.time() - 5  # different process now holds pid 501
    sessions = _run(sdir, {501: new_ct}, tmp_path)
    assert [s.short_id for s in sessions] == ["reuse000"]
    assert sessions[0].is_alive is False


def test_ac1_edge2_alias_stable_and_pid_miss_does_not_retire(tmp_path, monkeypatch):
    """A process miss cannot retire identity without family-1 death."""
    use_tmpdir(monkeypatch, tmp_path)
    sdir = tmp_path / "sessions"
    name_map = tmp_path / ".fno" / "session-names.json"
    ct1 = _write_session(
        sdir, 901, session_id="uuid-keep", job_id="keep0001", cwd="/Users/x/code/proj"
    )
    ct2 = _write_session(
        sdir, 902, session_id="uuid-stay", job_id="stay0001", cwd="/Users/x/code/proj"
    )

    def run(alive):
        return discover.discover_live_sessions(
            sessions_dir=sdir,
            name_map_path=name_map,
            psutil_mod=_FakePsutil(alive),
            project_resolver=lambda c: "proj",
        )

    first = {s.short_id: s.handle for s in run({901: ct1, 902: ct2})}
    assert first["keep0001"] == "proj-keep0001"
    # Same sessions, second call -> identical alias (stable).
    second = {s.short_id: s.handle for s in run({901: ct1, 902: ct2})}
    assert second["keep0001"] == first["keep0001"]
    # A PID miss alone preserves both identities.
    run({902: ct2})
    stored = json.loads(name_map.read_text(encoding="utf-8"))
    assert "uuid-keep" in stored
    assert "uuid-stay" in stored


def test_alias_lock_contention_is_bounded_to_canonical_handle(tmp_path, monkeypatch):
    """A busy UX alias map cannot block canonical mail addressing."""
    use_tmpdir(monkeypatch, tmp_path)
    sdir = tmp_path / "sessions"
    name_map = tmp_path / ".fno" / "session-names.json"
    session_id = "aaaaaaaa-1111-7222-8333-4444cafefeed"
    transcript = _write_session(
        sdir,
        913,
        session_id=session_id,
        job_id="deadbeef",
        cwd="/Users/x/code/project",
    )

    def scan():
        return discover.discover_live_sessions(
            sessions_dir=sdir,
            projects_dir=tmp_path / "no-projects",
            codex_sessions_dir=tmp_path / "no-codex",
            opencode_storage_dir=tmp_path / "no-opencode",
            name_map_path=name_map,
            registry_path=tmp_path / "no-registry.json",
            psutil_mod=_FakePsutil({913: transcript}),
            project_resolver=lambda _cwd: "project",
            classify_truth=False,
        )

    # Contrast pass: with the lock free the same scan writes the map, so the
    # contended pass can attribute its fallback to the lock timeout alone.
    baseline = scan()
    assert len(baseline) == 1
    assert name_map.exists()
    name_map.unlink()

    monkeypatch.setattr(discover, "_ALIAS_LOCK_TIMEOUT_SECONDS", 0.05)
    monkeypatch.setattr(discover, "_ALIAS_LOCK_POLL_SECONDS", 0.005)
    lock_path = name_map.with_suffix(name_map.suffix + ".lock")
    lock_path.parent.mkdir(parents=True, exist_ok=True)

    # The contender consumes its timeout on a controlled clock: monotonic
    # advances only when the lock loop sleeps, so a paused runner cannot
    # inflate the contended pass. Wall time is never measured.
    real_time = discover.time
    now = 0.0
    sleeps: list[float] = []

    def fake_sleep(seconds):
        nonlocal now
        sleeps.append(seconds)
        now += seconds

    monkeypatch.setattr(
        discover,
        "time",
        SimpleNamespace(
            monotonic=lambda: now,
            sleep=fake_sleep,
            time=lambda: real_time.time(),
        ),
    )

    with open(lock_path, "w") as holder:
        fcntl.flock(holder.fileno(), fcntl.LOCK_EX)
        sessions = scan()

    # Gave up at the deadline on the controlled clock, one poll-sized sleep
    # at a time, then fell back to the canonical handle without the write.
    budget = discover._ALIAS_LOCK_TIMEOUT_SECONDS
    poll = discover._ALIAS_LOCK_POLL_SECONDS
    assert sleeps
    assert all(d <= poll for d in sleeps)
    assert now == pytest.approx(budget, abs=poll / 2)
    assert [session.handle for session in sessions] == ["aaaaaaaa"]
    assert not name_map.exists()


def _resolve(handle, sdir, alive, tmp_path):
    return discover.resolve_or_suggest(
        handle,
        sessions_dir=sdir,
        name_map_path=tmp_path / ".fno" / "session-names.json",
        psutil_mod=_FakePsutil(alive),
        project_resolver=lambda c: "proj",
        truth_fn=lambda _session: {"state": "working"},
    )


def test_us2_resolve_by_hex_and_alias(tmp_path, monkeypatch):
    """US2: a send handle resolves by hex short-id AND by friendly alias."""
    use_tmpdir(monkeypatch, tmp_path)
    sdir = tmp_path / "sessions"
    ct = _write_session(
        sdir, 321, session_id="uuid-tgt", job_id="tgt00001", cwd="/Users/x/code/proj", status="idle"
    )
    # by hex
    match, sugg = _resolve("tgt00001", sdir, {321: ct}, tmp_path)
    assert match is not None and match.short_id == "tgt00001"
    assert match.project == "proj" and sugg == []
    # by friendly alias
    match2, _ = _resolve("proj-tgt00001", sdir, {321: ct}, tmp_path)
    assert match2 is not None and match2.short_id == "tgt00001"


# ---------------------------------------------------------------------------
# x-a1d5: canonical transcript-store fallback (~/.claude/projects)
#
# The sidecar is gone; liveness comes from a running ``claude`` process whose
# cwd maps to a projects subdir. A fake psutil supplies both process_iter (for
# the projects fallback) and create_time (for any sidecar row).
# ---------------------------------------------------------------------------

import os  # noqa: E402
import re  # noqa: E402


def _enc(cwd: str) -> str:
    return re.sub(r"[^a-zA-Z0-9]", "-", cwd)


class _FakeProcsProc:
    def __init__(self, cwd=None, create_time=None):
        self._cwd = cwd
        self._ct = create_time

    def cwd(self):
        if self._cwd is None:
            raise _FakeProcsPsutil.NoSuchProcess(0)
        return self._cwd

    def create_time(self):
        if self._ct is None:
            raise _FakeProcsPsutil.NoSuchProcess(0)
        return self._ct


class _IterItem:
    def __init__(self, pid, cmdline):
        self.info = {"pid": pid, "cmdline": cmdline}


class _FakeProcsPsutil:
    """psutil stand-in supporting process_iter + Process(pid).cwd()/create_time().

    ``procs`` maps pid -> (cmdline, cwd); ``alive`` maps pid -> create_time for
    any sidecar liveness check.
    """

    class NoSuchProcess(Exception):
        pass

    def __init__(self, procs=None, alive=None):
        self._procs = procs or {}
        self._alive = alive or {}

    def process_iter(self, _fields=None):
        return [_IterItem(pid, cmd) for pid, (cmd, _cwd) in self._procs.items()]

    def Process(self, pid):  # noqa: N802 (mirror psutil API)
        cwd = self._procs.get(pid, (None, None))[1]
        ct = self._alive.get(pid)
        if cwd is None and ct is None and pid not in self._procs:
            raise self.NoSuchProcess(pid)
        return _FakeProcsProc(cwd=cwd, create_time=ct)


def _write_transcript(projects_dir, *, cwd, session_id, mtime_age=5.0, body_cwd=True):
    pdir = projects_dir / _enc(cwd)
    pdir.mkdir(parents=True, exist_ok=True)
    f = pdir / f"{session_id}.jsonl"
    line = {"type": "user", "sessionId": session_id}
    if body_cwd:
        line["cwd"] = cwd
    f.write_text(json.dumps(line) + "\n", encoding="utf-8")
    mt = time.time() - mtime_age
    os.utime(f, (mt, mt))
    return f


def _claude_proc(pid, cwd, *, session_id=None):
    cmd = ["claude"]
    if session_id:
        cmd += ["--session-id", session_id]
    return pid, (cmd, cwd)


def _run_projects(tmp_path, projects_dir, procs, **kw):
    """Discover with an absent sidecar so the projects fallback fires."""
    pmap = dict(procs)
    return discover.discover_live_sessions(
        sessions_dir=tmp_path / "no-sessions-here",
        projects_dir=projects_dir,
        name_map_path=tmp_path / ".fno" / "session-names.json",
        psutil_mod=_FakeProcsPsutil(procs=pmap),
        project_resolver=kw.pop("project_resolver", lambda c: None),
        **kw,
    )


def test_x_a1d5_fallback_surfaces_live_session(tmp_path, monkeypatch):
    """AC1: with no sidecar, a live claude proc's transcript still surfaces."""
    use_tmpdir(monkeypatch, tmp_path)
    projects = tmp_path / "projects"
    sid = "02a5c8bc-c83c-4bb0-a473-19f85d0f3671"
    _write_transcript(projects, cwd="/Users/x/code/proj", session_id=sid)
    procs = [_claude_proc(4242, "/Users/x/code/proj", session_id=sid)]
    sessions = _run_projects(tmp_path, projects, procs)
    assert len(sessions) == 1
    s = sessions[0]
    assert s.session_id == sid
    assert s.short_id == "02a5c8bc"  # canonical first-eight
    assert s.cwd == "/Users/x/code/proj"  # from the live process
    assert s.pid == 4242  # real pid from the running claude
    assert s.agent == "claude"


def test_x_a1d5_session_id_from_newest_transcript_not_argv(tmp_path, monkeypatch):
    """Live id == newest transcript filename, even when argv --session-id differs.

    Proven on the real host: a claude ran with --session-id X but its on-disk
    transcript was a different id. The filename is canonical, argv is not.
    """
    use_tmpdir(monkeypatch, tmp_path)
    projects = tmp_path / "projects"
    _write_transcript(
        projects, cwd="/Users/x/code/proj", session_id="real-transcript-id", mtime_age=2
    )
    procs = [_claude_proc(4242, "/Users/x/code/proj", session_id="argv-only-id")]
    sessions = _run_projects(tmp_path, projects, procs)
    assert [s.session_id for s in sessions] == ["real-transcript-id"]


def test_x_a1d5_noise_ignored(tmp_path, monkeypatch):
    """sync-conflict copies, non-jsonl files, and subdirs never become sessions."""
    use_tmpdir(monkeypatch, tmp_path)
    projects = tmp_path / "projects"
    real = _write_transcript(
        projects, cwd="/Users/x/code/proj", session_id="real-sid", mtime_age=20
    )
    pdir = real.parent
    # A FRESHER sync-conflict copy must still lose to name-based skipping.
    conflict = pdir / "real-sid.sync-conflict-20260626.jsonl"
    conflict.write_text(json.dumps({"sessionId": "conflict"}) + "\n", encoding="utf-8")
    (pdir / "summary.md").write_text("# notes", encoding="utf-8")
    (pdir / "tool-results").mkdir()  # UUID subdir analog, never a session
    procs = [_claude_proc(4242, "/Users/x/code/proj")]
    sessions = _run_projects(tmp_path, projects, procs)
    assert [s.session_id for s in sessions] == ["real-sid"]


def test_x_a1d5_bg_infra_proc_excluded(tmp_path, monkeypatch):
    """A --bg-pty-host helper shares the claude binary but is not a session."""
    use_tmpdir(monkeypatch, tmp_path)
    projects = tmp_path / "projects"
    _write_transcript(projects, cwd="/Users/x/code/proj", session_id="real-sid")
    procs = {
        16637: (
            ["/Users/x/.local/share/claude/versions/2.1.193", "--bg-pty-host", "/tmp/x.sock"],
            "/Users/x/code/proj",
        ),
    }
    assert _run_projects(tmp_path, projects, list(procs.items())) == []


# --------------------------------------------------------------------------
# US2/US4: codex disk-discovery — a hand-started codex session is
# mail-able even without the registry hook. US3: cross-harness resolution.
# --------------------------------------------------------------------------


def _recovery_session_id(index: int) -> str:
    return f"01a039aa-0000-7000-8000-{index:012x}"


def _write_codex_rollout(
    codex_dir,
    *,
    session_id,
    cwd,
    mtime_age=5.0,
    meta=True,
    readable_turn=True,
    turn_timestamp="2026-08-25T15:00:00Z",
):
    """Write a rollout jsonl whose first line is a session_meta record."""
    day = codex_dir / "2026" / "07" / "09"
    day.mkdir(parents=True, exist_ok=True)
    f = day / f"rollout-2026-07-09T00-00-00-{session_id}.jsonl"
    if meta:
        line = {"type": "session_meta", "payload": {"id": session_id, "cwd": cwd}}
    else:
        line = {"type": "turn_context", "payload": {"nope": 1}}
    lines = [json.dumps(line)]
    if readable_turn:
        turn = {
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": f"resume {session_id}"}
                ],
            },
        }
        if turn_timestamp is not None:
            turn["timestamp"] = turn_timestamp
        lines.append(
            json.dumps(turn)
        )
    f.write_text("\n".join(lines) + "\n", encoding="utf-8")
    mt = time.time() - mtime_age
    os.utime(f, (mt, mt))
    return f


def _scan_recoverable(codex_dir, registry, *, cwd="/repo", recency_seconds=60.0):
    return discover.scan_recoverable_codex_rollouts(
        Path(cwd),
        recency_seconds,
        sessions_dir=codex_dir,
        registry_path=registry,
        now=time.time(),
    )


def test_recovery_scan_bounds_by_recency_exact_cwd_and_newest_full_id(tmp_path):
    codex = tmp_path / "codex"
    duplicate_id = _recovery_session_id(1)
    older = _write_codex_rollout(
        codex, session_id=duplicate_id, cwd="/repo", mtime_age=20.0
    )
    newer = codex / "2026" / "07" / "10" / f"rollout-new-{duplicate_id}.jsonl"
    newer.parent.mkdir(parents=True)
    newer.write_text(
        json.dumps(
            {"type": "session_meta", "payload": {"id": duplicate_id, "cwd": "/repo"}}
        )
        + "\n",
        encoding="utf-8",
    )
    newest_mtime = time.time() - 1.0
    os.utime(newer, (newest_mtime, newest_mtime))
    _write_codex_rollout(
        codex, session_id=_recovery_session_id(2), cwd="/repo", mtime_age=120.0
    )
    _write_codex_rollout(
        codex, session_id=_recovery_session_id(3), cwd="/repo/.", mtime_age=2.0
    )

    scan = _scan_recoverable(codex, tmp_path / "missing-registry.json")

    assert scan.complete is True
    assert scan.scanned_count == 4
    assert len(scan.recoverable) == 1
    rollout = scan.recoverable[0]
    assert rollout.session_id == duplicate_id
    assert rollout.cwd == "/repo"
    assert rollout.rollout_path == newer
    assert rollout.mtime == pytest.approx(newest_mtime)
    assert rollout.rollout_path != older


def test_recovery_scan_subtracts_only_canonical_codex_registry_ids(tmp_path):
    from fno.agents.registry import AgentEntry, write_registry

    codex = tmp_path / "codex"
    registered = _recovery_session_id(4)
    same_id_on_claude = _recovery_session_id(5)
    _write_codex_rollout(codex, session_id=registered, cwd="/repo")
    _write_codex_rollout(codex, session_id=same_id_on_claude, cwd="/repo")
    registry = tmp_path / "registry.json"
    write_registry(
        [
            AgentEntry(
                name="registered-codex",
                harness="codex",
                harness_session_id=registered,
                cwd="/repo",
                log_path="/tmp/codex.log",
            ),
            AgentEntry(
                name="registered-claude",
                harness="claude",
                harness_session_id=same_id_on_claude,
                cwd="/repo",
                log_path="/tmp/claude.log",
            ),
        ],
        path=registry,
    )

    scan = _scan_recoverable(codex, registry)

    assert scan.complete is True
    assert [row.session_id for row in scan.recoverable] == [same_id_on_claude]


def test_recovery_scan_marks_malformed_and_non_string_cwd_incomplete(tmp_path):
    codex = tmp_path / "codex"
    malformed = _write_codex_rollout(
        codex, session_id=_recovery_session_id(6), cwd="/repo"
    )
    malformed.write_text("not-json\n", encoding="utf-8")
    non_string = _write_codex_rollout(
        codex, session_id=_recovery_session_id(7), cwd=["/repo"]
    )

    scan = _scan_recoverable(codex, tmp_path / "missing-registry.json")

    assert scan.complete is False
    assert scan.recoverable == ()
    assert scan.scanned_count == 2
    assert scan.malformed_count == 2
    assert scan.unreadable_count == 0
    assert any(str(malformed) in failure and "JSON" in failure for failure in scan.failures)
    assert any(str(non_string) in failure and "cwd" in failure for failure in scan.failures)


def test_recovery_scan_marks_rollout_read_failure_incomplete(tmp_path, monkeypatch):
    codex = tmp_path / "codex"
    rollout = _write_codex_rollout(
        codex, session_id=_recovery_session_id(9), cwd="/repo"
    )
    original_open = builtins.open

    def fail_rollout_open(path, *args, **kwargs):
        if Path(path) == rollout:
            raise OSError("read denied")
        return original_open(path, *args, **kwargs)

    monkeypatch.setattr(builtins, "open", fail_rollout_open)

    scan = _scan_recoverable(codex, tmp_path / "missing-registry.json")

    assert scan.complete is False
    assert scan.scanned_count == 1
    assert scan.unreadable_count == 1
    assert any(str(rollout) in failure and "read" in failure for failure in scan.failures)


def test_recovery_scan_marks_registry_schema_failure_incomplete(tmp_path):
    codex = tmp_path / "codex"
    candidate_id = _recovery_session_id(10)
    _write_codex_rollout(codex, session_id=candidate_id, cwd="/repo")
    registry = tmp_path / "registry.json"
    registry.write_text('{"schema_version":"wrong","agents":[]}', encoding="utf-8")

    scan = _scan_recoverable(codex, registry)

    assert scan.complete is False
    assert [row.session_id for row in scan.recoverable] == [candidate_id]
    assert any(str(registry) in failure and "registry" in failure for failure in scan.failures)


def test_recovery_scan_keeps_shared_prefix_full_ids_distinct_and_usable(tmp_path):
    codex = tmp_path / "codex"
    session_ids = [_recovery_session_id(index) for index in (11, 12, 13)]
    for session_id in session_ids:
        _write_codex_rollout(codex, session_id=session_id, cwd="/repo")

    scan = _scan_recoverable(codex, tmp_path / "missing-registry.json")

    assert scan.complete is True
    assert [row.session_id for row in scan.recoverable] == session_ids[::-1]
    assert [row.session_id for row in scan.usable_recoverable] == session_ids[::-1]
    assert scan.usable_recoverable_count == 3
    assert all(row.transcript_usable is True for row in scan.recoverable)
    assert all(
        row.last_event_at == "2026-08-25T15:00:00Z"
        for row in scan.recoverable
    )
    assert all(row.last_turn_marker.startswith("resume ") for row in scan.recoverable)


def test_recovery_scan_rejects_short_session_identity(tmp_path):
    codex = tmp_path / "codex"
    rollout = _write_codex_rollout(
        codex,
        session_id="01a039aa",
        cwd="/repo",
    )

    scan = _scan_recoverable(codex, tmp_path / "missing-registry.json")

    assert scan.complete is False
    assert scan.recoverable == ()
    assert scan.usable_recoverable == ()
    assert any(str(rollout) in failure and "full UUID" in failure for failure in scan.failures)


def _run_codex(tmp_path, codex_dir, **kw):
    """Discover with empty claude stores + fake psutil so only codex rows surface."""
    kwargs = dict(
        sessions_dir=tmp_path / "no-sessions",
        projects_dir=tmp_path / "no-projects",
        codex_sessions_dir=codex_dir,
        name_map_path=tmp_path / ".fno" / "session-names.json",
        registry_path=tmp_path / "missing-registry.json",
        psutil_mod=_FakePsutil(alive={}),
        project_resolver=kw.pop("project_resolver", lambda c: None),
        **kw,
    )
    with patch.object(discover, "_discover_from_codex_daemon", return_value=[]):
        return discover.discover_live_sessions(**kwargs)


def test_us2_codex_old_watching_rollout_is_not_bulk_enumerated(tmp_path):
    codex = tmp_path / "codex"
    session_id = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    rollout = _write_codex_rollout(
        codex,
        session_id=session_id,
        cwd="/x",
        mtime_age=10_000.0,
    )
    with rollout.open("a", encoding="utf-8") as fh:
        fh.write(
            json.dumps(
                {
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "<watching pr=7>"}],
                    },
                }
            )
            + "\n"
        )
    stale = time.time() - 10_000.0
    os.utime(rollout, (stale, stale))

    assert _run_codex(tmp_path, codex) == []

    match, suggestions = discover.resolve_or_suggest(
        session_id,
        sessions_dir=tmp_path / "no-sessions",
        projects_dir=tmp_path / "no-projects",
        codex_sessions_dir=codex,
        name_map_path=tmp_path / ".fno" / "session-names.json",
        psutil_mod=_FakePsutil(alive={}),
        require_alive=False,
    )

    assert suggestions == []
    assert match is not None
    assert match.session_id == session_id
    from fno.agents.session_truth import resolve_session_truth

    truth = resolve_session_truth(
        session_id,
        resolve=lambda _handle: (match, []),
        codex_sessions_dir=codex,
    )
    # The <watching> tag is 10,000s old: expiry (x-c1a3) retires the news to
    # stalled. Discovery still resolves it (require_alive=False), it is just
    # no longer warm.
    assert truth["state"] == "stalled"


def test_resolver_only_full_claude_id_does_not_expand_glob_tokens(tmp_path):
    projects = tmp_path / "projects"
    _write_transcript(
        projects,
        cwd="/repo/one",
        session_id="c655c326-1111-2222-3333-444455556666",
    )

    match, suggestions = discover.resolve_or_suggest(
        "????????-????-????-????-????????????",
        registry_path=tmp_path / "missing-registry.json",
        projects_dir=projects,
        require_alive=False,
    )

    assert match is None
    assert suggestions == []


def test_resolver_only_short_collision_checks_every_store(tmp_path):
    from fno.agents.registry import AgentEntry, write_registry

    registry = tmp_path / "registry.json"
    write_registry(
        [
            AgentEntry(
                name="registered-codex",
                harness="codex",
                harness_session_id="deadbeef-registry",
                cwd="/registry",
                log_path="/tmp/codex.log",
            )
        ],
        path=registry,
    )
    codex = tmp_path / "codex"
    _write_codex_rollout(codex, session_id="deadbeef-transcript", cwd="/transcript")

    match, suggestions = discover.resolve_or_suggest(
        "deadbeef",
        registry_path=registry,
        sessions_dir=tmp_path / "no-sessions",
        projects_dir=tmp_path / "no-projects",
        codex_sessions_dir=codex,
        name_map_path=tmp_path / ".fno" / "session-names.json",
        psutil_mod=_FakePsutil(alive={}),
        project_resolver=lambda _cwd: None,
        require_alive=False,
    )

    assert match is None
    assert suggestions == ["deadbeef-registry", "deadbeef-transcript"]


def test_a_registry_projection_keeps_its_falsifier():
    """Projecting a registry row into this lane must not launder its evidence.

    ``_discover_from_registry`` hardcodes ``pid=0`` and ``status=None``, so an
    exited or pid-dead worker arrived here carrying nothing to falsify with and
    classified reachable off a still-warm transcript. Mail then assigned the
    durable copy to `live-drain` instead of `wake-daemon`, stranding it.

    The falsifier is derived ONCE, by ``registry_falsifier`` at projection time
    (which already knows about panes, exit tombstones and pids), and carried -
    rather than this lane growing a second copy of those rules.
    """
    condemned = discover.DiscoveredSession(
        session_id="feedface",
        short_id="feedface",
        handle="feedface",
        pid=0,  # the projection's "not recorded" placeholder
        cwd="/tmp",
        project=None,
        status=None,
        truth_state="working",
        registry_falsifier="exit-recorded",
    )
    assert condemned.is_reachable is False
    row = condemned.to_row()
    assert row["status"] == "orphaned"
    assert row["basis"] == "exit-recorded"

    # Still an addressing filter: the handle must keep resolving.
    assert condemned.is_alive is True


def test_reachability_reaches_the_decision_not_only_the_rendered_row():
    """The verdict has to reach whatever DECIDES, not only what prints.

    Applying the falsifier in ``to_row`` alone left every caller that asks the
    question rather than printing the answer on the old behaviour -- mail read
    a transcript-only property to set `recipient_live`, so a provably dead
    session was classed live and its durable fallback went to `live-drain`
    instead of `wake-daemon`, stranding the message.

    The two properties stay SPLIT on purpose. `is_alive` backs address
    resolution, which must keep resolving a handle whose recorded pid went
    stale -- an unaddressable row cannot even be resumed by hand. Only
    `is_reachable` carries the falsifiers.
    """
    dead = discover.DiscoveredSession(
        session_id="feedface",
        short_id="feedface",
        handle="feedface",
        pid=1,  # never a worker: `_pid_alive` returns a confident False
        cwd="/tmp",
        project=None,
        status="busy",
        truth_state="working",
    )
    assert dead.is_reachable is False
    assert dead.is_alive is True, "addressing must not lose the handle"

    # A row with no recorded pid stays reachable on both: absence of a pid is
    # absence of evidence, never a death sentence.
    unknown_pid = discover.DiscoveredSession(
        session_id="feedface",
        short_id="feedface",
        handle="feedface",
        pid=0,
        cwd="/tmp",
        project=None,
        status="busy",
        truth_state="working",
    )
    assert unknown_pid.is_reachable is True
    assert unknown_pid.is_alive is True


def test_a_discovered_row_carries_its_evidence_not_just_a_word():
    """Every row on this lane is DERIVED, so the basis matters most here.

    Reducing the verdict to a bare ``status`` left a reader unable to tell a
    transcript-derived ``live`` from an ``orphaned`` produced by a fired
    falsifier -- and the age was hardcoded to None, which threw away the one
    number that makes ``unknown`` actionable. The Rust list path re-serves these
    rows verbatim, so the omission reached both runtimes.
    """
    live = discover.DiscoveredSession(
        session_id="feedface",
        short_id="feedface",
        handle="feedface",
        pid=0,
        cwd="/tmp",
        project=None,
        status="busy",
        truth_state="working",
        last_activity_age_s=32,
    ).to_row()
    assert live["reachability"] == "reachable"
    assert live["basis"] == "transcript"
    assert live["last_activity_age_s"] == 32

    condemned = discover.DiscoveredSession(
        session_id="feedface",
        short_id="feedface",
        handle="feedface",
        pid=1,
        cwd="/tmp",
        project=None,
        status="busy",
        truth_state="working",
        last_activity_age_s=32,
    ).to_row()
    # Same word on the wire as a quiet row would once have produced; only the
    # basis says a falsifier actually fired.
    assert condemned["status"] == "orphaned"
    assert condemned["reachability"] == "unreachable"
    assert condemned["basis"] == "process-gone"


def test_loaded_daemon_thread_does_not_override_stalled_transcript(tmp_path, monkeypatch):
    codex = tmp_path / "codex"
    sid = "019f4d0c-1111-2222-3333-444444444444"
    rollout = _write_codex_rollout(codex, session_id=sid, cwd="/old/repo", mtime_age=10_000.0)
    with rollout.open("a", encoding="utf-8") as fh:
        fh.write(
            json.dumps(
                {
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "still working"}],
                    },
                }
            )
            + "\n"
        )
    stale = time.time() - 10_000.0
    os.utime(rollout, (stale, stale))
    monkeypatch.setattr(
        discover,
        "_discover_from_codex_daemon",
        lambda: [
            {
                "session_id": sid,
                "short_id": sid[:8],
                "pid": 0,
                "cwd": "/live/repo",
                "status": None,
                "agent": "codex",
            }
        ],
    )

    resolved, _ = discover.resolve_or_suggest(
        "019f4d0c",
        sessions_dir=tmp_path / "no-sessions",
        projects_dir=tmp_path / "no-projects",
        codex_sessions_dir=codex,
        name_map_path=tmp_path / ".fno" / "session-names.json",
        psutil_mod=_FakePsutil(alive={}),
        project_resolver=lambda c: None,
    )

    assert resolved is None


def test_daemon_probe_shapes_valid_rows_and_skips_bad_entries(monkeypatch):
    from fno import rust_binary

    output = {
        "codex_loaded": {
            "available": True,
            "threads": [
                {"session_id": "short", "cwd": None},
                {"session_id": "short", "cwd": "/duplicate"},
                {"session_id": "019f4d0c-full", "cwd": "/repo"},
                {"session_id": 7, "cwd": "/bad"},
            ],
        }
    }
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: Path("/fake/fno-agents"))
    monkeypatch.setattr(
        discover.subprocess,
        "run",
        lambda *a, **kw: subprocess.CompletedProcess([], 0, stdout=json.dumps(output), stderr=""),
    )

    rows = discover._discover_from_codex_daemon()

    assert [(r["session_id"], r["short_id"], r["cwd"]) for r in rows] == [
        ("short", "short", ""),
        ("019f4d0c-full", "019f4d0c", "/repo"),
    ]


def test_daemon_probe_argv_rides_list_not_a_hidden_verb(monkeypatch):
    from fno import rust_binary

    seen: dict = {}

    def fake_run(argv, **kw):
        seen["argv"] = argv
        return subprocess.CompletedProcess([], 0, stdout="{}", stderr="")

    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: Path("/fake/fno-agents"))
    monkeypatch.setattr(discover.subprocess, "run", fake_run)

    assert discover._codex_daemon_threads_raw() is None
    assert seen["argv"] == [
        "/fake/fno-agents",
        "list",
        "--json",
        "--no-discovered",
        "--harness",
        "codex",
    ]


def test_us3_resolve_bare_short_id_across_harnesses(tmp_path):
    """A codex session answers to the same bare short-id a claude one does - the
    address does not encode the harness. The retired prefixed form is refused,
    and the refusal leads with the bare id so the caller can fix what it built."""
    codex = tmp_path / "codex"
    _write_codex_rollout(
        codex,
        session_id="019f48e1-5b09-72a0-9bc8-6b364bcf4ae4",
        cwd="/x",
    )
    seams = dict(
        sessions_dir=tmp_path / "no-sessions",
        projects_dir=tmp_path / "no-projects",
        codex_sessions_dir=codex,
        name_map_path=tmp_path / ".fno" / "session-names.json",
        psutil_mod=_FakePsutil(alive={}),
        project_resolver=lambda c: None,
        truth_fn=lambda _session: {"state": "working"},
    )
    resolved, _ = discover.resolve_or_suggest("019f48e1", **seams)
    assert resolved is not None
    assert resolved.agent == "codex"
    assert resolved.session_id == "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"

    refused, suggestions = discover.resolve_or_suggest("codex-019f48e1", **seams)
    assert refused is None
    assert suggestions[0] == "019f48e1"


def test_retired_shape_refused_even_when_stored_as_friendly_alias(tmp_path):
    sid = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    codex = tmp_path / "codex"
    _write_codex_rollout(codex, session_id=sid, cwd="/x")
    name_map = tmp_path / ".fno" / "session-names.json"
    name_map.parent.mkdir(parents=True)
    name_map.write_text(json.dumps({sid: "codex-019f48e1"}), encoding="utf-8")

    resolved, suggestions = discover.resolve_or_suggest(
        "codex-019f48e1",
        sessions_dir=tmp_path / "no-sessions",
        projects_dir=tmp_path / "no-projects",
        codex_sessions_dir=codex,
        name_map_path=name_map,
        psutil_mod=_FakePsutil(alive={}),
        project_resolver=lambda c: None,
    )

    assert resolved is None
    assert suggestions[0] == "019f48e1"
    assert json.loads(name_map.read_text(encoding="utf-8"))[sid] == "session-019f48e1"


# --------------------------------------------------------------------------
# x-605c: daemon-roster (US1) + fno-agents registry (US2) resolution sources
# --------------------------------------------------------------------------


def _write_roster(daemon_dir: Path, workers: dict[str, dict]) -> None:
    daemon_dir.mkdir(parents=True, exist_ok=True)
    (daemon_dir / "roster.json").write_text(
        json.dumps({"proto": 1, "workers": workers}), encoding="utf-8"
    )


def _empty_seams(tmp_path: Path) -> dict:
    """discover seams that make every disk source contribute zero rows, so a
    test isolates the roster/registry sources under test."""
    return dict(
        sessions_dir=tmp_path / "no-sessions",
        projects_dir=tmp_path / "no-projects",
        codex_sessions_dir=tmp_path / "no-codex",
        opencode_storage_dir=tmp_path / "no-opencode",
        name_map_path=tmp_path / ".fno" / "session-names.json",
        psutil_mod=_FakePsutil({}),
        project_resolver=lambda c: None,
        truth_fn=lambda _session: {"state": "working"},
    )


def test_us1_roster_source_resolves_bg_worker(tmp_path, monkeypatch):
    """US1/AC1-HP: a rostered bg worker (no pid-sidecar) resolves by handle."""
    use_tmpdir(monkeypatch, tmp_path)
    daemon = tmp_path / "daemon"
    _write_roster(
        daemon,
        {
            "9a063cd3": {
                "sessionId": "9a063cd3-69d4-415a-ada5-649b0164189c",
                "pid": 4242,
                "cwd": "/Users/x/code/proj",
            }
        },
    )
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(daemon))
    resolved, _ = discover.resolve_or_suggest("9a063cd3", **_empty_seams(tmp_path))
    assert resolved is not None
    assert resolved.agent == "claude"
    assert resolved.short_id == "9a063cd3"
    assert resolved.session_id == "9a063cd3-69d4-415a-ada5-649b0164189c"


def test_identity_only_sweep_skips_falsifier_and_project(tmp_path, monkeypatch):
    """The address sweep reads identity alone: the falsifier spawns a holder
    probe per claude row and a project resolve parses every settings file."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, write_registry

    reg = tmp_path / "registry.json"
    write_registry(
        [
            AgentEntry(
                name="x-foo",
                harness="claude",
                cwd="/Users/x/code/proj",
                log_path="/tmp/x-foo.log",
                short_id="9a063cd3",
                harness_session_id="9a063cd3-69d4-415a-ada5-649b0164189c",
            )
        ],
        path=reg,
    )
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(tmp_path / "no-daemon"))
    calls: list[str] = []
    monkeypatch.setattr(discover, "registry_falsifier", lambda e: calls.append("falsify"))
    seams = _empty_seams(tmp_path)
    seams["project_resolver"] = lambda cwd: calls.append("project")
    sessions = discover.discover_live_sessions(
        registry_path=reg, classify_truth=False, identity_only=True, **seams
    )
    assert [s.name for s in sessions] == ["x-foo"]
    assert sessions[0].project is None
    assert calls == []
    discover.discover_live_sessions(registry_path=reg, classify_truth=False, **seams)
    assert sorted(calls) == ["falsify", "project"]


def test_repaired_codex_identity_resolves_by_name_short_and_full_id(tmp_path, monkeypatch):
    """Every public handle reaches one peer, GIVEN an already-repaired row.

    Scope note: this seeds the repaired state and performs no repair, so it pins
    resolver behaviour only and does NOT prove AC2-CON. The end-to-end repair
    claim belongs to test_late_codex_identity_composes_across_every_peer_surface
    in test_spawn_pane.py, which reconciles for real and then joins the surfaces.
    """
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, write_registry

    session_id = "019fb024-2327-75f3-8b80-06e9d5ade05f"
    requested_name = "repro-late-id"
    registry = tmp_path / "registry.json"
    write_registry(
        [
            AgentEntry(
                name=requested_name,
                harness="codex",
                harness_session_id=session_id,
                status="live",
                cwd="/repo",
                log_path="",
                pid=4242,
                mux={"session": "main", "pane_id": 7},
            )
        ],
        path=registry,
    )

    resolved = []
    for handle in (requested_name, session_id[-8:], session_id[:8], session_id):
        match, suggestions = discover.resolve_or_suggest(
            handle,
            registry_path=registry,
            require_alive=False,
            **_empty_seams(tmp_path),
        )
        assert suggestions == []
        assert match is not None
        resolved.append(match)

    assert {peer.session_id for peer in resolved} == {session_id}
    assert {peer.short_id for peer in resolved} == {session_id[:8]}
    assert {peer.name for peer in resolved} == {requested_name}


def test_us2_registered_name_and_colliding_alias_are_ambiguous(monkeypatch):
    """A registered name cannot silently displace another session's alias."""
    named = discover.DiscoveredSession(
        session_id="sid-named",
        short_id="aa111111",
        handle="aa111111",
        pid=0,
        cwd="/x",
        project=None,
        status=None,
        agent="codex",
        truth_state="working",
        name="dup-name",
    )
    aliased = discover.DiscoveredSession(
        session_id="sid-alias",
        short_id="bb222222",
        handle="dup-name",
        pid=0,
        cwd="/x",
        project=None,
        status=None,
        agent="claude",
        truth_state="working",
    )
    monkeypatch.setattr(discover, "discover_live_sessions", lambda **_k: [named, aliased])
    match, suggestions = discover.resolve_or_suggest("dup-name")
    assert match is None
    assert sorted(suggestions) == ["sid-alias", "sid-named"]


def test_ac1_edge_source_overlap_dedups(tmp_path, monkeypatch):
    """AC1-EDGE: one session present in registry AND roster yields exactly one row."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, write_registry

    sid = "9a063cd3-69d4-415a-ada5-649b0164189c"
    daemon = tmp_path / "daemon"
    _write_roster(daemon, {"9a063cd3": {"sessionId": sid, "pid": 4242, "cwd": "/x"}})
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(daemon))
    reg = tmp_path / "registry.json"
    write_registry(
        [
            AgentEntry(
                name="x-foo",
                harness="claude",
                cwd="/x",
                log_path="/tmp/x.log",
                short_id="9a063cd3",
                harness_session_id=sid,
            )
        ],
        path=reg,
    )
    sessions = discover.discover_live_sessions(registry_path=reg, **_empty_seams(tmp_path))
    assert [s.session_id for s in sessions] == [sid]


def test_opencode_row_without_captured_id_yields_no_live_recipient(tmp_path, monkeypatch):
    """AC3-EDGE: an id-less opencode row must not widen discovery.

    The roster gate admits opencode rows, so an id-less one (backfill
    missed, or a pane spawned before capture existed) reaches the `if not sid`
    guard instead of the harness guard. It must still contribute nothing, or
    mail would queue to a handle nobody drains.
    """
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, write_registry

    reg = tmp_path / "registry.json"
    write_registry(
        [
            AgentEntry(
                name="oc-live-only",
                harness="opencode",
                cwd="/x",
                log_path="/tmp/oc.log",
                harness_session_id=None,
            )
        ],
        path=reg,
    )
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(tmp_path / "no-daemon"))
    assert discover.discover_live_sessions(registry_path=reg, **_empty_seams(tmp_path)) == []


def test_registry_orphaned_status_cannot_hide_family1_live_session(tmp_path, monkeypatch):
    """A stale registry verdict yields to transcript truth on every read caller."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, write_registry

    sid = "feedface-1111-2222-3333-444444444444"
    reg = tmp_path / "registry.json"
    write_registry(
        [
            AgentEntry(
                name="x-live",
                harness="claude",
                cwd="/x",
                log_path="/tmp/live.log",
                short_id="feedface",
                harness_session_id=sid,
                status="orphaned",
            )
        ],
        path=reg,
    )
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(tmp_path / "no-daemon"))

    resolved, _ = discover.resolve_or_suggest(
        "feedface",
        registry_path=reg,
        **_empty_seams(tmp_path),
    )

    assert resolved is not None
    assert resolved.truth_state == "working"


# --------------------------------------------------------------------------
# US6 — opencode disk discovery
# --------------------------------------------------------------------------


def _write_opencode_session(
    storage: Path,
    *,
    session_id: str,
    cwd: str,
    mtime_age: float,
    project_id: str = "proj0001",
    messages: int = 1,
    info: bool = True,
) -> Path:
    """Build a storage tree matching a real opencode install (1.0.223).

    Layout verified on disk: session info at ``session/<projectID>/<ses>.json``
    with the cwd under ``directory``; messages at ``message/<ses>/<msg>.json``.
    """
    sdir = storage / "session" / project_id
    sdir.mkdir(parents=True, exist_ok=True)
    f = sdir / f"{session_id}.json"
    body = (
        {"id": session_id, "directory": cwd, "time": {"created": 1, "updated": 2}}
        if info
        else {"no": "id"}
    )
    f.write_text(json.dumps(body), encoding="utf-8")
    mt = time.time() - mtime_age
    mdir = storage / "message" / session_id
    if messages:
        mdir.mkdir(parents=True, exist_ok=True)
        for i in range(messages):
            m = mdir / f"msg_{i}.json"
            m.write_text(
                json.dumps({"id": f"msg_{i}", "role": "user", "time": {"created": i}}),
                encoding="utf-8",
            )
            os.utime(m, (mt, mt))
        os.utime(mdir, (mt, mt))
    os.utime(f, (mt, mt))
    return f


def _run_opencode(tmp_path, storage, **kw):
    """Discover with every other source empty so only opencode rows surface."""
    with patch.object(discover, "_discover_from_roster", return_value=[]):
        return discover.discover_live_sessions(
            sessions_dir=tmp_path / "no-sessions",
            projects_dir=tmp_path / "no-projects",
            codex_sessions_dir=tmp_path / "no-codex",
            opencode_storage_dir=storage,
            name_map_path=tmp_path / ".fno" / "session-names.json",
            registry_path=tmp_path / "no-registry.json",
            psutil_mod=_FakePsutil(alive={}),
            project_resolver=kw.pop("project_resolver", lambda c: None),
            **kw,
        )


def _touch(path: Path, age: float) -> None:
    mt = time.time() - age
    os.utime(path, (mt, mt))


def test_us6_opencode_streaming_turn_stays_live_via_part_mtime(tmp_path):
    """A long turn writes into part/<msg_id>/, which moves neither the session
    info nor the message dir. Without the deeper look such a session ages out
    of discovery and becomes unaddressable while still alive."""
    storage = tmp_path / "opencode"
    sid = "ses_streaming"
    info = _write_opencode_session(storage, session_id=sid, cwd="/x", mtime_age=1800.0, messages=1)
    # Both cheap signals are stale (well past the 600s window)...
    assert info.stat().st_mtime < time.time() - 600
    assert (storage / "message" / sid).stat().st_mtime < time.time() - 600
    # ...but the newest message's parts are being written right now.
    pdir = storage / "part" / "msg_0"
    pdir.mkdir(parents=True)
    (pdir / "prt_000.json").write_text('{"type":"text","text":"streaming"}', encoding="utf-8")
    _touch(pdir, 5.0)
    assert [s.session_id for s in _run_opencode(tmp_path, storage)] == [sid]


# --------------------------------------------------------------------------
# opencode SQLite store (current opencode; the JSON tree above is legacy)
# --------------------------------------------------------------------------


def _write_opencode_db(storage: Path, sessions, messages=(), parts=()) -> Path:
    """Build an opencode.db matching the real 1.14.50 schema.

    Timestamps are milliseconds, as opencode stores them.
    """
    import sqlite3

    storage.mkdir(parents=True, exist_ok=True)
    db = storage.parent / "opencode.db"
    con = sqlite3.connect(db)
    con.execute(
        "CREATE TABLE session (id TEXT, directory TEXT, time_created INTEGER, time_updated INTEGER)"
    )
    con.execute("CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, data TEXT)")
    con.execute(
        "CREATE TABLE part (id TEXT, message_id TEXT, session_id TEXT,"
        " time_created INTEGER, data TEXT)"
    )
    for sid, directory, age in sessions:
        updated = int((time.time() - age) * 1000)
        con.execute("INSERT INTO session VALUES (?,?,?,?)", (sid, directory, updated, updated))
    for mid, sid, created, data in messages:
        con.execute("INSERT INTO message VALUES (?,?,?,?)", (mid, sid, created, json.dumps(data)))
    for pid, mid, created, data in parts:
        con.execute(
            "INSERT INTO part VALUES (?,?,?,?,?)",
            (pid, mid, "", created, json.dumps(data)),
        )
    con.commit()
    con.close()
    return db


def test_opencode_db_surfaces_live_session(tmp_path):
    """The SQLite store is where current opencode writes; time_updated is an
    explicit activity timestamp, so no mtime inference is involved."""
    storage = tmp_path / "opencode" / "storage"
    _write_opencode_db(
        storage,
        [("ses_live", "/Users/x/proj", 5.0), ("ses_stale", "/Users/x/old", 10_000.0)],
    )
    sessions = _run_opencode(tmp_path, storage)
    assert [(s.session_id, s.cwd, s.agent) for s in sessions] == [
        ("ses_live", "/Users/x/proj", "opencode"),
        ("ses_stale", "/Users/x/old", "opencode"),
    ]
    assert [s.truth_state for s in sessions] == ["unknown", "unknown"]


def test_opencode_empty_db_does_not_resurrect_legacy_sessions(tmp_path):
    """A database that exists but yields nothing means "nothing is live", NOT
    "no database". Falling back here would surface the legacy tree's long-dead
    sessions as live."""
    storage = tmp_path / "opencode" / "storage"
    _write_opencode_session(storage, session_id="ses_legacy", cwd="/legacy", mtime_age=5.0)
    _write_opencode_db(storage, [])  # real store, no live sessions
    assert _run_opencode(tmp_path, storage) == []


def test_opencode_broken_db_does_not_resurrect_legacy_sessions(tmp_path):
    """Same for a locked/corrupt/schema-drifted store: an error reads as empty,
    and must not be mistaken for "this host has no database"."""
    storage = tmp_path / "opencode" / "storage"
    _write_opencode_session(storage, session_id="ses_legacy", cwd="/legacy", mtime_age=5.0)
    storage.mkdir(parents=True, exist_ok=True)
    (storage.parent / "opencode.db").write_text("not a database", encoding="utf-8")
    assert _run_opencode(tmp_path, storage) == []


# ---------------------------------------------------------------------------
# resolve_reachable (x-e864): the liveness-blind rung below discovery.
# Each source gets its own test because a source that silently never fires is
# indistinguishable from one that fired and missed -- the exact bug class this
# node exists to kill. Two of these caught real defects: the roster reader fell
# back to Path('') (which is Path('.'), not falsy, so it read ./roster.json),
# and the graph reader imported a module path that does not exist.
# ---------------------------------------------------------------------------


def _stale(path):
    old = time.time() - 7200
    os.utime(path, (old, old))


def test_resolve_reachable_reads_the_roster_without_an_env_override(tmp_path, monkeypatch):
    """The roster source must resolve its own default dir and carry the cwd.

    Regression pin: the fallback used to be ``Path(os.environ.get(k, ""))``,
    and ``Path("")`` is ``Path(".")`` -- truthy -- so with the env var unset the
    reader looked for ./roster.json and this source never fired in production.
    Claude resume is cwd-scoped, so the roster cwd must ride along for a wake.
    """
    from fno.agents import discover

    sid = "aa11bb22-3344-5566-7788-99aabbccddee"
    monkeypatch.delenv("FNO_CLAUDE_DAEMON_DIR", raising=False)
    home = tmp_path / "home"
    daemon = home / ".claude" / "daemon"
    daemon.mkdir(parents=True)
    (daemon / "roster.json").write_text(
        json.dumps(
            {
                "proto": 1,
                "workers": {sid[:8]: {"sessionId": sid, "pid": 1, "cwd": "/Users/x/other-repo"}},
            }
        ),
        encoding="utf-8",
    )
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))

    empty_projects = tmp_path / "no-projects"
    empty_projects.mkdir()
    found, _ = discover.resolve_reachable(sid[:8], projects_dir=empty_projects)

    assert found is not None, "the roster source did not fire on its default dir"
    assert found.session_id == sid and found.source == "roster"
    assert found.cwd == "/Users/x/other-repo"


def test_resolve_reachable_reads_backlog_session_stamps(tmp_path, monkeypatch):
    """The graph source must actually import and parse.

    Regression pin: it imported ``fno.graph.io`` (no such module) and indexed
    the result as a dict-with-nodes, while ``load_graph`` lives in
    ``fno.graph.load`` and returns a flat ``list[dict]``. Both failures were
    swallowed, so the source returned [] forever.
    """
    from fno.agents import discover

    sid = "ccdd1122-3344-5566-7788-99aabbccddee"
    graph = tmp_path / "graph.json"
    seed_graph(graph, json.dumps(
            {
                "entries": [
                    {
                        "id": "x-0001",
                        "sessions": [{"phase": "ship", "harness": "claude", "session_id": sid}],
                    }
                ]
            }
        ))
    monkeypatch.setattr("fno.graph.load.GRAPH_JSON", graph)

    empty_projects = tmp_path / "no-projects"
    empty_projects.mkdir()
    found, _ = discover.resolve_reachable(sid[:8], projects_dir=empty_projects)

    assert found is not None, "the graph source did not fire"
    assert found.session_id == sid and found.source == "graph"


def test_resolve_reachable_ambiguity_names_the_source_of_each_candidate(tmp_path, monkeypatch, capsys):
    """The ambiguity refusal must attribute each candidate to its store.

    The 2026-09-05 refusal named two uuids for one token while 0 of the
    registry's rows carried the id, and nothing said which stores disagreed
    (x-a469). A transcript hit and a registry hit sharing one 8-hex token
    reproduce the shape; the stderr line must name source=transcript and
    source=registry.
    """
    from fno.agents import discover

    a = "c0ffee11-1111-2222-3333-444444444444"
    b = "c0ffee11-9999-8888-7777-666666666666"

    proj = tmp_path / "projects" / "-Users-x-a"
    proj.mkdir(parents=True)
    t = proj / f"{a}.jsonl"
    t.write_text("{}\n", encoding="utf-8")
    _stale(t)

    class _Row:
        harness = "claude"
        harness_session_id = b
        short_id = "c0ffee11"

    monkeypatch.setattr("fno.agents.registry.load_registry", lambda *_a, **_k: [_Row()])

    found, ambiguous = discover.resolve_reachable(
        "c0ffee11", projects_dir=tmp_path / "projects", registry_path=tmp_path / "registry.json"
    )

    assert found is None
    assert sorted(ambiguous) == sorted([a, b])
    err = capsys.readouterr().err
    assert f"{a} (source=transcript" in err
    assert f"{b} (source=registry" in err


def test_unreadable_store_raises_instead_of_reporting_absence(tmp_path, monkeypatch):
    """The mail-loss guard: a store that ERRORS must not read as 'not found'.

    The caller turns a clean miss into exit 16 and queues nothing, so absence
    has to be proven rather than assumed. A torn registry is the realistic
    trigger: it raises, and reporting that as "no rows" would drop mail for a
    session the registry actually knows about.
    """
    from fno.agents import discover
    from fno.agents.registry import RegistryVersionError

    projects = tmp_path / "projects"
    projects.mkdir()
    daemon = tmp_path / "daemon"
    daemon.mkdir()
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(daemon))

    def _torn(*_a, **_k):
        raise RegistryVersionError("schema drift")

    monkeypatch.setattr("fno.agents.registry.load_registry", _torn)

    with pytest.raises(discover.StoreReadError) as err:
        discover.resolve_reachable("deadbeef", projects_dir=projects)
    assert "registry" in err.value.failed


def test_an_absent_store_is_a_clean_answer_not_a_read_failure(tmp_path, monkeypatch):
    """An absent transcript store means no claude session ever ran here.

    That is definitive, so it must stay a clean miss. Classifying it as
    unreadable would make every typo queue durably on a host that has never
    run claude, stranding envelopes and destroying the exit-16 typo guard.
    """
    from fno.agents import discover

    daemon = tmp_path / "daemon"
    daemon.mkdir()
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(daemon))

    found, ambiguous = discover.resolve_reachable(
        "deadbeef",
        projects_dir=tmp_path / "never-created",
        registry_path=tmp_path / "registry.json",
    )

    assert found is None and ambiguous == []


def test_one_hit_plus_a_degraded_store_is_unproven_not_unique(tmp_path, monkeypatch):
    """A lone hit is not proof of uniqueness while a store is unreadable.

    The unreadable store could hold a session colliding on the same short id,
    so waking the one hit would be exactly the guess the never-guess rule
    forbids. The candidate rides along on the error so the caller can still
    address a durable copy to a real session instead of demoting blind.
    """
    from fno.agents import discover

    sid = "aa11bb22-3344-5566-7788-99aabbccddee"
    daemon = tmp_path / "daemon"
    daemon.mkdir()
    (daemon / "roster.json").write_text(
        json.dumps({"proto": 1, "workers": {sid[:8]: {"sessionId": sid, "pid": 1}}}),
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(daemon))

    def _torn(*_a, **_k):
        raise OSError("registry unreadable")

    monkeypatch.setattr("fno.agents.registry.load_registry", _torn)
    projects = tmp_path / "projects"
    projects.mkdir()

    with pytest.raises(discover.StoreReadError) as err:
        discover.resolve_reachable(sid[:8], projects_dir=projects)
    assert "registry" in err.value.failed
    assert err.value.resolved is not None
    assert err.value.resolved.session_id == sid


def test_registry_source_yields_the_resumable_uuid_not_the_job_id(tmp_path, monkeypatch):
    """`AgentEntry.session_id` is harness-polymorphic: for claude it resolves to
    `short_id`, the 8-hex daemon transport key, NOT a resumable uuid. Waking on
    that value would run `claude -r <jobId>` against a session that does not
    exist, so this source must read `harness_session_id` directly.
    """
    from fno.agents import discover

    sid = "bb22cc33-4455-6677-8899-aabbccddeeff"

    class _Row:
        harness = "claude"
        harness_session_id = sid
        short_id = "deadbeef"

        @property
        def session_id(self):  # what the buggy version read
            return self.short_id

    monkeypatch.setattr("fno.agents.registry.load_registry", lambda *_a, **_k: [_Row()])
    projects = tmp_path / "projects"
    projects.mkdir()

    found, _ = discover.resolve_reachable(sid[:8], projects_dir=projects)

    assert found is not None
    assert found.session_id == sid, "resolved the job id instead of the uuid"
    assert found.agent == "claude"


# ---------------------------------------------------------------------------
# Review-round fixes (PR 501): every store consulted before uniqueness, alias
# survival, cwd propagation, case-insensitive tokens, malformed-store guards.
# ---------------------------------------------------------------------------


def test_opencode_ids_keep_their_case_while_hex_folds(tmp_path):
    """Case folding is for hex ids only.

    An opencode id (`ses_...`) is mixed-case by construction, so folding it
    would let two sessions differing only in case collide -- and a wrong
    collision here wakes a stranger's session. Mirrors the normalization rule
    in `agents.store_fallback`; the two must not drift.
    """
    from fno.agents.discover import _token_matches

    # Hex folds: same session, different spelling.
    assert _token_matches("5B17E2F0", "5b17e2f0-1c44-4d9a-8e3b-2f6a7c081d55")
    assert _token_matches("5b17e2f0", "5B17E2F0-1C44-4D9A-8E3B-2F6A7C081D55")

    # opencode does NOT fold: case is meaningful, so these are distinct.
    assert _token_matches("ses_AbC123", "ses_AbC123")
    assert not _token_matches("ses_abc123", "ses_AbC123")


# An id string is unique only WITHIN a harness. The three regression tests below
# pin that on each structure that deduplicates independently: collapsing them
# would resolve a two-session token to one unambiguous hit and wake a stranger's
# session. Fixing one structure alone is decorative -- an upstream source that
# still folds on the raw id drops the second row before the merge ever sees it.
_SHARED_SID = "019fb417-2222-7333-8444-5555cafebabe"


def test_resolve_reachable_keeps_same_id_under_different_harnesses_distinct(tmp_path, monkeypatch):
    """One uuid string under two harnesses is two sessions, never one."""
    from fno.agents import discover

    monkeypatch.setattr(
        discover,
        "_reachable_from_transcripts",
        lambda *_a: (
            [
                (_SHARED_SID, "claude", "/claude-cwd", True, None),
                (_SHARED_SID, "codex", "/codex-cwd", True, None),
            ],
            True,
        ),
    )
    monkeypatch.setattr(discover, "_reachable_from_registry", lambda *_a: ([], True))
    monkeypatch.setattr(discover, "_reachable_from_roster", lambda *_a: ([], True))
    monkeypatch.setattr(discover, "_reachable_from_graph", lambda *_a: ([], True))

    found, ambiguous = discover.resolve_reachable(_SHARED_SID, projects_dir=tmp_path / "projects")

    assert found is None, "a cross-harness id collision must never resolve uniquely"
    assert ambiguous == [_SHARED_SID, _SHARED_SID]


def test_discover_live_sessions_keeps_cross_harness_rows_distinct(tmp_path, monkeypatch):
    """Candidates are the union of every harness's source; the merge must not
    fold two of them into one row that absorbs the other's cwd."""
    from fno.agents.registry import AgentEntry
    from tests._table_seed import seed_legacy_registry

    reg = tmp_path / "registry.json"
    # Two harness rows sharing one session id predate the identity
    # invariant; only a legacy registry file can still hold them.
    seed_legacy_registry(
        [
            AgentEntry(
                name="claude-side",
                harness="claude",
                cwd="/claude-cwd",
                log_path="/tmp/c.log",
                short_id="cafebabe",
                harness_session_id=_SHARED_SID,
            ),
            AgentEntry(
                name="codex-side",
                harness="codex",
                cwd="/codex-cwd",
                log_path="/tmp/x.log",
                harness_session_id=_SHARED_SID,
            ),
        ],
        reg,
    )
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(tmp_path / "no-daemon"))

    sessions = discover.discover_live_sessions(registry_path=reg, **_empty_seams(tmp_path))

    assert sorted((s.agent, s.cwd) for s in sessions) == [
        ("claude", "/claude-cwd"),
        ("codex", "/codex-cwd"),
    ]


def test_resolve_reachable_includes_complete_harness_store_hits(tmp_path, monkeypatch):
    """Codex/OpenCode stores participate below the liveness listing."""
    sid = "019fb417-1111-7222-8333-4444deadbeef"
    monkeypatch.setattr(discover, "_reachable_from_transcripts", lambda *_a: ([], True))
    monkeypatch.setattr(discover, "_reachable_from_registry", lambda *_a: ([], True))
    monkeypatch.setattr(discover, "_reachable_from_roster", lambda *_a: ([], True))
    monkeypatch.setattr(discover, "_reachable_from_graph", lambda *_a: ([], True))
    monkeypatch.setattr(
        discover,
        "_reachable_from_harness_stores",
        lambda _token: ([(sid, "codex", "/repo", True, None)], True),
    )

    found, ambiguous = discover.resolve_reachable("deadbeef", projects_dir=tmp_path / "projects")

    assert ambiguous == []
    assert found is not None
    assert (found.session_id, found.agent, found.cwd) == (sid, "codex", "/repo")


def test_resolve_or_suggest_rechecks_live_short_against_durable_namespace(
    monkeypatch,
):
    """Liveness filtering cannot turn a persisted collision into uniqueness."""
    visible = discover.DiscoveredSession(
        session_id="aaaaaaaa-1111-7222-8333-4444deadbeef",
        short_id="deadbeef",
        handle="deadbeef",
        pid=1,
        cwd="/visible",
        project=None,
        status="live",
        agent="codex",
        truth_state="working",
    )
    hidden = "bbbbbbbb-1111-7222-8333-4444deadbeef"
    monkeypatch.setattr(discover, "discover_live_sessions", lambda **_kwargs: [visible])
    monkeypatch.setattr(
        discover,
        "resolve_reachable",
        lambda *_args, **_kwargs: (None, [visible.session_id, hidden]),
    )

    resolved, ambiguous = discover.resolve_or_suggest("deadbeef")

    assert resolved is None
    assert ambiguous == [visible.session_id, hidden]


def test_resolve_reachable_alias_and_canonical_collision_fails_ambiguous(tmp_path, monkeypatch):
    """A persisted alias cannot silently displace or be displaced by a handle."""
    alias_sid = "11111111-0000-0000-0000-11111111"
    canonical_sid = "22222222-0000-0000-0000-deadbeef"
    project = tmp_path / "projects" / "-tmp-project"
    project.mkdir(parents=True)
    for sid in (alias_sid, canonical_sid):
        (project / f"{sid}.jsonl").write_text("{}\n")
    aliases = tmp_path / "aliases.json"
    aliases.write_text(json.dumps({alias_sid: "deadbeef"}))
    monkeypatch.setattr(discover, "_reachable_from_registry", lambda *_a: ([], True))
    monkeypatch.setattr(discover, "_reachable_from_roster", lambda *_a: ([], True))
    monkeypatch.setattr(discover, "_reachable_from_graph", lambda *_a: ([], True))

    found, ambiguous = discover.resolve_reachable(
        "deadbeef", projects_dir=project.parent, name_map_path=aliases
    )
    assert found is None
    assert sorted(ambiguous) == sorted([alias_sid, canonical_sid])


def test_disambiguate_never_emits_a_duplicate_even_when_every_suffix_is_taken():
    """The uniqueness guarantee has to hold on the exhausted path too.

    Reachable only from a hand-edited map that aliases one session to exactly
    the `<name>-<sid>` another session would escalate to. Falling through would
    emit a duplicate, and a duplicate alias resolves to two holders on the send
    path - the precise failure the guarantee exists to prevent.
    """
    from fno.agents.discover import _disambiguate, canonical_handle

    # The victim must sort LAST, so every rung of its ladder is already in
    # `seen` by the time it is processed. Sorting it first would mean no
    # collision at all and the exhausted path would never run.
    victim = "zzzzzzzz-1111-2222-3333-999999999999"
    holders = [
        "aaaaaaaa-1111-2222-3333-444444444444",
        "bbbbbbbb-1111-2222-3333-555555555555",
        "cccccccc-1111-2222-3333-666666666666",
        "dddddddd-1111-2222-3333-777777777777",
    ]
    live = [{"session_id": s, "short_id": "dup"} for s in [*holders, victim]]
    aliases = {
        holders[0]: "etl-worker",
        holders[1]: "etl-worker-dup",
        holders[2]: f"etl-worker-{canonical_handle(victim)}",
        holders[3]: f"etl-worker-{victim}",
        victim: "etl-worker",
    }

    out = _disambiguate(aliases, live)

    assert len(set(out.values())) == len(out), f"duplicate emitted: {out}"
    assert out[victim] not in [out[s] for s in holders]


def test_same_bucket_default_aliases_heal_once_and_stay_healed(tmp_path, capsys):
    """A shared short_id must not put the alias map in a permanent heal loop.

    Three Codex sessions from one UUIDv7 bucket and two OpenCode sessions
    sharing a head-8 all default to the same alias. The old collision suffix
    appended the very short_id the base already carries, so the stored form
    was ``fno-01a034f3-01a034f3`` - which the next pass classifies as
    accretion damage, discards with a user-visible warning, and regenerates
    into the same shape on every mail send and peek. The second pass against
    the persisted map therefore has to be a complete no-op: same map, same
    bytes on disk, no diagnostic.
    """
    from fno.agents.discover import _is_accreted, _resolve_aliases

    live = [
        {"session_id": "01a034f3-8bad-7e83-92e9-78cfd45881f7",
         "short_id": "01a034f3", "project": "fno"},
        {"session_id": "01a034f3-a58f-71e3-8ed4-bfe77f816168",
         "short_id": "01a034f3", "project": "fno"},
        {"session_id": "01a034f3-c9d0-7e70-ba92-1dae616956f4",
         "short_id": "01a034f3", "project": "fno"},
        {"session_id": "ses_fd2ad214cffeavsk7YFt3qbKDc",
         "short_id": "ses_fd2a"},
        {"session_id": "ses_fd2ad649dffeOX8Yx0RqQEjGg5",
         "short_id": "ses_fd2a"},
    ]
    name_map = tmp_path / ".fno" / "session-names.json"

    first = _resolve_aliases(live, name_map)

    expected = {
        "01a034f3-8bad-7e83-92e9-78cfd45881f7": "fno-01a034f3",
        "01a034f3-a58f-71e3-8ed4-bfe77f816168":
            "fno-01a034f3-a58f-71e3-8ed4-bfe77f816168",
        "01a034f3-c9d0-7e70-ba92-1dae616956f4":
            "fno-01a034f3-c9d0-7e70-ba92-1dae616956f4",
        "ses_fd2ad214cffeavsk7YFt3qbKDc": "session-ses_fd2a",
        "ses_fd2ad649dffeOX8Yx0RqQEjGg5":
            "session-ses_fd2ad649dffeOX8Yx0RqQEjGg5",
    }
    assert first == expected, f"collision aliases not rebuilt from full ids: {first}"
    assert json.loads(name_map.read_text(encoding="utf-8")) == expected
    assert len(set(first.values())) == len(first), "an alias resolves to two holders"
    assert not any(_is_accreted(v) for v in first.values())
    assert capsys.readouterr().err == ""

    second = _resolve_aliases(live, name_map)
    bytes_after_first = name_map.read_text(encoding="utf-8")

    assert second == first
    assert json.loads(bytes_after_first) == second
    assert capsys.readouterr().err == ""


def test_true_accretion_is_still_discarded_after_the_collision_repair(
    tmp_path, capsys
):
    """The repair must not blind the damage detector.

    ``etl-dup-dup`` is genuine historical accretion: a stored alias with a
    repeated tail token-group. It has to keep hitting the discard diagnostic
    and persist the deterministic healed default, or the fix would have
    traded the heal loop for silent damage retention.
    """
    from fno.agents.discover import _resolve_aliases

    sid = "aaaaaaaa-1111-2222-3333-444444444444"
    live = [{"session_id": sid, "short_id": "dup", "project": "etl"}]
    name_map = tmp_path / ".fno" / "session-names.json"
    name_map.parent.mkdir(parents=True, exist_ok=True)
    name_map.write_text(json.dumps({sid: "etl-dup-dup"}), encoding="utf-8")

    out = _resolve_aliases(live, name_map)

    assert "discarding damaged session alias 'etl-dup-dup'" in capsys.readouterr().err
    assert out == {sid: "etl-dup"}
    assert json.loads(name_map.read_text(encoding="utf-8")) == {sid: "etl-dup"}


def test_long_basename_collision_stays_inside_the_stored_cap(tmp_path, capsys):
    """A long project basename must not resurrect the discard loop.

    With a 50-char basename the full-id candidates exceed the 80-char stored
    cap, and the exhausted fallback used to emit a 98-char alias - which the
    persistence pass re-discarded with a user-visible warning on EVERY pass,
    the same heal loop this repair closes, on a warning instead of growth.
    The collision name is cut from the head instead, keeping the unique
    session id tail, so nothing the writer emits is ever thrown back.
    """
    from fno.agents.discover import _MAX_STORED_ALIAS_LEN, _resolve_aliases

    live = [
        {"session_id": "01a034f3-8bad-7e83-92e9-78cfd45881f7",
         "short_id": "01a034f3", "project": "p" * 50},
        {"session_id": "01a034f3-a58f-71e3-8ed4-bfe77f816168",
         "short_id": "01a034f3", "project": "p" * 50},
    ]
    name_map = tmp_path / ".fno" / "session-names.json"

    first = _resolve_aliases(live, name_map)
    second = _resolve_aliases(live, name_map)

    assert second == first
    assert len(set(first.values())) == len(first), "an alias resolves to two holders"
    for value in first.values():
        assert len(value) <= _MAX_STORED_ALIAS_LEN, value
    assert capsys.readouterr().err == ""
