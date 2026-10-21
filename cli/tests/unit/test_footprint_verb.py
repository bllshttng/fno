"""Tests for the hidden ``fno doctor footprint`` diagnostic."""

from __future__ import annotations

import json
import os
from pathlib import Path
from types import SimpleNamespace

import pytest
from typer.testing import CliRunner

from fno import doctor_footprint
from fno.footprint import Footprint, parse_footprint
from fno.cli import app

# Import the mux_spawn -> dispatch chain at collection, before any test
# patches anything: patching `fno.agents.registry.load_registry` first and
# importing mux_spawn later would bind the patched loader into dispatch's
# `from registry import load_registry` permanently, poisoning every later
# test in the process. One module-level import pins that order for the file.
import fno.agents.mux_spawn  # noqa: F401,E402


runner = CliRunner()


@pytest.fixture
def no_worker_roots(monkeypatch):
    from fno import doctor_footprint

    monkeypatch.setattr(
        doctor_footprint,
        "_live_root_pids",
        lambda **_kwargs: (set(), None),
    )
    monkeypatch.setattr(
        doctor_footprint,
        "_live_shared_serve_root_pids",
        lambda **_kwargs: (set(), None),
    )
    monkeypatch.setattr(
        doctor_footprint,
        "_codex_app_server_serve",
        lambda _snapshot: (set(), "absent"),
    )
    # Settings resolution shells git on a cold cache (repo-root discovery);
    # a shard that runs a footprint test first must not pay it, or the
    # ps-is-the-only-subprocess assertions see a git argv.
    monkeypatch.setattr(
        "fno.config.load_settings",
        lambda: SimpleNamespace(agents=SimpleNamespace(max_load_per_cpu=4.0)),
    )
    monkeypatch.setattr(
        "fno.config.load_settings_for_repo",
        lambda _root: SimpleNamespace(
            agents=SimpleNamespace(footprint_sustained_cpu_cores=None)
        ),
    )


def _fake_runner(
    monkeypatch, ps_output: str, roster: list[dict], calls: list[list[str]]
):
    """Fake the process-table door (``fno-agents census --ps``) and pin the
    live roster the count reads.

    The roster used to arrive over a ``fno agents list`` subprocess and now
    comes from an in-process registry read, so the fake moves with it. The
    table itself rides the same door, so the fake records the door calls a
    test wants to count.
    """
    from types import SimpleNamespace

    monkeypatch.setattr(
        "fno.agents.registry.load_registry",
        lambda: [SimpleNamespace(status="live", **row) for row in roster],
    )

    def door(verb, args=(), **kwargs):
        calls.append([verb, *args])
        return (None, {"ps": ps_output, "unreadable": 0})

    monkeypatch.setattr(doctor_footprint, "call_binary_json", door)


def _pin_load(
    monkeypatch,
    *,
    status: str,
    load: float = 1.0,
    ceiling: float = 96.0,
    load_15m: float | None = None,
):
    """Pin the spawn-load snapshot so a verdict test is hermetic: the real
    snapshot reads the host's live load average, which no exit-code assertion
    should ride on. Load is trend context only; nothing decides on it."""
    from types import SimpleNamespace

    from fno import doctor_footprint

    snapshot = SimpleNamespace(
        load_1m=load,
        max_load_per_cpu=8.0,
        load_ceiling=ceiling,
        load_cpu_count=int(ceiling // 8),
        spawn_load_status=status,
        load_5m=None,
        load_15m=load_15m,
    )
    monkeypatch.setattr(doctor_footprint, "_spawn_load_snapshot", lambda: snapshot)


def _pin_admission(monkeypatch, share: float = 0.5):
    """Pin the CPU axis's share ceiling so a verdict test never reads the real
    config roots (the default matches a stock install)."""
    from fno import doctor_footprint

    monkeypatch.setattr(doctor_footprint, "_admission_config", lambda: share)


def _pin_capacity(monkeypatch, cores: int):
    from fno import doctor_footprint

    monkeypatch.setattr(doctor_footprint, "_cpu_capacity_cores", lambda: cores)


def test_ac9_edge_ps_timeout_caller_refuses_with_exit_four(monkeypatch) -> None:
    from fno import doctor_footprint

    monkeypatch.setattr(
        doctor_footprint,
        "_read_ps",
        lambda **_kwargs: (None, "process table unavailable: timed out after 5.0s"),
    )

    result = runner.invoke(app, ["doctor", "footprint", "--json"])

    assert result.exit_code == 4, result.output
    assert json.loads(result.stdout) == {
        "error": "process table unavailable: timed out after 5.0s",
        "exit_code": 4,
    }


def test_live_root_pids_includes_live_detached_opencode_serve(monkeypatch, tmp_path) -> None:
    from fno import doctor_footprint

    (tmp_path / "opencode-serve.json").write_text(
        json.dumps({"pid": 900, "pid_start": 123}), encoding="utf-8"
    )
    monkeypatch.setenv("FNO_AGENTS_HOME", str(tmp_path))
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [])
    monkeypatch.setattr(
        "fno.agents.session_procs.bg_socket_pid_map", lambda: {}
    )
    monkeypatch.setattr(
        "fno.agents.spawn_gate._pid_alive",
        lambda pid, _start: True if pid == 900 else None,
    )

    assert doctor_footprint._live_root_pids() == (set(), None)
    assert doctor_footprint._live_shared_serve_root_pids() == ({900}, None)

    (tmp_path / "opencode-serve.json").write_text(
        json.dumps({"pid": 901, "pid_start": 123}), encoding="utf-8"
    )
    err = doctor_footprint._live_shared_serve_root_pids()[1]
    assert isinstance(err, doctor_footprint.AttributionGap)
    assert err.text.startswith("shared opencode serve root unattributed: ")
    assert "pid 901" in err.text and "start time could not be read" in err.text, err

    (tmp_path / "opencode-serve.json").write_text(
        json.dumps({"pid": 900, "pid_start": None}), encoding="utf-8"
    )
    err = doctor_footprint._live_shared_serve_root_pids()[1]
    assert isinstance(err, doctor_footprint.AttributionGap)
    assert err.text.startswith("shared opencode serve root unattributed: ")
    assert "carries no usable pid and pid_start pair" in err.text, err


def _assert_named_cause(error: str, expect: str) -> None:
    """The refusal names its own cause, not just the family."""
    family = "worker root liveness unavailable"
    assert error.startswith(f"{family}: "), error
    cause = error.split(": ", 1)[1].strip()
    assert cause, f"{error!r} carries the family word and no cause"
    assert expect in cause, error


def test_live_root_pids_routes_a_tokenless_pid_through_its_session(monkeypatch) -> None:
    """One tokenless row never blinds the instrument: its listed pid is
    untrusted and the session route attributes the real root instead."""
    import os
    from fno import doctor_footprint
    from types import SimpleNamespace

    row = SimpleNamespace(
        status="live",
        pid=902,
        pid_start_time=None,
        harness="claude",
        short_id="oc",
        name="t-tokenless",
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(doctor_footprint, "_row_transport_key", lambda _row: "oc")
    monkeypatch.setattr(
        "fno.agents.session_procs.bg_socket_pid_map", lambda **_kw: {"oc": os.getpid()}
    )

    roots, error = doctor_footprint._live_root_pids()
    assert error is None
    assert roots == {os.getpid()}


def test_live_root_pids_names_a_recycled_root_as_a_gap(monkeypatch) -> None:
    from fno import doctor_footprint
    from types import SimpleNamespace

    row = SimpleNamespace(
        status="live",
        pid=902,
        pid_start_time=123,
        harness="opencode",
        short_id="oc",
        name="bp-recycled-worker",
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(
        "fno.agents.spawn_gate._pid_alive",
        lambda _pid, _start: False,
    )
    # pid 902 runs, but under a start time the row never recorded: the
    # recorded worker is proven dead and the pid belongs to someone else.
    monkeypatch.setattr(
        "fno.agents.spawn_gate._process_start_time",
        lambda _pid: 999,
    )

    roots, verdict = doctor_footprint._live_root_pids(snapshot_pids={902})

    assert roots == set()
    assert isinstance(verdict, doctor_footprint.AttributionGap)
    assert "bp-recycled-worker" in verdict.text
    assert "pid=902" in verdict.text


def test_live_root_pids_still_refuses_when_recycling_is_unproven(monkeypatch) -> None:
    from fno import doctor_footprint
    from types import SimpleNamespace

    row = SimpleNamespace(
        status="live",
        pid=902,
        pid_start_time=123,
        harness="opencode",
        short_id="oc",
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(
        "fno.agents.spawn_gate._pid_alive",
        lambda _pid, _start: False,
    )
    monkeypatch.setattr(
        "fno.agents.spawn_gate._process_start_time",
        lambda _pid: None,
    )

    roots, error = doctor_footprint._live_root_pids(snapshot_pids={902})
    assert roots == set()
    _assert_named_cause(error, "recycling is unproven")


def test_terminal_row_stamp_compares_at_stored_precision() -> None:
    # Transition stamps are whole-second; a stamp of T covers [T, T+1), so a
    # snapshot inside that second cannot rule out a later transition.
    from datetime import datetime, timedelta, timezone
    from fno import doctor_footprint
    from types import SimpleNamespace

    def row(exited_at):
        return SimpleNamespace(exited_at=exited_at)

    base = datetime(2026, 8, 25, 10, 0, 59, tzinfo=timezone.utc)
    changed = doctor_footprint._terminal_row_changed_after_snapshot
    assert changed(row(base.strftime("%Y-%m-%dT%H:%M:%SZ")), base.timestamp() + 0.5)
    assert not changed(
        row((base - timedelta(seconds=2)).strftime("%Y-%m-%dT%H:%M:%SZ")),
        base.timestamp() + 0.5,
    )
    assert changed(
        row(base.replace(microsecond=700000).isoformat()), base.timestamp() + 0.5
    )
    assert not changed(
        row(base.replace(microsecond=300000).isoformat()), base.timestamp() + 0.5
    )
    assert not changed(row(None), base.timestamp() + 0.5)
    assert changed(row("not-a-stamp"), base.timestamp() + 0.5)


def test_footprint_reads_a_registry_one_schema_ahead_but_refuses_a_skipped_row(
    monkeypatch, tmp_path
) -> None:
    from fno import doctor_footprint
    from fno.agents.registry import SCHEMA_VERSION, LoadedRegistry

    monkeypatch.setenv("FNO_AGENTS_HOME", str(tmp_path))
    (tmp_path / "registry.json").write_text(
        json.dumps({"schema_version": SCHEMA_VERSION + 1, "agents": []}),
        encoding="utf-8",
    )
    assert doctor_footprint.live_registry_rows() == ([], None)
    assert doctor_footprint._live_root_pids() == (set(), None)

    monkeypatch.setattr(
        "fno.agents.registry.load_registry",
        lambda: LoadedRegistry([], complete=False, rows_complete=False),
    )
    assert doctor_footprint._live_root_pids() == (
        set(),
        "worker registry incomplete",
    )


def test_live_root_pids_refuses_unavailable_pidless_worker_discovery(monkeypatch) -> None:
    from fno import doctor_footprint
    from types import SimpleNamespace

    row = SimpleNamespace(
        status="live",
        pid=None,
        harness="claude",
        short_id="cl-bg",
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(
        "fno.agents.session_procs.bg_socket_pid_map",
        lambda **_kwargs: {},
    )
    # x-a457: the map missing the row is no longer proof of death on its own;
    # an UNREADABLE roster oracle is what keeps this row in the gap.
    monkeypatch.setattr("fno.agents.session_procs.roster_pid_map", lambda: None)

    roots, error = doctor_footprint._live_root_pids()
    assert roots == set()
    assert isinstance(error, doctor_footprint.AttributionGap)
    assert "socket map" in error.text


def test_live_root_pids_drops_routed_corpse_row_dead_in_both_daemon_oracles(
    monkeypatch,
) -> None:
    """x-a457: a routed row in NEITHER the rv socket farm nor the claude roster
    names a session the daemon no longer holds. It is a corpse the registry
    never retired, not an unattributed live process, so the reading stops
    calling its (nonexistent) cost a gap."""
    from fno import doctor_footprint
    from types import SimpleNamespace

    row = SimpleNamespace(
        status="live",
        pid=None,
        pid_start_time=None,
        harness="claude",
        short_id="deadbee",
        name="corpse",
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(
        "fno.agents.session_procs.bg_socket_pid_map",
        lambda **_kwargs: {},
    )
    monkeypatch.setattr("fno.agents.session_procs.roster_pid_map", lambda: {})

    assert doctor_footprint._live_root_pids() == (set(), None)


def test_live_root_pids_attributes_routed_row_through_roster_pid(monkeypatch) -> None:
    """The roster is the second oracle AND a fallback attribution: its pid is
    the PTY host hosting the session, a real process the reading should
    attribute when the rv socket farm missed it."""
    from fno import doctor_footprint
    from types import SimpleNamespace

    row = SimpleNamespace(
        status="live",
        pid=None,
        pid_start_time=None,
        harness="claude",
        short_id="alive123",
        name="hosted",
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(
        "fno.agents.session_procs.bg_socket_pid_map",
        lambda **_kwargs: {},
    )
    monkeypatch.setattr(
        "fno.agents.session_procs.roster_pid_map", lambda: {"alive123": 903}
    )
    monkeypatch.setattr(
        "fno.agents.spawn_gate._pid_alive",
        lambda pid, _start: pid == 903,
    )

    assert doctor_footprint._live_root_pids() == ({903}, None)


def test_live_root_pids_drops_unrouted_row_with_expired_claim(monkeypatch) -> None:
    """x-a457: an unrouted row whose worker claim store positively reports no
    live holder is a corpse row. The 14 rows that kept this box's spawn gate
    refusing were exactly this population - claims expired, rows still live.
    The harness is one with no shared daemon, so the row rides the per-row
    witness path (x-cb2b moved pidless codex rows to the daemon verdict)."""
    from fno import doctor_footprint
    from types import SimpleNamespace

    row = SimpleNamespace(
        status="live",
        pid=None,
        pid_start_time=None,
        harness="luna",
        short_id="",
        name="t-stale-lane",
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(doctor_footprint, "_claim_witness", lambda _name: "stale")

    assert doctor_footprint._live_root_pids() == (set(), None)


def test_live_root_pids_keeps_unrouted_row_on_unreadable_claim_store(monkeypatch) -> None:
    """An unreadable claim store proves nothing: fail closed, gap row."""
    from fno import doctor_footprint
    from types import SimpleNamespace

    row = SimpleNamespace(
        status="live",
        pid=None,
        pid_start_time=None,
        harness="codex",
        short_id="",
        name="t-unreadable",
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(doctor_footprint, "_claim_witness", lambda _name: None)

    roots, error = doctor_footprint._live_root_pids()
    assert roots == set()
    assert isinstance(error, doctor_footprint.AttributionGap)


def _codex_thread_row(name: str, session_id: str | None = None):
    from types import SimpleNamespace

    return SimpleNamespace(
        status="live",
        pid=None,
        pid_start_time=None,
        harness="codex",
        short_id="",
        name=name,
        harness_session_id=session_id,
    )


def test_live_root_pids_resolves_a_codex_thread_row_through_its_rollout(
    monkeypatch,
) -> None:
    """x-9958 Task 3: a codex thread row's session id has an accepting route -
    the rollout fd - and a resolved, live pid attributes like any root."""
    from fno import doctor_footprint

    row = _codex_thread_row("t-codex-thread", "tid-907")
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(
        "fno.agents.session_procs.codex_rollout_pid_map",
        lambda session_ids, **kwargs: {"tid-907": 907},
    )
    monkeypatch.setattr(doctor_footprint, "_root_pid_is_live", lambda pid, start: True)

    assert doctor_footprint._live_root_pids() == ({907}, None)


def test_live_root_pids_spares_an_advancing_row_and_names_only_the_silent_one(
    monkeypatch,
) -> None:
    """x-9958 Task 2: a pidless row advancing by transcript evidence is a live
    worker, not an unattributable process - it drops from the gap and the
    reading stands as an undercount. Positive marker: the silent sibling is
    still named, and only the silent row was ever witnessed."""
    from fno import doctor_footprint
    import fno.agents.session_truth as session_truth

    advancing = _codex_thread_row("t-adv-row")
    silent = _codex_thread_row("t-silent-row")
    monkeypatch.setattr(
        "fno.agents.registry.load_registry", lambda: [advancing, silent]
    )

    def fake_truth(handle, **kwargs):
        if handle == "t-adv-row":
            return {
                "state": "working",
                "last_activity_age_s": 30,
                "observed_model": None,
            }
        return {
            "state": "unknown",
            "reason": "not-found",
            "last_activity_age_s": None,
            "observed_model": {"kind": "no-transcript"},
        }

    monkeypatch.setattr(session_truth, "resolve_session_truth", fake_truth)

    witness_calls: list[str] = []
    monkeypatch.setattr(
        doctor_footprint,
        "_claim_witness",
        lambda name: witness_calls.append(name) or "live",
    )

    roots, error = doctor_footprint._live_root_pids()
    assert roots == set()
    assert isinstance(error, doctor_footprint.AttributionGap)
    assert "t-silent-row" in error.text
    assert "t-adv-row" not in error.text
    assert witness_calls == ["t-silent-row"]


def test_live_root_pids_pane_row_costs_the_attributed_mux_server(monkeypatch) -> None:
    """A pane burns CPU inside the mux server process the reading attributes;
    whatever the probe answers, the pane adds no unattributed cost. Only an
    answer the mux could NOT give leaves the cost unproven. The harness is
    one with no shared daemon, so the row rides the pane-probe path (x-cb2b
    moved pidless codex rows to the daemon verdict)."""
    from fno import doctor_footprint
    from types import SimpleNamespace

    row = SimpleNamespace(
        status="live",
        pid=None,
        pid_start_time=None,
        harness="luna",
        short_id="",
        name="t-pane",
        mux={"session": "main", "pane_id": 7},
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.setattr(
        "fno.agents.mux_spawn._mux_pane_alive", lambda _mux, **_kwargs: True
    )

    assert doctor_footprint._live_root_pids() == (set(), None)


def test_ac9_edge_cause_payload_is_bounded(monkeypatch) -> None:
    from fno import doctor_footprint
    from fno.footprint import parse_footprint

    rows = "\n".join(
        f"{100 + i} 1 01:00:00 {100 - i}.0 1024 fno-agents-worker worker-{i} "
        + "x" * 5000
        for i in range(10)
    )
    reading = parse_footprint(f"PID PPID ELAPSED %CPU RSS COMMAND\n{rows}")

    payload = doctor_footprint._payload(
        reading,
        process_threshold=None,
        exit_code=0,
        top_limit=5,
        command_limit=64,
    )

    assert len(payload["top"]) == 5
    assert all(
        len(json.dumps(item["command"]).encode("utf-8")) <= 64
        for item in payload["top"]
    )

    reading.top[0] = (100.0, "😀" * 1000)
    payload = doctor_footprint._payload(
        reading,
        process_threshold=None,
        exit_code=0,
        top_limit=1,
        command_limit=64,
    )
    assert len(json.dumps(payload["top"][0]["command"]).encode("utf-8")) <= 64


def test_broken_spawn_gate_import_is_loud_not_misdiagnosed(monkeypatch) -> None:
    # A renamed spawn_gate private must surface as ImportError, not degrade
    # every reading to "worker root discovery unavailable".
    from types import SimpleNamespace

    from fno import doctor_footprint
    from fno.agents import spawn_gate as gate_module

    row = SimpleNamespace(
        status="live", pid=5, pid_start_time=1, harness="codex", short_id="cd"
    )
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
    monkeypatch.delattr(gate_module, "_pid_alive")
    monkeypatch.delattr(gate_module, "_process_start_time")

    with pytest.raises(ImportError):
        doctor_footprint._live_root_pids(snapshot_pids=set())


def test_ac5_edge_capacity_honors_cpu_quota(monkeypatch) -> None:
    from fno import doctor_footprint

    monkeypatch.setattr(doctor_footprint, "_cpu_quota_cores", lambda: 2.0)
    monkeypatch.setattr(
        doctor_footprint.os,
        "process_cpu_count",
        lambda: 64,
        raising=False,
    )
    monkeypatch.setattr(doctor_footprint.os, "cpu_count", lambda: 64)
    monkeypatch.setattr(
        doctor_footprint.os,
        "sched_getaffinity",
        lambda _pid: set(range(64)),
        raising=False,
    )

    assert doctor_footprint._cpu_capacity_cores() == 2


def test_ac5_hp_json_reports_fleet_totals_and_cpu_shares(
    monkeypatch, no_worker_roots
) -> None:
    from fno import doctor_footprint

    _pin_load(monkeypatch, status="within")
    _fake_runner(
        monkeypatch,
        """\
        PID PPID ELAPSED %CPU RSS COMMAND
        100 1 01:00:00 20.0 1024 fno-agents-worker --run
        101 100 00:00:05 80.0 1024 cargo test -p fno
        200 1 01:00:00 100.0 1024 unrelated-build
        """,
        [{"name": "worker-a"}],
        [],
    )

    result = runner.invoke(app, ["doctor", "footprint", "--json"])

    assert result.exit_code == 0, result.output
    payload = json.loads(result.stdout)
    capacity = doctor_footprint._cpu_capacity_cores()
    assert payload["descendant_cpu_cores"] == pytest.approx(0.8)
    assert payload["fleet_cpu_cores"] == pytest.approx(1.0)
    assert payload["descendant_process_count"] == 1
    assert payload["cpu_capacity_cores"] == capacity
    assert payload["fleet_percent_capacity"] == pytest.approx(100 / capacity)
    assert payload["fleet_percent_measured_cpu"] == pytest.approx(50.0)


def test_ac6_edge_cause_only_excludes_observer_subtree_and_skips_roster(
    monkeypatch, no_worker_roots
) -> None:
    from fno import doctor_footprint

    observer_pid = os.getpid()
    calls: list[list[str]] = []
    monkeypatch.setattr(
        doctor_footprint,
        "_live_root_pids",
        lambda **_kwargs: (set(), None),
    )
    _fake_runner(
        monkeypatch,
        f"""\
        PID PPID ELAPSED %CPU RSS COMMAND
        {observer_pid} 1 01:00:00 20.0 1024 fno-py doctor footprint
        999 {observer_pid} 01:00:00 80.0 1024 ps -Ao pid,ppid
        100 1 01:00:00 20.0 1024 fno-agents-worker --run
        101 100 01:00:00 80.0 1024 cargo test -p fno
        """,
        [],
        calls,
    )

    result = runner.invoke(app, ["doctor", "footprint", "--json", "--cause-only"])

    assert result.exit_code == 0, result.output
    payload = json.loads(result.stdout)
    assert payload["process_count"] == 2
    assert payload["fleet_cpu_cores"] == pytest.approx(1.0)
    # Git calls the config-root resolver may shell are not the cause-only
    # contract's subject; what it promises is ONE table read and no roster walk.
    assert [call for call in calls if call[0] == "census"] == [
        ["census", "--ps"]
    ]
    assert not [call for call in calls if "agents" in call]


def _ps_with(header: str, *rows: str) -> str:
    return header + "\n" + "\n".join(rows) + "\n"


def test_ac1_hp_one_bad_row_keeps_the_reading_and_the_gate_admits(
    monkeypatch, no_worker_roots
) -> None:
    from fno import doctor_footprint
    from fno.agents.spawn_gate import _cpu_axis

    good = "\n".join(
        f"{1000 + n} 1 01:00:00 0.5 1024 /usr/bin/tool{n}" for n in range(200)
    )
    ps_output = _ps_with(
        "PID PPID ELAPSED %CPU RSS COMMAND",
        good,
        "9999 1 - 20.0 1024 /bin/echo oops",
    )
    _fake_runner(monkeypatch, ps_output, [], [])
    monkeypatch.setattr(os, "getloadavg", lambda: (1.0, 1.0, 1.0))
    monkeypatch.setattr("fno.agents.spawn_gate._load_cpus", lambda: 12)

    reading, error = doctor_footprint.cause_reading()

    assert error is None
    assert reading is not None
    assert reading.unparsed_lines == 1
    # The 200 good rows all parsed: unattributed (no fno root), so they ride
    # in the measured whole-machine number, not the attributed process count.
    assert reading.process_count == 0
    assert reading.measured_cpu_cores == pytest.approx(1.0)

    admission = _cpu_axis((reading, None))
    assert admission.verdict == "admit"
    assert admission.axis == "fleet_cpu_share"
    assert admission.axis != "cpu_instrument"


def test_ac1_edge_all_bad_rows_still_refuse_and_name_the_rows(
    monkeypatch, no_worker_roots
) -> None:
    from fno import doctor_footprint

    ps_output = _ps_with(
        "PID PPID ELAPSED %CPU RSS COMMAND",
        "12345 1 00:01 - 4096 /usr/bin/true",
        "second bad row here",
        "third bad row here",
    )
    _fake_runner(monkeypatch, ps_output, [], [])

    reading, error = doctor_footprint.cause_reading()

    assert reading is None
    assert error is not None
    # The floor arm: every data row failing means the instrument is
    # unreadable, and the refusal names the count and the masked first row.
    assert "all 3 ps row(s) failed to parse" in error
    assert "row 1 pid 12345 cpu: 12345 1 00:01 <tok:1> 4096" in error


def test_cause_reading_still_voids_on_serve_discovery_exception(monkeypatch) -> None:
    from fno import doctor_footprint

    monkeypatch.setattr(
        doctor_footprint,
        "_live_shared_serve_root_pids",
        lambda **_kwargs: (
            set(),
            "shared serve root discovery unavailable: OSError",
        ),
    )
    _fake_runner(
        monkeypatch,
        _ps_with(
            "PID PPID ELAPSED %CPU RSS COMMAND",
            "501 1 01:00:00 0.5 1024 /usr/bin/tool",
        ),
        [],
        [],
    )

    reading, error = doctor_footprint.cause_reading()

    assert reading is None
    assert error == (
        "footprint unavailable: shared serve root discovery unavailable: OSError"
    )


def test_ac2_sec_no_argv_token_reaches_any_render(
    monkeypatch, no_worker_roots
) -> None:

    def ps(rows: str) -> str:
        return _ps_with("PID PPID ELAPSED %CPU RSS COMMAND", *rows.splitlines())

    secret_row = (
        "12345 1 00:01 - 4096 /usr/bin/curl -H Authorization:Bearer SUPERSECRET1234"
    )
    _fake_runner(monkeypatch, ps(secret_row + "\n200 1 - 0.0 1024 tool"), [], [])
    # The refusal renders twice: json error payload and text readout.
    for argv in (
        ["doctor", "footprint", "--json", "--cause-only"],
        ["doctor", "footprint", "--cause-only"],
    ):
        result = runner.invoke(app, argv)
        assert result.exit_code == 4, (argv, result.output)
        for secret in ("SUPERSECRET1234", "Authorization", "curl", "Bearer"):
            assert secret not in result.output, argv

    # The surviving reading renders twice: json payload and text readout.
    _fake_runner(
        monkeypatch,
        ps(secret_row + "\n200 1 01:00:00 0.0 1024 /usr/bin/tool"),
        [],
        [],
    )
    for argv in (
        ["doctor", "footprint", "--json", "--cause-only"],
        ["doctor", "footprint", "--cause-only"],
    ):
        result = runner.invoke(app, argv)
        assert result.exit_code == 0, (argv, result.output)
        for secret in ("SUPERSECRET1234", "Authorization", "curl", "Bearer"):
            assert secret not in result.output, argv
        if "--json" in argv:
            payload = json.loads(result.stdout)
            assert payload["unparsed_lines"] == 1
            assert payload["unparsed_samples"][0]["masked"].startswith("12345 1")


def test_sustained_cpu_threshold_derives_from_capacity_and_honors_override(
    monkeypatch,
) -> None:
    """The old absolute 1.0 asked a 12-core machine's fleet to idle at 8%.
    The threshold is now a fraction of capacity; a config override pins it
    absolutely for a small box."""
    from fno import doctor_footprint as df

    assert df.sustained_cpu_threshold(12) == pytest.approx(1.2)
    assert df.sustained_cpu_threshold(1) == pytest.approx(
        df.SUSTAINED_CPU_FLOOR_CORES
    )
    monkeypatch.setattr(df, "_footprint_cpu_override", lambda: 2.0)
    assert df.sustained_cpu_threshold(12) == pytest.approx(2.0)


def test_ac7_edge_short_lived_descendant_counts_in_fleet_cpu(
    monkeypatch, no_worker_roots
) -> None:
    """A 100% descendant for 5s lands in the fleet's CPU reading. It no longer
    decides the exit on its own: sustained CPU is reported against a derived
    threshold, while the exit answers the two alarms (capacity, leak)."""

    _pin_load(monkeypatch, status="within")
    _pin_admission(monkeypatch)
    _pin_capacity(monkeypatch, 12)
    _fake_runner(
        monkeypatch,
        """\
        PID PPID ELAPSED %CPU RSS COMMAND
        100 1 01:00:00 20.0 1024 fno-agents-worker --run
        101 100 00:00:05 100.0 1024 cargo test -p fno
        """,
        [{"name": "worker-a"}],
        [],
    )

    result = runner.invoke(app, ["doctor", "footprint"])

    assert result.exit_code == 0, result.output
    assert "fleet CPU: 1.200 cores" in result.stdout
    assert "verdict: admit on fleet_cpu_share (10.0% against 50.0%)" in result.stdout


def test_ac8_edge_descendants_do_not_consume_direct_process_threshold(
    monkeypatch, no_worker_roots
) -> None:

    _fake_runner(
        monkeypatch,
        """\
        PID PPID ELAPSED %CPU RSS COMMAND
        100 1 01:00:00 20.0 1024 fno-agents-worker --run
        101 100 01:00:00 20.0 1024 cargo test -p fno
        102 101 01:00:00 20.0 1024 rustc --crate-name fno
        """,
        [{"name": "worker-a"}],
        [],
    )

    _pin_load(monkeypatch, status="within")

    result = runner.invoke(app, ["doctor", "footprint"])

    assert result.exit_code == 0, result.output
    assert "processes: 3" in result.stdout
    assert "unexplained processes: 0 (1 direct, roster explains 2)" in result.stdout


def test_ac3_hp_reports_both_thresholds_and_exits_zero(
    monkeypatch, no_worker_roots
) -> None:
    from fno import doctor_footprint

    calls: list[list[str]] = []
    _pin_load(monkeypatch, status="within")
    _pin_capacity(monkeypatch, 4)
    # Path resolution and the settings load shell git on cold caches, and CI
    # sharding decides which test pays the cold read. Pin the repo root (the
    # wrapper reads the env var per call and answers without a subprocess)
    # and the override seam, so the exact-call-list contract is
    # order-independent.
    monkeypatch.setenv("FNO_REPO_ROOT", str(Path(__file__).resolve().parents[3]))
    monkeypatch.setattr(doctor_footprint, "_footprint_cpu_override", lambda: None)
    _fake_runner(
        monkeypatch,
        """\
        PID ELAPSED %CPU RSS COMMAND
        101 01:00:00 20.0 1024 fno mux serve
        102 00:00:01 92.0 1024 fno --version
        """,
        [{"name": "worker-a"}, {"name": "worker-b"}],
        calls,
    )

    result = runner.invoke(app, ["doctor", "footprint"])

    assert result.exit_code == 0, result.output
    assert "sustained CPU: 0.200 cores (threshold 0.400 from 4 cpus; a separate axis - it did not decide the verdict)" in result.stdout
    assert "processes: 2" in result.stdout
    assert "unexplained processes: 0 (2 direct, roster explains 3)" in result.stdout
    assert "transient calls: 1" in result.stdout
    # The census door is the only exec left: the roster count reads the
    # registry in process, so there is no second shell-out to budget.
    assert [call for call in calls] == [
        ["census", "--ps"],
    ]


def test_ac4_edge_capacity_over_exits_three_and_names_top_consumers(
    monkeypatch, no_worker_roots
) -> None:
    """The fleet over its share ceiling and a leak BOTH fire; the CPU axis keeps
    the exit (3) as the more urgent alarm and the leak still prints with its
    own words. The load pinned beside it decides nothing (x-c588)."""

    _pin_load(monkeypatch, status="within", load=110.4, load_15m=500.0)
    _pin_admission(monkeypatch)
    _pin_capacity(monkeypatch, 12)
    _fake_runner(
        monkeypatch,
        """\
        PID ELAPSED %CPU RSS COMMAND
        201 02:00:00 800.0 1024 fno mux serve
        202 01:00:00 400.0 2048 fno-agents-daemon --serve
        """,
        [],
        [],
    )

    result = runner.invoke(app, ["doctor", "footprint"])

    assert result.exit_code == 3
    assert "verdict: hold on fleet_cpu_share" in result.stdout
    assert "unexplained processes: 1 (2 direct, roster explains 1)" in result.stdout
    assert "fno mux serve (800.0%)" in result.stdout
    assert "fno-agents-daemon --serve (400.0%)" in result.stdout


def test_ac4_edge_unexplained_processes_get_their_own_exit(
    monkeypatch, no_worker_roots
) -> None:
    """A leak without a capacity breach exits 5 - the leak's own code, not the
    capacity code the old merged verdict borrowed (defect 1 in the plan). The
    verdict names the unexplained processes as the cause; the admitted CPU
    axis is context, not the cause."""

    _pin_load(monkeypatch, status="within")
    _pin_admission(monkeypatch)
    _pin_capacity(monkeypatch, 4)
    _fake_runner(
        monkeypatch,
        """\
        PID ELAPSED %CPU RSS COMMAND
        211 02:00:00 10.0 1024 fno worker-a
        212 02:00:00 10.0 1024 fno worker-b
        """,
        [],
        [],
    )

    result = runner.invoke(app, ["doctor", "footprint"])

    assert result.exit_code == 5
    assert (
        "verdict: leak on unexplained processes (1 of 2 direct, roster explains 1; "
        "cpu admission: admit on fleet_cpu_share (5.0% against 50.0%), a separate "
        "axis - it did not decide the verdict) (exit 5)" in result.stdout
    )
    assert "sustained CPU: 0.200 cores" in result.stdout
    assert "processes: 2" in result.stdout
    assert "unexplained processes: 1 (2 direct, roster explains 1)" in result.stdout


def test_ac5_edge_roster_failure_degrades_the_threshold_not_the_reading(
    monkeypatch, no_worker_roots, tmp_path
) -> None:
    """x-e040: the roster is an enrichment. On roster failure the measurement
    still prints, with the threshold degraded away and the reason named. The
    old contract killed the whole report (exit 4, no reading)."""
    # A cold HOME sends config resolution climbing to the canonical root,
    # whose resolver shells `git worktree list`. Pin the root so the startup
    # probe is an env read, and the pinned budget below stays the verdict's
    # alone.
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    from fno import doctor_footprint

    calls: list[list[str]] = []

    def ps_only(verb, args=(), **kwargs):
        calls.append([verb, *args])
        return (
            None,
            {
                "ps": "PID ELAPSED %CPU RSS COMMAND\n101 01:00:00 20.0 1024 fno daemon\n",
                "unreadable": 0,
            },
        )

    def unreadable_registry():
        raise OSError("registry is a directory")

    # The CLI app resolves config roots through git when the process cache is
    # cold, and the admission pair and CPU override read config per verdict.
    # Warm the cache and pin the seams before the recorder goes in, so the
    # recorded window holds only what a footprint run itself executes.
    import contextlib

    from fno.config import load_settings

    with contextlib.suppress(Exception):
        load_settings()
    _pin_admission(monkeypatch)
    monkeypatch.setattr(doctor_footprint, "_footprint_cpu_override", lambda: None)
    monkeypatch.setattr(doctor_footprint, "call_binary_json", ps_only)
    monkeypatch.setattr("fno.agents.registry.load_registry", unreadable_registry)
    # The census door is the only exec this report may spend. The roster is
    # not the only enrichment: repo-root and worktree attribution also shell
    # out when their declarations are cold, so pin both seams hermetic.
    monkeypatch.setenv("FNO_REPO_ROOT", str(os.getcwd()))
    import fno.paths as _paths

    monkeypatch.setattr(_paths, "resolve_canonical_worktree", lambda *a, **k: None)
    _pin_load(monkeypatch, status="within")
    _pin_capacity(monkeypatch, 4)

    result = runner.invoke(app, ["doctor", "footprint"])

    assert result.exit_code == 0
    assert "roster unavailable" in result.stdout
    assert "unexplained processes: unknown" in result.stdout
    assert "processes:" in result.stdout
    assert "degraded: roster unavailable" in result.stdout
    # The roster no longer costs a subprocess: the census door is the only exec.
    assert [call[0] for call in calls] == ["census"]


def test_codex_app_server_serve_attributes_a_live_root(monkeypatch, tmp_path) -> None:
    """x-cb2b: the fno-harness-daemon state file names a live root; that pid
    joins the attributed roots and the verdict reads live."""
    from fno import doctor_footprint

    codex_home = tmp_path / "codex"
    (codex_home / "app-server-daemon").mkdir(parents=True)
    (codex_home / "app-server-daemon" / "fno-harness-daemon.json").write_text(
        json.dumps({"pid": 910, "processStartToken": 555}), encoding="utf-8"
    )
    monkeypatch.setenv("CODEX_HOME", str(codex_home))
    monkeypatch.setattr(
        "fno.agents.spawn_gate._pid_alive",
        lambda pid, _start: True if pid == 910 else None,
    )

    assert doctor_footprint._codex_app_server_serve(set()) == ({910}, "live")


def test_gap_reading_prints_the_measurement_and_admits_on_the_upper_bound(monkeypatch):
    """x-7783 LD3: an attribution gap no longer forces exit 4. The reading
    stands, the share becomes an interval, and a ceiling above the interval
    admits with `bound` recording that the upper edge decided. Both gates
    read the admission object, not the exit code."""
    from fno import doctor_footprint

    reading = doctor_footprint.parse_footprint(
        "PID PPID ELAPSED %CPU RSS COMMAND\n100 1 01:00:00 0.5 1024 fno daemon\n",
        excluded_root_pids=set(),
        attributed_root_pids=set(),
        threshold_excluded_root_pids=set(),
    )._replace(
        attribution_gap="1 pidless codex row(s) unresolved",
        measured_cpu_cores=0.02,
    )
    monkeypatch.setattr(
        doctor_footprint, "cause_reading", lambda: (reading, None)
    )
    _pin_load(monkeypatch, status="within", load_15m=2.0)
    _pin_admission(monkeypatch)
    _pin_capacity(monkeypatch, 12)
    result = runner.invoke(app, ["doctor", "footprint", "--json", "--cause-only"])
    assert result.exit_code == 0, result.output
    payload = json.loads(result.stdout)
    assert payload["process_count"] >= 1
    assert "codex" in payload["attribution_gap"]
    assert payload["admission"]["verdict"] == "admit"
    assert payload["admission"]["bound"] == "upper"
    assert payload["exit_code"] == 0


def test_cpu_admission_hot_machine_with_gap_is_undecidable():
    """The mirror: the same hot reading with a gap widens share_high to the
    machine share, the ceiling falls inside that band, and the verdict is
    undecidable."""
    from fno import doctor_footprint

    reading = Footprint(
        sustained_cpu_cores=0.0,
        descendant_cpu_cores=0.0,
        fleet_cpu_cores=0.6,
        descendant_process_count=0,
        direct_process_count=0,
        transient_call_count=0,
        process_count=0,
        rss_gb=0.0,
        measured_cpu_cores=11.4,
        top=[],
        unparsed_lines=0,
        attribution_gap="2 rows unattributed",
    )
    adm = doctor_footprint.cpu_admission(
        reading, capacity_cores=12.0, share_ceiling=0.5
    )
    assert adm.verdict == "undecidable"
    assert adm.bound == "upper"
    assert adm.share_high == pytest.approx(0.95)


def test_ac10_hp_machine_census_is_machine_wide():
    """x-d6ad AC10: machine_process_count and runnable_count count every
    parsed row, distinct from the roster-scoped counts beside them."""
    rows = [
        "  50    1 R      10:00 90.0 1024 /bin/run-top",
        "  51    1 S      10:00  0.0 1024 /bin/sleeper",
        "  52    1 Z      10:00  0.0 1024 (defunct)",
        "  53    1 RN     10:00 12.0 1024 cargo test",
        "  54    1 S      10:00  0.0 1024 /bin/other",
    ]
    reading = parse_footprint("\n".join(rows), attributed_root_pids={50})
    assert reading.machine_process_count == 5
    assert reading.runnable_count == 2  # R and RN; Z is not runnable
    # The roster-scoped counts sit beside them, unchanged in meaning.
    assert reading.process_count == 1
    assert reading.direct_process_count == 1
    assert reading.measured_cpu_cores == pytest.approx(1.02)


def test_parse_reads_a_headered_state_snapshot():
    """A real headered `ps -Ao pid,ppid,state,etime,%cpu,rss,command` snapshot
    routes to the state shape."""
    headered = (
        "  PID  PPID STAT     ELAPSED  %CPU   RSS COMMAND\n"
        "   80     1 S+       10:00   5.0  1024 /bin/watcher\n"
        "   81     1 R        10:00  90.0  1024 /bin/spinner"
    )
    reading = parse_footprint(headered)
    assert reading.machine_process_count == 2
    assert reading.runnable_count == 1
    assert reading.measured_cpu_cores == pytest.approx(0.95)


# ---------------------------------------------------------------------------
