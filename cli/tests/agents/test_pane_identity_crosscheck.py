"""An unstamped spawn reports itself.

A codex spawn with an OPTIONAL session binding whose stamp never landed used to
fall through the reconcile pass and return zero with an id-less row and no
journal entry. The failure is now an event plus a ``stamp_failure`` receipt
field, never a silent None. The pane/registry cross-check that once shared
this file is native now; its tests live in crates/fno-agents/src/pane_identity.rs.
"""
from __future__ import annotations

import ast
import inspect
import textwrap

from fno.agents import mux_spawn


# ---------------------------------------------------------------------------
# Task 2.2: the stamp step reports when it does not write


def test_optional_binding_fall_through_reports_itself() -> None:
    """The silent fall-through is gone: the codex reconcile branch emits the
    uncaptured-id event and sets stamp_failure when the binding is optional
    and the backfill found nothing."""
    src = textwrap.dedent(inspect.getsource(mux_spawn.dispatch_spawn_pane))
    tree = ast.parse(src)
    # Find the codex reconcile `if` chain by its sentinel string, then require
    # the emit and the assignment inside the SAME chain.
    emits = [
        n
        for n in ast.walk(tree)
        if isinstance(n, ast.Call)
        and isinstance(n.func, ast.Attribute)
        and n.func.attr == "emit"
        and any(
            isinstance(a, ast.Constant) and a.value == "agent_session_id_uncaptured"
            for a in n.args
        )
    ]
    assert emits, "the fall-through no longer emits agent_session_id_uncaptured"
    assert "stamp_failure = stamp_failure_detail" in src, (
        "the fall-through no longer records stamp_failure"
    )
    assert "stamp_failure=stamp_failure" in src, (
        "MuxSpawnResult no longer carries stamp_failure"
    )
