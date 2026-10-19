"""The CPU probe the Rust gate reads: its time budget, its knobs, and the
console-script entry that must answer byte for byte like the verb.
"""
from __future__ import annotations

import json

import pytest


def test_rust_probe_budget_exceeds_the_python_measurement_budget():
    """The two runtimes must not disagree about admission on a slow box.

    Both gates refuse when fleet attribution is unreadable, so whichever one
    gives up first refuses first. Python calls `cause_reading` IN PROCESS and
    spends its whole budget measuring. Rust runs the same reading as a
    subprocess, so its budget must also cover spawning the CLI and importing
    it. Equal numbers therefore do NOT mean equal behaviour: they make the
    Rust gate time out first, and since this node a timeout REFUSES rather
    than merely losing the explanation.

    Read from the Rust source because there is no shared constant to import.
    A cheap string read is worth more than an untested comment, and this
    fails loudly if either budget moves.
    """
    import inspect
    import re
    from pathlib import Path

    from fno import doctor_footprint

    python_budget = inspect.signature(
        doctor_footprint.cause_reading
    ).parameters["timeout"].default
    assert python_budget == 5.0

    rust = Path(__file__).resolve().parents[3] / "crates/fno-agents/src/spawn_gate.rs"
    source = rust.read_text()
    match = re.search(
        r"const FOOTPRINT_PROBE_BUDGET: Duration = Duration::from_secs\((\d+)\)",
        source,
    )
    assert match, "FOOTPRINT_PROBE_BUDGET missing or renamed in the Rust gate"
    rust_budget = int(match.group(1))

    assert rust_budget > python_budget, (
        f"Rust probe budget {rust_budget}s must exceed the Python measurement "
        f"budget {python_budget}s, or the Rust gate refuses on a loaded box "
        f"where the Python gate admits"
    )


def test_non_finite_never_disarms_a_machine_guard():
    """`nan` loses every comparison and `inf` wins every one.

    Either way the ceiling stops refusing while still reading as
    configured, which is worse than a value that is merely wrong. All
    sibling knobs shared the hole, so all are checked.
    """
    from fno.config import AgentsBlock

    fields = {
        "min_free_gb": 4.0,
        "max_load_per_cpu": 8.0,
        "max_fleet_cpu_share": 0.5,
    }
    for name, default in fields.items():
        for bad in ("nan", "inf", "-inf", "NaN", float("nan"), float("inf")):
            got = getattr(AgentsBlock(**{name: bad}), name)
            assert got == default, f"{name}={bad!r} coerced to {got}, not {default}"


class TestCauseMainProbeEntry:
    """AC1-HP: the console script IS the verb, byte for byte.

    The Rust gate reads the probe's stdout and exit code; if the narrow entry
    point ever answered differently from `fno-py doctor footprint --json
    --cause-only`, the two halves of the probe transport would disagree about
    the same machine. So the parity is asserted directly, under the same
    patched reading.
    """

    @staticmethod
    def _reading(gap=None):
        from fno.footprint import parse_footprint

        rows = "\n".join(
            f"{100 + i} 1 01:00:00 {20 - i}.0 1024 fno-agents-worker worker-{i}"
            for i in range(3)
        )
        reading = parse_footprint(f"PID PPID ELAPSED %CPU RSS COMMAND\n{rows}")
        return reading._replace(attribution_gap=gap)

    def _run_both(self, monkeypatch, capsys, *, gap=None, error=None):
        import typer
        from types import SimpleNamespace

        from fno import doctor_footprint

        if error is not None:
            monkeypatch.setattr(
                doctor_footprint, "cause_reading", lambda **kw: (None, error)
            )
        else:
            reading = self._reading(gap)
            monkeypatch.setattr(
                doctor_footprint, "cause_reading", lambda **kw: (reading, None)
            )
        # x-7783: pin the admission inputs so the box's live numbers cannot
        # move between the two calls; the snapshot feeds only the trend.
        monkeypatch.setattr(
            doctor_footprint,
            "_spawn_load_snapshot",
            lambda: SimpleNamespace(load_1m=None, load_cpu_count=12, load_15m=1.0),
        )
        monkeypatch.setattr(doctor_footprint, "_admission_config", lambda: 0.5)
        monkeypatch.setattr(doctor_footprint, "_cpu_capacity_cores", lambda: 12)

        def invoke(entry):
            # footprint_command raises typer.Exit in-process; cause_main
            # translates it to SystemExit, which is the parity under test.
            with pytest.raises((typer.Exit, SystemExit)) as ei:
                entry()
            code = getattr(ei.value, "exit_code", None)
            if code is None:
                code = ei.value.code
            return capsys.readouterr().out, code

        verb_out, verb_code = invoke(
            lambda: doctor_footprint.footprint_command(json_output=True, cause_only=True)
        )
        probe_out, probe_code = invoke(doctor_footprint.cause_main)
        return (verb_out, verb_code), (probe_out, probe_code)

    def test_gapped_reading_parity_admits_on_the_upper_bound(self, monkeypatch, capsys):
        """x-7783 LD3: a gap no longer forces exit 4. Both transports answer
        identically with the interval carried and the exit 0."""
        (verb, verb_code), (probe, probe_code) = self._run_both(
            monkeypatch, capsys, gap="21 unmapped bg-socket rows"
        )
        assert (probe, probe_code) == (verb, verb_code)
        assert probe_code == 0
        payload = json.loads(probe)
        assert payload["exit_code"] == 0
        assert payload["admission"]["bound"] == "upper"
        assert payload["attribution_gap"] == "21 unmapped bg-socket rows"

    def test_failed_reading_names_the_error_and_exits_4(self, monkeypatch, capsys):
        (verb, verb_code), (probe, probe_code) = self._run_both(
            monkeypatch, capsys, error="footprint unavailable: ps failed"
        )
        assert (probe, probe_code) == (verb, verb_code)
        assert probe_code == 4
        assert json.loads(probe) == {
            "error": "footprint unavailable: ps failed",
            "exit_code": 4,
        }

# The gate-side lead share, schema and fleet-load cases decided inside the
# ONE Rust gate now (crates/fno-agents/src/spawn_gate.rs and
# spawn_gate_lanes.rs tests). The transport contract lives in
# tests/agents/test_spawn_gate.py::TestTransport.
