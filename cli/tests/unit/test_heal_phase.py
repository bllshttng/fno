"""The pr-watch tick's heal phase gate: armed, unarmed, and no-binary paths.

Every assertion names a positive marker: the argv the ONE drive loop
carries, the unarmed short-circuit that never resolves the binary. "Nothing
ran" is only ever asserted beside a same-run positive that proves the phase
was reached.
"""
from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

from fno.pr_watch._heal_phase import run_heal_phase


class Recorder:
    def __init__(self) -> None:
        self.runs: list[list[str]] = []

    def resolve(self):
        return Path("/bin/fno-agents")

    def run(self, argv, **kwargs):
        self.runs.append(argv)
        assert kwargs.get("check") is False, "the tick never fails on a red PR"


def _settings(*, armed: bool) -> SimpleNamespace:
    return SimpleNamespace(
        auto_heal=SimpleNamespace(enabled=armed),
    )


def test_an_armed_tick_spawns_one_drive_loop_carrying_every_root(tmp_path):
    # The per-root loop is a Rust loop now: the tick pays ONE spawn whose
    # argv carries every root, in root order.
    rec = Recorder()
    roots = [tmp_path / "a", tmp_path / "b", tmp_path / "c"]

    outcome = run_heal_phase(
        _settings(armed=True), roots, resolve_binary=rec.resolve, run=rec.run
    )

    assert outcome == "ran"
    assert len(rec.runs) == 1, f"one spawn: {rec.runs}"
    argv = rec.runs[0]
    assert argv[:3] == ["/bin/fno-agents", "pr-heal", "--all"], f"{argv}"
    assert "--apply" in argv and "--detach" in argv, f"{argv}"
    tail = argv[argv.index("--cwd"):]
    assert tail[1::2] == [str(r) for r in roots], f"{argv}"


def test_an_unarmed_tick_resolves_nothing_and_runs_nothing():
    rec = Recorder()
    # The positive control: the same call with the flag on DOES run, so a
    # zero below means the gate, not a phase that never executed.
    armed = run_heal_phase(
        _settings(armed=True), [Path("/tmp")], resolve_binary=rec.resolve, run=rec.run
    )
    assert armed == "ran" and rec.runs, "control: the armed path runs"

    unarmed = run_heal_phase(
        _settings(armed=False), [Path("/tmp")], resolve_binary=rec.resolve, run=rec.run
    )

    assert unarmed == "unarmed"
    assert len(rec.runs) == 1, "the unarmed tick added no run"


def test_a_missing_binary_is_reported_and_runs_nothing(tmp_path):
    rec = Recorder()
    outcome = run_heal_phase(
        _settings(armed=True),
        [tmp_path],
        resolve_binary=lambda: None,
        run=rec.run,
    )
    assert outcome == "no-binary"
    assert rec.runs == []


def test_an_armed_tick_with_no_roots_never_claims_a_run(tmp_path):
    # "ran" without a run is the false receipt this phase exists not to
    # print: no root means no pr_heal_tick row, and the log must say so.
    rec = Recorder()
    outcome = run_heal_phase(
        _settings(armed=True), [], resolve_binary=rec.resolve, run=rec.run
    )
    assert outcome == "no-roots"
    assert rec.runs == []


def test_a_stale_binary_that_exits_4_reports_failed_and_names_the_repair(tmp_path, caplog):
    # A stale binary exits 4 on the one spawn: nothing ran, no pr_heal_tick
    # row will land, the status line must not keep a stale "last run", and
    # the warning names `fno doctor` (AC3-ERR). The gate row comes from
    # cli.py for any non-"ran" answer.
    import logging
    import types

    def failing(argv, **kwargs):
        return types.SimpleNamespace(returncode=4)

    with caplog.at_level(logging.WARNING):
        outcome = run_heal_phase(
            _settings(armed=True),
            [tmp_path / "a", tmp_path / "b"],
            resolve_binary=Recorder().resolve,
            run=failing,
        )

    assert outcome == "failed", "an all-failed drive loop is not a run"
    assert any("fno doctor" in r.message for r in caplog.records), caplog.text
