"""Loop participation: can the fno target loop CLOSE on this harness?

The field replaced ``stop_hook``, which read ``native`` on every row and had no
consumer, so nothing here would have noticed when it stopped being true. These
tests are what makes the new field load-bearing: the artifact behind an
``extension`` row, and the refusal that stops a looping dispatch from producing
a worker with nothing to stop it.

The decision itself lives in ``capability_leaves.rs`` (the ``fno-agents status
--target-family --harness`` leaf); the Rust test module carries the refusal
table, and the Rust spawn admission calls it. What stays here is the artifact
check.

Every measurement here reads the capability-backed roster (``known_harnesses``),
never the complete ``KNOWN_HARNESSES`` roster: hermes and openclaw are supported
identities with no capability row, and loop participation is a property of the
row, so there is nothing to measure for a row-less harness.
"""
from __future__ import annotations

from pathlib import Path

from fno.agents.harness_map import capabilities, known_harnesses

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
