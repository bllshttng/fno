"""A codex thread worker proves its identity from its own spawn record.

The process-tree walk cannot answer for the thread lane: every thread worker
is a WebSocket client of the ONE shared ``codex app-server`` daemon, so N
workers share one pid and ancestry cannot name which thread a shell belongs
to. The spawn record can. These tests pin the cwd-keyed read of that record
and the resolver fill that consumes it, including the guard that a proven
ambient identity is never overwritten.
"""

import json

from fno.harness_identity import live_thread_row_for_cwd
from fno.paths import agents_registry_path
from fno.paths_testing import use_tmpdir

# HARNESS_SESSION_MARKERS is the ambient marker set; identity tests scrub it so
# the pytest host's own harness env cannot pose as the synthetic session.
from fno.harness_identity import HARNESS_SESSION_MARKERS


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


def _two_worker_rows(tmp_path):
    """Two live thread rows: one worktree each, one session id each."""
    cwd_a = tmp_path / "worker-a"
    cwd_b = tmp_path / "worker-b"
    cwd_a.mkdir()
    cwd_b.mkdir()
    sid_a = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    sid_b = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae5"
    rows = [
        _thread_row("worker-a", str(cwd_a), sid_a),
        _thread_row("worker-b", str(cwd_b), sid_b),
    ]
    return rows, str(cwd_a), str(cwd_b), sid_a, sid_b


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


# --- The resolver fill -------------------------------------------------------

from fno.claims.self_identity import resolve_self_identity  # noqa: E402
from fno.claims import session_pid as _session_pid  # noqa: E402

_REAL_RESOLVE_HARNESS = _session_pid.resolve_session_harness

_SID_A = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
_SID_B = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae5"
_SID_C = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae6"


def _scrub_env(monkeypatch):
    for marker, _ in HARNESS_SESSION_MARKERS:
        monkeypatch.delenv(marker, raising=False)
    for name in (
        "FNO_HARNESS_NAME",
        "FNO_HARNESS_SESSION_ID",
        "FNO_SESSION_HARNESS",
        "FNO_SESSION_PID",
    ):
        monkeypatch.delenv(name, raising=False)


def test_proven_ambient_identity_wins_and_the_registry_is_not_consulted(
    tmp_path, monkeypatch
):
    """A proven claude identity whose cwd happens to match a live thread row is
    returned unchanged: the resolved session id short-circuits before the
    registry is read, so no proven identity launders into codex."""
    use_tmpdir(monkeypatch, tmp_path)
    here = tmp_path / "operator-shell"
    here.mkdir()
    _write_registry(tmp_path, monkeypatch, [_thread_row("worker", str(here), _SID_A)])
    _scrub_env(monkeypatch)
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", _SID_B)
    monkeypatch.setattr(_session_pid, "resolve_session_harness", lambda from_pid=None: "claude")
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_attester_identity",
        lambda env=None: (_SID_B, "process"),
    )

    monkeypatch.chdir(here)
    # Booby-trap the registry read: if the fill ever ran past the
    # session-id short-circuit, this raises and fails the test.
    monkeypatch.setattr(
        "fno.claims.self_identity.live_thread_row_for_cwd",
        lambda cwd: (_ for _ in ()).throw(AssertionError("registry read past short-circuit")),
    )
    ident = resolve_self_identity()

    assert ident.harness == "claude"
    assert ident.session_id == _SID_B
    assert ident.session_id != _SID_A
    assert ident.disposition == "single"


def test_harness_contradiction_leaves_the_answer_untouched(tmp_path, monkeypatch):
    """A resolved claude harness with no provable id disagrees with the codex
    row at this cwd; the walk is authoritative on contradiction, so the answer
    stays ambiguous rather than adopting the record's id."""
    cwd_a = tmp_path / "worker-a"
    cwd_a.mkdir()
    _write_registry(tmp_path, monkeypatch, [_thread_row("worker", str(cwd_a), _SID_A)])
    _scrub_env(monkeypatch)
    # A proven claude family whose two markers disagree on the id resolves to
    # ambiguous with the harness kept and the id dropped - the only shape that
    # reaches the fill with a non-empty harness and no session id.
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", _SID_B)
    monkeypatch.setenv("CLAUDE_SESSION_ID", _SID_C)
    monkeypatch.setattr(
        _session_pid, "resolve_session_harness", lambda from_pid=None: "claude"
    )

    monkeypatch.chdir(cwd_a)
    ident = resolve_self_identity()

    assert ident.harness == "claude"
    assert ident.session_id is None
    assert ident.disposition == "ambiguous"


def test_agreeing_unproven_codex_harness_adopts_the_record_id(tmp_path, monkeypatch):
    """The real thread-worker shell shape: ambient codex markers the walk cannot
    prove (a sibling could have written them), resolving to a proven harness
    with no provable id. The cwd-keyed record supplies the id - the record, not
    the marker, is the ground, so this is not a circular fill."""
    cwd_a = tmp_path / "worker-a"
    cwd_a.mkdir()
    _write_registry(tmp_path, monkeypatch, [_thread_row("worker", str(cwd_a), _SID_A)])
    _scrub_env(monkeypatch)
    monkeypatch.setenv("CODEX_THREAD_ID", _SID_B)
    monkeypatch.setenv("CODEX_SESSION_ID", _SID_C)

    monkeypatch.chdir(cwd_a)
    ident = resolve_self_identity()

    assert ident.harness == "codex"
    assert ident.session_id == _SID_A
    assert ident.disposition == "spawn_record"


def test_cross_family_markers_stay_fail_closed(tmp_path, monkeypatch):
    """A process carrying two families' markers standing in a thread worker's
    worktree is a bystander, not the worker: a malformed or contradictory
    answer is never overwritten by the record, whatever disposition the walk
    returned."""
    cwd_a = tmp_path / "worker-a"
    cwd_a.mkdir()
    _write_registry(tmp_path, monkeypatch, [_thread_row("worker", str(cwd_a), _SID_A)])
    _scrub_env(monkeypatch)
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", _SID_B)
    monkeypatch.setenv("CODEX_THREAD_ID", _SID_C)

    monkeypatch.chdir(cwd_a)
    ident = resolve_self_identity()

    assert ident.session_id is None
    assert ident.disposition != "spawn_record"


def test_foreign_legacy_marker_blocks_adoption(tmp_path, monkeypatch):
    """A legacy claude marker beside a codex marker reads single-family to the
    canonical parse but two-family in markers_present; the families-agreement
    guard uses the full marker set, so the record is not adopted."""
    cwd_a = tmp_path / "worker-a"
    cwd_a.mkdir()
    _write_registry(tmp_path, monkeypatch, [_thread_row("worker", str(cwd_a), _SID_A)])
    _scrub_env(monkeypatch)
    monkeypatch.setenv("CLAUDE_SESSION_ID", _SID_B)
    monkeypatch.setenv("CODEX_THREAD_ID", _SID_C)

    monkeypatch.chdir(cwd_a)
    ident = resolve_self_identity()

    assert ident.session_id is None
    assert ident.disposition != "spawn_record"


def test_resolve_owned_identity_command_stamps_the_spawn_record(tmp_path, monkeypatch):
    """The production path end to end: init's manifest stamper command answers
    HARNESS=codex SESSION_ID=<row id> for a thread worker standing in its own
    worktree - the answer whose absence stamped harness=unknown and refused
    the worker's node claim."""
    from typer.testing import CliRunner

    from fno.cli import app

    rows, cwd_a, cwd_b, sid_a, sid_b = _two_worker_rows(tmp_path)
    _write_registry(tmp_path, monkeypatch, rows)
    _scrub_env(monkeypatch)

    monkeypatch.chdir(cwd_a)
    result = CliRunner().invoke(app, ["do", "target", "resolve-owned-identity"])
    assert result.exit_code == 0, result.output
    fields = {
        line.split("=", 1)[0]: line.split("=", 1)[1]
        for line in result.stdout.splitlines()
        if "=" in line
    }
    assert fields["HARNESS"] == "codex"
    assert fields["SESSION_ID"] == sid_a
    assert fields["DISPOSITION"] == "spawn_record"
    assert fields["COLLISION"] == ""

    # One shared app-server pid, two thread workers: each cwd resolves to its
    # OWN row, never the first match.
    monkeypatch.chdir(cwd_b)
    ident_b = resolve_self_identity()
    assert ident_b.session_id == sid_b != sid_a
    assert ident_b.disposition == "spawn_record"
