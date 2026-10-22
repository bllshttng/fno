"""Tests for fno do pr watch CLI surface and plist installer.

TDD: tests written BEFORE implementation.  Every test targets a named
acceptance criterion from the task 1.3 spec.

No real ~/Library/LaunchAgents write, no real launchctl load, no real
claude/gh.  All I/O is redirected to tmp directories.
"""
from __future__ import annotations

import os
from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def _sandbox_bounce_receipts(tmp_path, monkeypatch):
    """Bounce receipts and pr_watch_bounce events land in a tmp state dir,
    never the developer's ~/.fno."""
    monkeypatch.setattr("fno.paths.state_dir", lambda: tmp_path / "state")


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture()
def tmp_home(tmp_path, monkeypatch):
    """Redirect HOME + state dir to a tmp directory for isolation."""
    home = tmp_path / "home"
    home.mkdir()
    fno_dir = home / ".fno"
    fno_dir.mkdir()
    monkeypatch.setenv("HOME", str(home))
    yield home


@pytest.fixture()
def tmp_launch_agents(tmp_path):
    """Return a temp dir standing in for ~/Library/LaunchAgents."""
    d = tmp_path / "LaunchAgents"
    d.mkdir()
    return d


@pytest.fixture()
def plist_kwargs(tmp_launch_agents):
    """Common kwargs for render_plist / install with a known LaunchAgents dir."""
    return {
        "launch_agents_dir": tmp_launch_agents,
        "fno_binary": "/usr/local/bin/fno",
        "install_path": str(Path(os.environ.get("PATH", "/usr/bin:/bin"))),
    }


# ---------------------------------------------------------------------------
# Helper: import _install lazily (module may not exist yet in RED phase)
# ---------------------------------------------------------------------------


def _install():
    from fno.pr_watch import _install as m
    return m


# ---------------------------------------------------------------------------
# AC3-HP: render_plist returns a string with the right shape
# ---------------------------------------------------------------------------


def test_ac3hp_render_plist_contains_required_keys(tmp_home, plist_kwargs):
    """render_plist() returns a valid XML plist string containing required keys."""
    m = _install()
    rendered = m.render_plist(**plist_kwargs)

    assert "sh.fno.pr-watcher" in rendered
    assert "fno" in rendered
    # AC4-HP: the argv carries the current command spelling; the retired
    # pr-watch token must not appear as a bare argv string.
    assert (
        "<string>do</string>\n"
        "    <string>pr</string>\n"
        "    <string>watch</string>\n"
        "    <string>tick</string>"
    ) in rendered
    assert "<string>pr-watch</string>" not in rendered
    assert "<false/>" in rendered  # RunAtLoad false
    # AC7-HP: both launchd log paths end under the logs/ subfolder, never
    # at the top level of the state root.
    assert "/logs/pr-watcher.out.log" in rendered
    assert "/logs/pr-watcher.err.log" in rendered
    assert "/.fno/pr-watcher.out.log" not in rendered
    assert "/.fno/pr-watcher.err.log" not in rendered
    # ProcessType Standard (x-c79d): the positive read is the control for the
    # negative one below.
    assert "<key>ProcessType</key>\n  <string>Standard</string>" in rendered
    assert "<string>Background</string>" not in rendered


# ---------------------------------------------------------------------------
# AC3-EDGE: plist security and correctness checks
# ---------------------------------------------------------------------------


def test_ac3edge_plist_has_path_and_home_no_api_key(tmp_home, plist_kwargs):
    """Rendered plist has PATH and HOME in EnvironmentVariables; no ANTHROPIC_API_KEY."""
    m = _install()
    rendered = m.render_plist(**plist_kwargs)

    assert "<key>PATH</key>" in rendered
    assert "<key>HOME</key>" in rendered
    assert "ANTHROPIC_API_KEY" not in rendered


# ---------------------------------------------------------------------------
# Config: PrWatchBlock schema
# ---------------------------------------------------------------------------


def test_config_pr_watch_block_defaults():
    """PrWatchBlock has the specified defaults."""
    from fno.config import PrWatchBlock

    block = PrWatchBlock()
    assert block.enabled is False
    assert block.interval_seconds == 600
    assert block.retries == 3
    assert block.max_age_days == 14
    assert block.model == "claude-haiku-4-5"


def test_config_pr_watch_nonmapping_degrades_to_defaults():
    """config.pr_watch given a non-mapping (e.g. 42) loads as defaults, never raises."""
    from fno.config import ConfigBlock

    cb = ConfigBlock.model_validate({"pr_watch": 42})
    assert cb.pr_watch.enabled is False
    assert cb.pr_watch.interval_seconds == 600


def _record_runner(calls, *, rc_by_command=None, timeout_command=None):
    """A _run_launchctl_timed stub that records calls and can inject rc/timeout."""
    rc_by_command = rc_by_command or {}

    def _run(*args, timeout_s=0):
        calls.append(args)
        command = args[0]
        if command == timeout_command:
            return (-1, True)
        return (rc_by_command.get(command, 0), False)

    return _run


def test_bounce_order_is_bootout_bootstrap_kickstart(tmp_launch_agents):
    m = _install()
    calls: list[tuple] = []
    calls: list[tuple] = []
    msg, rc = m.bounce(
        plist_path=tmp_launch_agents / "x.plist", uid=501,
        run=_record_runner(calls),
    )
    assert rc == 0
    assert [c[0] for c in calls] == ["bootout", "bootstrap", "kickstart"]
    assert calls[0] == ("bootout", "gui/501/sh.fno.pr-watcher")
    assert calls[1] == ("bootstrap", "gui/501", str(tmp_launch_agents / "x.plist"))
    assert calls[2] == ("kickstart", "-k", "gui/501/sh.fno.pr-watcher")


def test_bounce_tolerates_bootout_failure_when_not_loaded(tmp_launch_agents):
    """bootout returns nonzero for a not-loaded job; the bounce proceeds anyway."""
    m = _install()
    calls: list[tuple] = []
    msg, rc = m.bounce(
        plist_path=tmp_launch_agents / "x.plist", uid=501,
        run=_record_runner(calls, rc_by_command={"bootout": 1}),
    )
    assert rc == 0  # bootout nonzero is expected, not fatal
    assert [c[0] for c in calls] == ["bootout", "bootstrap", "kickstart"]


def test_bounce_bootstrap_failure_is_reported(tmp_launch_agents):
    m = _install()
    calls: list[tuple] = []
    msg, rc = m.bounce(
        plist_path=tmp_launch_agents / "x.plist", uid=501,
        run=_record_runner(calls, rc_by_command={"bootstrap": 5}),
        sleep=lambda _s: None,
    )
    assert rc == 1 and "bootstrap" in msg
    # A persistently-failing bootstrap is retried, then reported; never kickstarts.
    bootstrap_calls = [c[0] for c in calls if c[0] == "bootstrap"]
    assert len(bootstrap_calls) == m._BOOTSTRAP_RETRIES
    assert "kickstart" not in [c[0] for c in calls]


def test_bounce_kickstart_hang_names_the_wedged_step(tmp_launch_agents):
    """A HANG (not a nonzero rc) on kickstart is fatal and names the step."""
    m = _install()
    calls: list[tuple] = []
    msg, rc = m.bounce(
        plist_path=tmp_launch_agents / "x.plist", uid=501,
        run=_record_runner(calls, timeout_command="kickstart"),
    )
    assert rc == 1 and "kickstart" in msg and "timed out" in msg


def test_bounce_defers_while_tick_claim_is_young(tmp_launch_agents, monkeypatch):
    """AC4-HP: a tick mid-flight (live claim under one interval) makes
    the heal defer - no launchctl step runs, and the deferral is named."""
    m = _install()
    monkeypatch.setattr(m, "_tick_in_flight", lambda: 4242)
    calls: list[tuple] = []
    msg, rc = m.bounce(
        plist_path=tmp_launch_agents / "x.plist", uid=501,
        run=_record_runner(calls),
        defer_when_ticking=True,
    )
    assert (msg, rc) == ("tick in flight (pid 4242); bounce deferred", 0)
    assert calls == []


def test_tick_in_flight_asks_launchd(monkeypatch):
    """launchd owns the in-flight answer: a listed PID younger than one
    StartInterval defers the cure; an old tick, a missing PID line, or an
    unread launchctl (OSError, timeout) never blocks it."""
    import subprocess as _subprocess

    m = _install()

    def _world(pid_line: str, etime: str):
        return lambda argv: pid_line if argv[0] == "launchctl" else etime

    def probe(pid_line: str, etime: str):
        return m._tick_in_flight(run=_world(pid_line, etime))

    assert probe('"PID" = 8574;', "02:02") == 8574  # 122s old: in flight
    assert probe('"PID" = 8574;', "09:59") == 8574  # 599s: still young
    assert probe('"PID" = 8574;', "10:01") is None  # 601s: hung tick, bounces
    assert probe('"PID" = 8574;', "1-02:03:04") is None
    assert probe('"PID" = 8574;', "garbage") is None
    assert probe("", "02:02") is None  # job not loaded

    def _raise(exc):
        def _run(*_a, **_kw):
            raise exc

        return _run

    monkeypatch.setattr(m.subprocess, "run", _raise(OSError("no launchctl")))
    assert m._tick_in_flight() is None
    monkeypatch.setattr(
        m.subprocess, "run", _raise(_subprocess.TimeoutExpired("launchctl", 10))
    )
    assert m._tick_in_flight() is None


def test_refresh_watcher_write_failure_is_error(tmp_path, monkeypatch):
    """A plist write failure returns nonzero and never reaches the bounce."""
    m = _install()
    # A regular file where the LaunchAgents dir should be -> mkdir/write fails.
    blocker = tmp_path / "blocker"
    blocker.write_text("not a dir")
    monkeypatch.setattr(m, "bounce", lambda **kw: pytest.fail("must not bounce on write failure"))
    msg, rc = m.refresh_watcher(
        launch_agents_dir=blocker / "LaunchAgents",
        fno_binary="/x/fno-py",
    )
    assert rc == 1 and "failed to write plist" in msg


# ---------------------------------------------------------------------------
# An unchanged refresh must not re-register the agent
# (launchd posts a macOS background-activity notice on every re-registration)
# ---------------------------------------------------------------------------


def test_rendered_watcher_path_contains_cargo_bin(tmp_home, tmp_launch_agents, monkeypatch):
    """The rendered plist's PATH itself carries the cargo bin dir."""
    import plistlib

    m = _install()
    cargo_bin = tmp_home / ".cargo" / "bin"
    cargo_bin.mkdir(parents=True)
    monkeypatch.delenv("CARGO_HOME", raising=False)
    rendered = m.render_plist(
        launch_agents_dir=tmp_launch_agents,
        fno_binary=str(tmp_home / ".local" / "bin" / "fno-py"),
    )
    env = plistlib.loads(rendered.encode())["EnvironmentVariables"]

    assert str(cargo_bin) in env["PATH"].split(":")


def test_refresh_unchanged_skips_write_and_bounce(tmp_home, tmp_launch_agents, monkeypatch):
    m = _install()
    plist = tmp_launch_agents / "sh.fno.pr-watcher.plist"
    plist.write_text(
        m.render_plist(
            launch_agents_dir=tmp_launch_agents,
            fno_binary="/usr/local/bin/fno",
            interval=600,
        )
    )
    bounces: list = []
    monkeypatch.setattr(m, "bounce", lambda **kw: bounces.append(kw) or ("bounced", 0))

    msg, rc = m.refresh_watcher(
        launch_agents_dir=tmp_launch_agents,
        fno_binary="/usr/local/bin/fno",
        interval=600,
    )

    assert rc == 0 and "unchanged" in msg
    assert bounces == [], "an unchanged refresh must not re-register the agent"


def test_refresh_changed_reregisters(tmp_home, tmp_launch_agents, monkeypatch):
    m = _install()
    plist = tmp_launch_agents / "sh.fno.pr-watcher.plist"
    plist.write_text("<plist>stale</plist>")
    bounces: list = []
    monkeypatch.setattr(m, "bounce", lambda **kw: bounces.append(kw) or ("bounced", 0))

    msg, rc = m.refresh_watcher(
        launch_agents_dir=tmp_launch_agents,
        fno_binary="/usr/local/bin/fno",
        interval=600,
    )

    assert rc == 0 and len(bounces) == 1
    assert "stale" not in plist.read_text()
