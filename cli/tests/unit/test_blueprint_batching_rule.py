"""x-c1c7: batch blueprints, up to three per session, one plan per shape.

Two checks, because they answer different questions.

Static, blocking: the lead skill's dispatch step states the number (a rule
without a number is advice nobody applies), and the unplanned board queue
carries the batching note at the exact moment a lead picks what to dispatch.
Both assert a positive marker, never an absence.

Behavioral, advisory: `evals/bank/capability-blueprint-batching.yaml` hands a
fresh worker five nodes, two sharing a shape, with no operator guidance. The
worker writes one line per intended blueprint dispatch to `dispatch-plan.md`.
This grader only runs when that artifact exists, and asserts exactly three
lines with the shape pair on one of them - a worker that emits five lines has
not received the rule.
"""
from __future__ import annotations

import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
LEAD_SKILL = REPO_ROOT / "skills" / "lead" / "references" / "once.md"


def _section_3d(text: str) -> str:
    match = re.search(
        r"\*\*3d\..*?(?=\n### 4\. Kick off)", text, re.S
    )
    assert match, "3d batching block not found in lead/references/once.md"
    return match.group(0)


def test_3d_states_the_number_three():
    section = _section_3d(LEAD_SKILL.read_text())
    assert re.search(r"\bthree\b|\b3\b", section, re.I), (
        "3d block names no count; a rule without a number is advice nobody applies"
    )


# The unplanned batching note and the undispatched command/note invariants moved
# with build_board into the Rust collector (x-25b8): lead_board.rs
# unplanned_note_names_the_batch_and_undispatched_names_the_target covers them.
