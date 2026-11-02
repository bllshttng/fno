"""Row -> session-process resolution, tree RSS, and its roster
joins. A bg row's recorded pid names the PTY HOST; the rv socket farm names the
process that IS the session."""
from __future__ import annotations

from types import SimpleNamespace

import pytest


@pytest.fixture(autouse=True)
def _empty_rv_farm(tmp_path, monkeypatch):
    """Point the socket-farm root at an empty tmp dir so no test on this machine
    joins the operator's REAL live bg sessions."""
    monkeypatch.setenv("FNO_CC_DAEMON_RV_ROOT", str(tmp_path / "rv-farm"))


_LSOF_OUT = (
    "p37355\n"
    "f13\n"
    "n/tmp/cc-daemon-501/608d3bdb/rv/119e3c52.sock\n"
    "f17\n"
    "n/tmp/cc-daemon-501/608d3bdb/rv/119e3c52.sock\n"
    "p58529\n"
    "f15\n"
    "n/tmp/cc-daemon-501/608d3bdb/rv/040f5e88.sock\n"
)


def test_pid_map_parses_stem_to_holder():
    from fno.agents.session_procs import _pid_map_from_lsof

    m = _pid_map_from_lsof(_LSOF_OUT)
    # Two fds on one socket collapse to one stem -> one pid (first wins).
    assert m == {"119e3c52": 37355, "040f5e88": 58529}


def _farm(tmp_path):
    root = tmp_path / "farm"
    (root / "608d3bdb" / "rv").mkdir(parents=True)
    (root / "608d3bdb" / "rv" / "119e3c52.sock").touch()
    return root


def test_bg_socket_pid_map_one_lsof_for_the_farm(tmp_path, monkeypatch):
    from fno.agents import session_procs

    calls: list[list[str]] = []

    def fake_run(argv, **kw):
        calls.append(argv)
        return SimpleNamespace(returncode=0, stdout=_LSOF_OUT)

    monkeypatch.setattr(session_procs.subprocess, "run", fake_run)
    m = session_procs.bg_socket_pid_map(_farm(tmp_path))
    assert m["119e3c52"] == 37355  # the one socket the farm holds
    assert len(calls) == 1  # one batched call, never one lsof per socket
    assert "--" in calls[0] and any("119e3c52.sock" in a for a in calls[0])


def test_resolve_session_pid_falls_back_on_miss():
    from fno.agents.session_procs import resolve_session_pid

    assert (
        resolve_session_pid(
            harness="claude", short_id="ffffffff", pid=98779, socket_map={}
        )
        == 98779
    )
    # Non-claude harnesses have no host/session split: recorded pid verbatim.
    assert (
        resolve_session_pid(
            harness="codex", short_id="119e3c52", pid=42, socket_map={"119e3c52": 37355}
        )
        == 42
    )


# ---------------------------------------------------------------------------
# roster discovery carries the resolved pid
# ---------------------------------------------------------------------------


