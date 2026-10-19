"""Focused tests for the ``fno doctor --codex-bind`` lane canary.

The pane-binding measurement for this node found the production oracle
(rollout-fd) binding 0/20 spawns against codex 0.148, while the app-server
daemon oracle bound 10/10. `--codex-bind` is the standing canary so a future
codex upgrade that breaks either lane again reads as a red canary naming the
oracle and the installed version, not a mystery some weeks later.

Drives the exact production binding sequence (``_await_pane_binding`` +
``_make_codex_bind_probe``), so the mocking surface here mirrors
``test_spawn_pane.py``'s daemon-oracle tests rather than hand-rolling its own
poll loop.

Every test fakes the mux/subprocess seams at the module level - the suite
never spawns a real codex pane.
"""
from __future__ import annotations

import subprocess

from typer.testing import CliRunner

from fno import doctor
from fno.agents import codex_pane, mux_spawn
from fno.cli import app

runner = CliRunner()
SID = "019cc081-de0d-7283-97cc-751c46742a07"


def _proc(returncode: int = 0, stdout: str = "") -> subprocess.CompletedProcess:
    return subprocess.CompletedProcess(args=[], returncode=returncode, stdout=stdout, stderr="")


def _patch_common(monkeypatch, *, run_returncode: int = 0, pane_id_out: str = "7\n"):
    from fno.agents import codex_pane

    monkeypatch.setattr(doctor, "_codex_version", lambda: "codex-cli 0.148.0")
    # build_pane_argv consults the installed codex version by exec'ing the
    # real binary; the provider-exec guard blocks that, so pin it like the
    # agents suite's _stub_codex_cli_version fixture does.
    monkeypatch.setattr(mux_spawn, "_codex_cli_version", lambda: (0, 148, 0))
    monkeypatch.setattr(mux_spawn, "resolve_mux_session", lambda: "main")
    monkeypatch.setattr(
        mux_spawn, "_run_mux", lambda *a, **k: _proc(run_returncode, pane_id_out)
    )
    monkeypatch.setattr(mux_spawn, "_lookup_child_pid", lambda *a, **k: 4242)
    reaped: list[tuple] = []
    monkeypatch.setattr(
        mux_spawn,
        "_reap_spawned_pane",
        lambda session, pane_id, runner: (reaped.append((session, pane_id)), (True, ""))[1],
    )
    monkeypatch.setattr(
        codex_pane, "_codex_session_ids_loaded", lambda cwd, **_kw: set()
    )
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *a, **k: True)
    monkeypatch.setattr(mux_spawn, "_read_pane_tail", lambda *a, **k: "")
    # x-a095: the canary starts the daemon from the capability form and stops
    # it on every exit; both provider execs are faked here. Non-stop commands
    # still reach the real subprocess.run captured before the rebinding.
    monkeypatch.setattr(codex_pane, "ensure_codex_daemon", lambda *_a, **_k: None)
    real_run = subprocess.run
    monkeypatch.setattr(
        doctor.subprocess,
        "run",
        lambda argv, *a, **k: _proc(0)
        if list(argv)[:4] == ["codex", "app-server", "daemon", "stop"]
        else real_run(argv, *a, **k),
    )
    return reaped


def test_binds_via_the_fd_oracle_and_reports_it(monkeypatch) -> None:
    reaped = _patch_common(monkeypatch)
    monkeypatch.setattr(
        mux_spawn, "_backfill_codex_session_id", lambda *a, **k: SID
    )
    monkeypatch.setattr(codex_pane, "_codex_daemon_candidate", lambda *a, **k: None)

    result = doctor._codex_bind_report()

    assert result == {
        "bound": True,
        "session_id": SID,
        "oracle": "rollout-fd",
        "elapsed_s": result["elapsed_s"],
        "codex_version": "codex-cli 0.148.0",
        "error": None,
    }
    assert reaped == [("main", 7)]


def test_binds_via_the_daemon_oracle_when_the_fd_probe_misses(monkeypatch) -> None:
    _patch_common(monkeypatch)
    monkeypatch.setattr(
        mux_spawn, "_backfill_codex_session_id", lambda *a, **k: None
    )
    # The stability gate needs the SAME candidate on two consecutive daemon
    # probes; a constant lambda satisfies that without a call counter.
    monkeypatch.setattr(codex_pane, "_codex_daemon_candidate", lambda *a, **k: SID)
    monkeypatch.setattr(codex_pane, "_CODEX_DAEMON_PROBE_INTERVAL_S", 0.0)
    monkeypatch.setattr(mux_spawn.time, "sleep", lambda *_a, **_k: None)

    result = doctor._codex_bind_report()

    assert result["bound"] is True
    assert result["session_id"] == SID
    assert result["oracle"] == "daemon"


def test_neither_oracle_binding_fails_named_and_reaps(monkeypatch) -> None:
    _patch_common(monkeypatch)
    monkeypatch.setattr(doctor, "_CODEX_BIND_CANARY_WINDOW_S", 0.05)
    monkeypatch.setattr(mux_spawn.time, "sleep", lambda *_a, **_k: None)
    monkeypatch.setattr(
        mux_spawn, "_backfill_codex_session_id", lambda *a, **k: None
    )
    monkeypatch.setattr(codex_pane, "_codex_daemon_candidate", lambda *a, **k: None)

    result = doctor._codex_bind_report()

    assert result["bound"] is False
    assert result["session_id"] is None
    assert result["oracle"] is None
    assert "neither oracle" in result["error"]


def test_short_binding_candidate_cannot_pass_the_canary(monkeypatch) -> None:
    _patch_common(monkeypatch)
    monkeypatch.setattr(
        mux_spawn, "_backfill_codex_session_id", lambda *a, **k: SID[:8]
    )
    monkeypatch.setattr(codex_pane, "_codex_daemon_candidate", lambda *a, **k: None)

    result = doctor._codex_bind_report()

    assert result["bound"] is False
    assert result["session_id"] is None
    assert result["oracle"] is None
    assert "full canonical session id" in result["error"]


def test_cli_exits_nonzero_on_a_failed_bind(monkeypatch) -> None:
    monkeypatch.setattr(
        doctor,
        "_codex_bind_report",
        lambda: {
            "bound": False, "oracle": None, "elapsed_s": 0.1,
            "codex_version": "codex-cli 0.148.0", "error": "neither oracle bound",
        },
    )
    result = runner.invoke(app, ["doctor", "--codex-bind"])
    assert result.exit_code == 1
    assert "FAILED" in result.output


def test_codex_hooks_and_codex_bind_together_is_rejected_not_silently_dropped(
    monkeypatch,
) -> None:
    # codex_hooks is checked first; before this fix its own exclusivity
    # guard did not know about codex_bind, so the combination silently ran
    # --codex-hooks and dropped --codex-bind with no error.
    monkeypatch.setattr(
        doctor,
        "_codex_bind_report",
        lambda: (_ for _ in ()).throw(AssertionError("should never be reached")),
    )
    result = runner.invoke(app, ["doctor", "--codex-hooks", "--codex-bind"])
    assert result.exit_code == 2
    assert "--codex-hooks may only be combined with --json" in result.output


def _record_daemon_stops(monkeypatch) -> list[list[str]]:
    stops: list[list[str]] = []
    fallback = doctor.subprocess.run

    def record(argv, *a, **k):
        if list(argv)[:4] == ["codex", "app-server", "daemon", "stop"]:
            stops.append(list(argv))
            return _proc(0)
        return fallback(argv, *a, **k)

    monkeypatch.setattr(doctor.subprocess, "run", record)
    return stops


def test_canary_stops_its_private_daemon_when_the_pane_run_fails(monkeypatch) -> None:
    """The early return before any pane exists must not leak the daemon."""
    _patch_common(monkeypatch, run_returncode=1)
    stops = _record_daemon_stops(monkeypatch)

    result = doctor._codex_bind_report()

    assert result["bound"] is False
    assert len(stops) == 1
