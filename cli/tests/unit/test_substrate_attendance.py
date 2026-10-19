"""The spawner stamps the substrate; nothing downstream guesses it (x-be78).

`attended` used to be derived from `FNO_AGENT_SELF` presence. Every spawn
substrate injects that variable, pane included, and pane is the default, so a
context-warm pane worker with an operator watching stamped `attended: false`.
The same guess labelled every headless one-shot a `pane` in the review-gate
refusal text.

These tests pin the one rule and the four places that used to repeat it.
"""

from __future__ import annotations

from fno.harness_identity import (
    AMBIENT_IDENTITY_ENV,
    ATTENDED_SUBSTRATES,
    FNO_AGENT_SUBSTRATE,
    env_marks_unattended,
    stamp_child_harness_identity,
)

SPAWNED = {"FNO_AGENT_SELF": "target-x-be78"}


def test_attendance_reads_the_substrate_not_the_mesh_identity():
    assert env_marks_unattended({**SPAWNED, FNO_AGENT_SUBSTRATE: "pane"}) is False
    assert env_marks_unattended({**SPAWNED, FNO_AGENT_SUBSTRATE: "thread"}) is True
    assert env_marks_unattended({**SPAWNED, FNO_AGENT_SUBSTRATE: "headless"}) is True
    # An operator's own shell carries no mesh identity at all.
    assert env_marks_unattended({}) is False
    # A spawn path that stamps nothing keeps the pre-x-be78 answer. It fails
    # toward skipping a blocking prompt rather than hanging on one.
    assert env_marks_unattended(SPAWNED) is True
    assert ATTENDED_SUBSTRATES == frozenset({"pane"})
    # FNO_BG is read for its value. Nothing in the tree sets it, so an exported
    # `FNO_BG=0` came from someone saying "not a bg run".
    assert env_marks_unattended({"FNO_BG": "1"}) is True
    assert env_marks_unattended({"FNO_BG": "0"}) is False
    assert env_marks_unattended({"FNO_BG": "false"}) is False


def test_stamp_child_scrubs_the_parents_substrate_before_stamping():
    """A pane worker passes its whole env to any one-shot it launches. Without
    the scrub the child reports `pane` for life and reads as attended."""
    assert FNO_AGENT_SUBSTRATE in AMBIENT_IDENTITY_ENV

    inherited = {FNO_AGENT_SUBSTRATE: "pane", "FNO_AGENT_SELF": "the-pane"}
    child = stamp_child_harness_identity(
        dict(inherited), "claude", agent_self="the-one-shot", substrate="headless"
    )
    assert child[FNO_AGENT_SUBSTRATE] == "headless"

    # A launcher that does not know its substrate leaves the child unknown
    # rather than letting the parent's value ride through.
    silent = stamp_child_harness_identity(dict(inherited), "claude", agent_self="c")
    assert FNO_AGENT_SUBSTRATE not in silent


def test_pane_spawn_argv_stamps_pane_after_the_ambient_scrub():
    """env(1) applies every -u before the assignments, so the pane wrapper
    carries both and the assignment is the one that survives."""
    from fno.agents.mux_spawn import _mesh_env_wrapper

    argv = _mesh_env_wrapper(name="w1", harness="codex", role=None, argv=["codex"])
    unset = [argv[i + 1] for i, tok in enumerate(argv) if tok == "-u"]
    assert "FNO_AGENT_SUBSTRATE" in unset
    assert "FNO_AGENT_SUBSTRATE=pane" in argv
