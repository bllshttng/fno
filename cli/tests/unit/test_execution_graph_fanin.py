"""Fan-in completeness: tally_fan_in + orchestrator.classify_worker_return.

Proves the deterministic barrier count refuses synthesis on any unresolved
input. Feeds null, empty, malformed, duplicate, failed, and missing worker
returns end-to-end (raw output string -> classify -> tally) and asserts exact
expected-versus-observed counts, per the plan's verification step 4.
"""
from __future__ import annotations

import importlib.util
from pathlib import Path

from fno.events.verify_child_promise import FanInTally, tally_fan_in

REPO = Path(__file__).resolve().parents[3]


def _load_orch():
    spec = importlib.util.spec_from_file_location(
        "do_orchestrator", REPO / "skills/execute/orchestrator.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _classify_many(outputs):
    orch = _load_orch()
    return [orch.classify_worker_return(o) for o in outputs]


# ── tally directly ───────────────────────────────────────────────────────────


def test_all_expected_completed_is_complete():
    t = tally_fan_in(["a", "b"], [("a", "completed"), ("b", "completed")])
    assert t == FanInTally(expected=2, completed=2, failed=0, duplicate=0, malformed=0, missing=0)
    assert t.complete is True


def test_failed_blocks_even_when_all_present():
    t = tally_fan_in(["a", "b"], [("a", "completed"), ("b", "failed")])
    assert t.failed == 1 and t.missing == 0
    assert t.complete is False


def test_malformed_counted_and_blocks():
    # None id and unknown kind are both malformed, attributed to no node.
    t = tally_fan_in(["a"], [(None, "completed"), ("a", "garbage")])
    assert t.malformed == 2 and t.completed == 0 and t.missing == 1
    assert t.complete is False


def test_duplicate_counted_first_wins():
    t = tally_fan_in(["a"], [("a", "completed"), ("a", "failed")])
    assert t.duplicate == 1 and t.completed == 1 and t.failed == 0
    assert t.complete is True  # first observation for a wins; a completed


# ── end-to-end: raw worker output -> classify -> tally ───────────────────────


def test_classify_maps_canonical_statuses():
    outs = [
        '```json\n{"result":"SUCCESS","task":"a"}\n```',
        '```json\n{"result":"DONE_WITH_CONCERNS","task":"b"}\n```',
        '```json\n{"result":"FAILED","task":"c","error":"boom"}\n```',
        '```json\n{"result":"BLOCKED","task":"d","reason":"dep"}\n```',
    ]
    pairs = _classify_many(outs)
    assert pairs == [("a", "completed"), ("b", "completed"), ("c", "failed"), ("d", "failed")]


def test_null_and_empty_output_are_explicit_kinds():
    # x-1862: (None, None) is never returned. Empty output says "nothing was
    # observed" (the instrument never ran, or the pipeline lost it); non-empty
    # unparseable output says "it ran, and this partial text is not an answer".
    pairs = _classify_many(["", "no contract here", '{"result":"NONSENSE","task":"x"}'])
    assert pairs == [(None, "no_output"), (None, "unknown_terminal"), (None, "unknown_terminal")]


def test_runtime_death_downgrades_claimed_success():
    # The runtime watched the process die; a self-reported SUCCESS cannot
    # certify it. The observation outranks the claim.
    orch = _load_orch()
    pair = orch.classify_worker_return(
        '```json\n{"result":"SUCCESS","task":"a"}\n```', runtime_terminal="signal"
    )
    assert pair == ("a", "failed")


def test_unrecognized_runtime_terminal_is_failure_class():
    # Unknown terminal reason is a failure, never partial output as success.
    orch = _load_orch()
    assert orch.classify_worker_return(
        '```json\n{"result":"SUCCESS","task":"a"}\n```', runtime_terminal="something_new"
    ) == ("a", "failed")
    assert orch.classify_worker_return("partial text", runtime_terminal="something_new") == (
        None,
        "runtime_failed",
    )


def test_runtime_terminal_from_exit_code_maps_shell_convention():
    orch = _load_orch()
    f = orch.runtime_terminal_from_exit_code
    assert f(0) == "completed"
    assert f(1) == "error"
    assert f(143) == "signal"  # SIGTERM
    assert f(137) == "signal"  # SIGKILL
    assert f(-9) == "signal"  # subprocess spelling of a signal death
    # The producer mapping feeds the classifier end-to-end: a signal death
    # downgrades a claimed SUCCESS observed through the exit code alone.
    assert orch.classify_worker_return(
        '```json\n{"result":"SUCCESS","task":"a"}\n```', runtime_terminal=f(143)
    ) == ("a", "failed")


def test_end_to_end_mixed_bag_blocks_synthesis():
    outputs = [
        '```json\n{"result":"SUCCESS","task":"a"}\n```',   # completed
        '```json\n{"result":"SUCCESS","task":"a"}\n```',   # duplicate
        '```json\n{"result":"FAILED","task":"b"}\n```',    # failed
        "garbage output",                                     # malformed (unknown_terminal)
        # expected id "c" never returns                       -> missing
    ]
    observed = _classify_many(outputs)
    t = tally_fan_in(["a", "b", "c"], observed)
    assert t.expected == 3
    assert t.completed == 1
    assert t.failed == 1
    assert t.duplicate == 1
    assert t.unknown_terminal == 1 and t.malformed == 0
    assert t.missing == 1
    assert t.complete is False


def test_end_to_end_runtime_death_names_the_cause():
    # A crashed worker and a garbage-emitting worker are distinguishable: the
    # crash lands in runtime_failed, the garbage in malformed.
    orch = _load_orch()
    observed = [
        ("a", "completed"),
        orch.classify_worker_return("killed mid-run", runtime_terminal="signal"),
        orch.classify_worker_return("random prose"),
    ]
    t = tally_fan_in(["a", "b"], observed)
    assert t.runtime_failed == 1
    assert t.unknown_terminal == 1 and t.malformed == 0
    assert t.missing == 1
    assert t.complete is False
