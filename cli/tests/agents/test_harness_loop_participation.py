"""Loop participation: can the fno target loop CLOSE on this harness?

The field replaced ``stop_hook``, which read ``native`` on every row and had no
consumer, so nothing here would have noticed when it stopped being true. These
tests are what makes the new field load-bearing: the artifact behind an
``extension`` row, and the refusal that stops a looping dispatch from producing
a worker with nothing to stop it.

The decision itself lives in ``capability_leaves.rs`` (the ``fno-agents status
--target-family --harness`` leaf); the Rust test module carries the refusal
table. What stays here is the spawn seam that calls the gate, the fail-closed
unreadable leaf, and one end-to-end door test on a dev build.

Every measurement here reads the capability-backed roster (``known_harnesses``),
never the complete ``KNOWN_HARNESSES`` roster: hermes and openclaw are supported
identities with no capability row, and loop participation is a property of the
row, so there is nothing to measure for a row-less harness.
"""
from __future__ import annotations

from pathlib import Path

import pytest

import fno.agents.harness_map as harness_map
from fno.agents.harness_map import (
    DispatchResolveError,
    capabilities,
    check_loop_participation,
    known_harnesses,
)
from fno.rust_binary import find_dev_binary

requires_rust = pytest.mark.dev_build

# Captured at import time, before the autouse ``_hermetic_loop_gate``
# conftest fixture stubs the module attribute: the door test re-binds the
# real leaf answer so the real binary is driven end to end.
_REAL_LOOP_GATE_ANSWER = harness_map._loop_gate_answer

REPO_ROOT = Path(__file__).resolve().parents[3]


def test_a_declared_loop_extension_exists_on_disk():
    """An ``extension`` row's artifact is the thing that closes its loop.

    Declaring a path that has been deleted or renamed is exactly the class of
    false-but-parseable value this field exists to remove, so the claim is
    checked against the tree rather than trusted.
    """
    named = {
        h: capabilities(h)["loop_extension"]
        for h in known_harnesses()
        if capabilities(h)["loop_extension"]
    }
    assert named, "no harness declares a loop extension; the check would be vacuous"
    for harness, rel in named.items():
        assert (REPO_ROOT / rel).is_file(), f"{harness} names a missing artifact: {rel}"


def test_a_non_looping_dispatch_is_never_refused():
    """The gate is scoped to the /target family, so a one-shot passes untouched.

    A harness that cannot close a loop can still run research, a review, or any
    command that ends on its own.
    """
    harness = "gemini"  # loop_participation = none: the row the gate refuses
    check_loop_participation(harness, "/think what breaks here")
    check_loop_participation(harness, "opencode run --command build")
    check_loop_participation(harness, "")
    # A whitespace-only message has no first token. It used to raise IndexError
    # out of the shared family test, which three callers reach.
    check_loop_participation(harness, "   ")
    check_loop_participation(harness, "\t\n")


def test_an_unreadable_leaf_refuses_never_admits(monkeypatch):
    """A missing or older binary must fail closed: an unreadable gate answers
    a refusal, so no harness is waved through on a broken door."""
    import fno.rust_binary

    def _missing(verb, args, **kwargs):
        return ("fno-agents binary not found", None)

    monkeypatch.setattr(fno.rust_binary, "call_binary_json", _missing)
    # The autouse hermetic stub answers without the transport; re-bind the
    # real answer so the stubbed transport is what the caller drives.
    monkeypatch.setattr(harness_map, "_loop_gate_answer", _REAL_LOOP_GATE_ANSWER)
    with pytest.raises(DispatchResolveError) as exc:
        check_loop_participation("gemini", "/target x-1")
    message = str(exc.value)
    assert "could not be read" in message
    assert "fno doctor update --rust" in message


@requires_rust
def test_the_real_door_refuses_a_looping_dispatch_at_gemini(monkeypatch):
    """End to end on the dev build: the leaf reads the packaged row (gemini:
    none) and the caller raises its ``never stop`` refusal. This is the door
    wiring test - a stubbed seam cannot catch a broken argument list."""
    binary = find_dev_binary()
    assert binary is not None
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))
    monkeypatch.setattr(harness_map, "_loop_gate_answer", _REAL_LOOP_GATE_ANSWER)
    with pytest.raises(DispatchResolveError) as exc:
        check_loop_participation("gemini", "/target x-1")
    assert "never stop" in str(exc.value)
