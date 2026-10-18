"""The session_pid shim over the native `claim session-pid` verb.

The walk itself is Rust (`spawn_context::session_identity_from_table`), so
these tests pin the shim's contract only: one cached exec per from_pid (AC6),
and the degrade-to-uncapturable shape. Assertions sit on `_session_identity`
(the cache seam) because the module's public wrappers are neutralized by the
suite-wide `_neutral_host_harness` fixture.
"""

import json
import subprocess
from pathlib import Path
from unittest import mock

import pytest

from fno.claims import session_pid


def _run_returning(payload):
    return subprocess.CompletedProcess(args=[], returncode=0, stdout=json.dumps(payload), stderr="")


@pytest.fixture(autouse=True)
def _fresh_cache():
    session_pid._clear_session_identity_cache()
    yield
    session_pid._clear_session_identity_cache()


def test_both_halves_come_from_one_cached_read():
    run = mock.MagicMock(return_value=_run_returning({"session_pid": 4242, "harness": "claude"}))
    with (
        mock.patch.object(subprocess, "run", run),
        mock.patch(
            "fno.rust_binary.resolve_binary",
            return_value=Path("/usr/local/bin/fno-agents"),
        ),
    ):
        first = session_pid._session_identity(100)
        second = session_pid._session_identity(100)
    assert first == (4242, "claude")
    assert second == (4242, "claude")
    # AC6: one exec total, whatever the call order.
    assert run.call_count == 1
    assert run.call_args.args[0] == [
        "/usr/local/bin/fno-agents",
        "claim",
        "session-pid",
        "--json",
        "--from-pid",
        "100",
    ]


def test_failed_read_degrades_to_none_without_raising():
    with mock.patch.object(subprocess, "run", side_effect=OSError("binary gone")):
        assert session_pid._session_identity(100) == (None, None)


def test_pid_dies_with_session_still_a_deny_list():
    assert session_pid.pid_dies_with_session("claude") is True
    assert session_pid.pid_dies_with_session("codex") is False
    assert session_pid.pid_dies_with_session(None) is True
