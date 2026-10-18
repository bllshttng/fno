"""The orphan sweep: attribution, the withheld verdict, and reap narrowness.

The live positive control proves the NAME arm, the CWD arm and the kill path
on every run. It cannot prove the uid arm, the census arm, or the reap age
gate, because the probe is ours, is not a census row, and is seconds old. Those
three are proven here against a fabricated process table, and that division is
stated in the sweep's own output so nobody reads the control as covering more
than it does.
"""
from __future__ import annotations

import collections
import os
import time
from pathlib import Path

import pytest

from fno.agents import orphans

CpuTimes = collections.namedtuple("CpuTimes", "user system")


def _proc(
    pid: int,
    *,
    name: str = "sleep",
    argv0: str | None = None,
    cwd: str | None = None,
    uid: int | None = None,
    ppid: int = 1,
    cpu: float = 0.0,
    age: float = 60.0,
    create: float | None = None,
) -> dict:
    return {
        "pid": pid,
        "ppid": ppid,
        "name": name,
        "cmdline": [argv0 if argv0 is not None else name, "30"],
        # Default to the RUNNING uid, never a literal. A hardcoded 501 passes on
        # a mac and matches nothing on a CI runner, so every finding vanishes
        # and the whole file goes green-on-nothing in one environment and red in
        # the other.
        "uids": (uid if uid is not None else os.getuid(),) * 3,
        # An explicit `create` pins the process identity across two fabricated
        # scans. Deriving it from `age` alone makes "the same process an hour
        # later" a DIFFERENT process, which is a test that cannot see a
        # start-time-keyed bug.
        "create_time": create if create is not None else time.time() - age,
        "cpu_times": CpuTimes(cpu, 0.0),
        "cwd": cwd,
    }


@pytest.fixture
def table(monkeypatch):
    """Substitute the process enumeration and neutralise the live probes, so a
    test scan sees exactly the rows it declares."""
    rows: list[dict] = []
    monkeypatch.setattr(orphans, "iter_processes", lambda *a, **k: iter(rows))
    monkeypatch.setattr(orphans, "_spawn_probe", lambda *a, **k: None)
    monkeypatch.setattr(orphans, "_await_orphaned", lambda *a, **k: None)
    monkeypatch.setattr(orphans, "_repo_roots", lambda: ["/repo"])
    monkeypatch.setattr(orphans, "_census_pids", set)
    # NEVER let a test signal a real process. The fabricated probes are pinned
    # to pids 9001/9002, which are ordinary live pids on a developer machine,
    # and an unstubbed `_kill` sent them a real SIGTERM and a SIGKILL two
    # seconds later. A test suite that kills the developer's own processes is a
    # worse bug than anything it is testing. Individual tests re-stub `_kill`
    # to record calls; the default here is that nothing escapes.
    # True = our signal reached it, which is the ordinary case these tests mean.
    monkeypatch.setattr(orphans, "_kill", lambda pid: True)
    monkeypatch.setattr(orphans, "_pid_alive", lambda pid: False)
    return rows


#: The two pids `_scan_with_working_control` hands to the fake probes.
PROBE_PIDS = (9001, 9002)


def _reaped_pids(killed: list[int]) -> list[int]:
    """Kills that are not probe cleanup."""
    return [pid for pid in killed if pid not in PROBE_PIDS]


def _scan_with_working_control(monkeypatch, rows, **kwargs):
    """Run a scan whose two controls are declared present in the table.

    The probes are pinned to fixed pids so the fabricated table can carry a row
    for each, which keeps every test running through the SAME control gate the
    live command runs through rather than around it.
    """
    rows.insert(0, _proc(9001, name="sleep", argv0="fno-orphan-probe-test", cwd="/tmp/x"))
    rows.insert(1, _proc(9002, name="sleep", cwd="/repo"))
    pids = iter([9001, 9002])
    monkeypatch.setattr(orphans, "_spawn_probe", lambda *a, **k: next(pids))
    monkeypatch.setattr(orphans, "_await_orphaned", lambda *a, **k: 1)
    return orphans.scan(**kwargs)


# ─── the measured correction ────────────────────────────────────────────────


def test_a_real_fno_binary_is_not_claimed_by_the_name_arm() -> None:
    """`fno-agents-daemon` runs at PPID 1 in this repo right now. A bare
    fno-prefix test claims it, and `--reap` then kills the daemon. Requiring a
    genuine rename is what stops that."""
    daemon = _proc(
        2,
        name="fno-agents-daemon",
        argv0="/Users/x/.cargo/bin/fno-agents-daemon",
        cwd="/elsewhere",
    )
    assert orphans.was_renamed(daemon) is False
    assert orphans._attribute(daemon, ["/repo"]) is None


def test_a_probe_that_never_reparents_withholds_the_verdict(monkeypatch, table) -> None:
    """With no reaper there is nothing to compare a ppid against.

    Counting against a guessed reaper is the absence trap; say the reaper is
    unknown instead.
    """
    monkeypatch.setattr(orphans, "_spawn_probe", lambda *a, **k: 9001)
    monkeypatch.setattr(orphans, "_await_orphaned", lambda *a, **k: None)

    result = orphans.scan()
    assert result.broken
    assert "reaper is unknown" in result.broken_reason
    assert result.findings == []


def test_an_out_of_enum_skip_probe_refuses(monkeypatch, table) -> None:
    """A falsifier that cannot fail is worse than no falsifier.

    An unrecognised value skipped nothing, so the scan came back healthy and an
    operator read that as proof the control works. `FNO_ORPHANS_SKIP_PROBE=NAME`
    is one shift key away from that.
    """
    result = orphans.scan(skip_probe="NAME")
    assert result.broken
    assert "not 'name' or 'cwd'" in result.broken_reason
    assert result.findings == []


def test_outside_a_repo_the_sweep_withholds_rather_than_claim_cwd(
    monkeypatch, table
) -> None:
    """No git root means the CWD arm has nowhere to look.

    Falling back to os.getcwd() made every PPID-1 process under $HOME a
    finding. Inventing territory to keep a control green is the absence trap in
    a different hat.
    """
    monkeypatch.setattr(orphans, "_repo_roots", list)

    result = orphans.scan()
    assert result.broken
    assert "no git root" in result.broken_reason
    assert result.findings == []
    assert "verdict withheld (scan-broken)" in orphans.render(result)


def test_a_probe_that_died_on_its_own_does_not_certify_the_kill_path(
    monkeypatch, table
) -> None:
    """The control must prove WE ended the probe, not that it is gone.

    Reading only "it is gone now" lets a completely broken `_kill` report a
    healthy kill arm against any short-lived probe, and `--reap` then runs
    behind a control that never tested anything.
    """
    monkeypatch.setattr(orphans, "_kill", lambda pid: False)  # signal never landed
    monkeypatch.setattr(orphans, "_pid_alive", lambda pid: False)  # but it is gone

    result = _scan_with_working_control(monkeypatch, table)
    assert result.broken, "a kill nobody made cannot certify the kill path"
    assert "kill arm FAILED" in result.broken_reason
    assert "verdict withheld (scan-broken)" in orphans.render(result)


def test_our_own_daemon_is_counted_not_listed(monkeypatch, table) -> None:
    """PPID 1 is this daemon's normal state, so its presence carries no signal.

    Reported hourly it is noise nobody can act on, and every restart mints a
    fresh seen-key that speaks again. Counted instead, never dropped in
    silence, and never reaped either way.
    """
    table.append(
        _proc(
            42,
            name="fno-agents-daemon",
            argv0="/Users/x/.cargo/bin/fno-agents-daemon",
            cwd="/repo",
            age=90000,
        )
    )
    result = _scan_with_working_control(monkeypatch, table, reap=True)
    assert [f.pid for f in result.findings] == []
    assert result.daemons_excluded == 1
    assert "1 own-daemon" in orphans.render(result)
    assert result.reaped == []


# ─── attribution arms ───────────────────────────────────────────────────────


def test_cwd_arm_claims_an_ordinary_name_under_the_repo(monkeypatch, table) -> None:
    table.append(_proc(50, name="grep", cwd="/repo/.claude/worktrees/x-9f21"))
    result = _scan_with_working_control(monkeypatch, table)
    assert [(f.pid, f.arm) for f in result.findings] == [(50, "CWD")]


def test_foreign_uid_is_skipped(monkeypatch, table) -> None:
    """Not reachable by the live control: both probes are ours."""
    table.append(_proc(53, name="grep", cwd="/repo", uid=os.getuid() + 1))
    result = _scan_with_working_control(monkeypatch, table)
    assert result.findings == []


def test_census_worker_is_skipped(monkeypatch, table) -> None:
    """A `claude --bg` worker is detached to PPID 1 with a worktree cwd. Without
    this exclusion the first --reap takes down the fleet."""
    table.append(_proc(54, name="claude", cwd="/repo/.claude/worktrees/x-1"))
    monkeypatch.setattr(orphans, "_census_pids", lambda: {54})
    result = _scan_with_working_control(monkeypatch, table)
    assert result.findings == []
    assert result.census_excluded == 1


# ─── the positive control ───────────────────────────────────────────────────


def test_missing_name_probe_withholds_the_verdict(monkeypatch, table) -> None:
    table.append(_proc(60, name="grep", cwd="/repo"))
    rows_with_cwd_probe_only = table
    rows_with_cwd_probe_only.insert(0, _proc(9002, name="sleep", cwd="/repo"))
    pids = iter([9001, 9002])
    monkeypatch.setattr(orphans, "_spawn_probe", lambda *a, **k: next(pids))
    monkeypatch.setattr(orphans, "_await_orphaned", lambda *a, **k: 1)
    result = orphans.scan()  # _kill/_pid_alive stubbed by the `table` fixture
    assert result.broken
    assert "NAME" in (result.broken_reason or "")
    rendered = orphans.render(result)
    assert "verdict withheld (scan-broken)" in rendered
    assert "orphans:" not in rendered


def test_a_broken_scan_reaps_nothing(monkeypatch, table) -> None:
    table.append(_proc(61, name="sleep", argv0="fno-load-x1", age=3600))
    killed: list[int] = []
    monkeypatch.setattr(orphans, "_kill", _recording_kill(killed))
    result = orphans.scan(reap=True)
    assert result.broken
    assert killed == []


# ─── reap narrowness ────────────────────────────────────────────────────────
def _recording_kill(killed: list[int]):
    """Record the pid AND report that the signal landed.

    `list.append` returns None, and the reap now credits only a kill it
    actually delivered, so a bare `killed.append` stub silently meant "the
    signal never landed" and every reap assertion went empty.
    """

    def _kill(pid: int) -> bool:
        killed.append(pid)
        return True

    return _kill




def test_reap_kills_an_old_renamed_fno_process(monkeypatch, table) -> None:
    table.append(_proc(70, name="sleep", argv0="fno-load-x1", age=1800))
    killed: list[int] = []
    monkeypatch.setattr(orphans, "_kill", _recording_kill(killed))
    result = _scan_with_working_control(monkeypatch, table, reap=True)
    # The probes are killed too, on every run; that IS the live kill-path
    # control. Only non-probe kills are the reap under test here.
    assert _reaped_pids(killed) == [70]
    assert [f.pid for f in result.reaped] == [70]


def test_reap_spares_a_young_one(monkeypatch, table) -> None:
    table.append(_proc(71, name="sleep", argv0="fno-load-x1", age=60))
    killed: list[int] = []
    monkeypatch.setattr(orphans, "_kill", _recording_kill(killed))
    result = _scan_with_working_control(monkeypatch, table, reap=True)
    assert _reaped_pids(killed) == []
    assert [f.pid for f in result.findings] == [71]


def test_reap_spares_an_unnamed_orphan(monkeypatch, table) -> None:
    """Tonight's 73 processes were named `yes`. Every orphan predating the
    guard is unnamed, so the reap can never touch it, and that is correct."""
    table.append(_proc(72, name="yes", cwd="/repo/.claude/worktrees/x-01ae", age=27000))
    killed: list[int] = []
    monkeypatch.setattr(orphans, "_kill", _recording_kill(killed))
    result = _scan_with_working_control(monkeypatch, table, reap=True)
    assert _reaped_pids(killed) == []
    assert [f.pid for f in result.findings] == [72]
    assert "reaped: 0" in orphans.render(result)


# ─── the SessionStart quiet gate ────────────────────────────────────────────


def test_seen_key_ignores_age(monkeypatch, table, tmp_path: Path) -> None:
    """The bug the first two filter_new tests could not see.

    They reused one ScanResult inside one second. Keying on an age BUCKET made
    the key advance every minute, so a long-lived daemon read as new at every
    hourly sweep and reprinted at every session start forever. The key must
    depend on the process's START time, which is fixed, never on how long it
    has been running, which is not.

    Asserted on the key directly rather than through two fabricated scans: a
    second scan with a different create_time is a DIFFERENT process, so that
    route cannot express "the same process, later" at all.
    """
    born = time.time() - 600
    common = dict(
        pid=90, name="node", exe_name="node", renamed=False, cmdline="node",
        cwd="/repo", cpu_seconds=0.0, start_time=born, arm="CWD",
    )
    young = orphans.Finding(age_seconds=600, **common)
    old = orphans.Finding(age_seconds=600 + 3600, **common)
    assert orphans._seen_key(young) == orphans._seen_key(old)

    seen = tmp_path / ".orphan-sweep-seen"
    table.append(_proc(90, name="node", cwd="/repo", create=born))
    result = _scan_with_working_control(monkeypatch, table)
    assert orphans.filter_new(result, seen) is True
    assert orphans.filter_new(result, seen) is False


def test_an_unreadable_census_breaks_the_scan(monkeypatch, table) -> None:
    """An empty exclusion set reads as "no live workers" and hands every
    `claude --bg` process to a reap that SessionStart runs unattended."""
    table.append(_proc(92, name="claude", cwd="/repo", age=3600))
    monkeypatch.setattr(orphans, "_census_pids", lambda: None)
    result = _scan_with_working_control(monkeypatch, table, reap=True)
    assert result.broken
    assert "census" in (result.broken_reason or "")
    assert result.reaped == []
    assert "orphans:" not in orphans.render(result)


