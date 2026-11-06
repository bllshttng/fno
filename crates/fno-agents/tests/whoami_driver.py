#!/usr/bin/env python3
"""The whoami case table and fixture builder.

The parity test (whoami_parity.rs) asks this file for the case table
(--list) and builds each case's fixture through it (--fixture); the native
binary runs the cases itself. The goldens froze the Python leg's bytes
through this file's old --case/--capture modes; that leg left with the
residue port, and the frozen goldens are the contract now (a golden can
only be captured while the oracle leg runs). The parity test floors the
child env to HOME and PATH plus each case's stamps, so every
identity-bearing enrichment (model, context, role, registry agent name,
mail unread) answers from the stamps alone. Stamps name ONE harness family
per case, and the one ambiguity case contradicts a single family with two
ids: the process walk proves a family, never a value, so those two shapes
answer identically under any ancestry.
"""
from __future__ import annotations

import json
import sys
from dataclasses import dataclass, field
from pathlib import Path

# --- fixture bodies (the deleted cli/tests/unit/fixtures/agent shapes) ----

TARGET_STATE = """---
status: IN_PROGRESS
current_phase: review
iteration: 3
session_id: 20260512T010101Z-99999-fixaaa
provenance_nonce: deadbeefcafef00d
pr_number: 1234
quality_check_passed: true
output_validated: false
artifact_shipped: false
---
# Target Session State (fixture)
"""

MALFORMED_STATE = """---
status: IN_PROGRESS
session_id: 20260512T010101Z-99999-malf01
this: is: not: valid: yaml: at: all
  - because
   indentation: [is broken
# missing closing delimiter intentionally
"""

SESSION_STATE = """---
status: IN_PROGRESS
current_phase: think
session_id: 20260511T235959Z-77777-thinkc
---
# Session State (planning fixture)
"""

RESIDENT_STATE = """---
session_id: resident-run
harness_session_id: resident-session
input: resident-work
plan_path: /tmp/resident-plan.md
---
"""

INCONSISTENT_STATE = """---
status: IN_PROGRESS
session_id: 20260512T010101Z-99999-fixaaa
current_phase: external
pr_number: 1234
external_review_passed: false
---
# fixture
"""

NOSTATUS_STATE = """---
session_id: 20260512T010101Z-99999-fixaaa
current_phase: review
---
# fixture
"""


def _events(rows: list[dict], partial: str) -> str:
    body = "".join(json.dumps(row) + "\n" for row in rows)
    return body + partial


PLAIN_EVENTS = _events(
    [{"ts": "T1", "type": "phase_init"}, {"ts": "T2", "type": "phase_transition"}],
    '{"ts":"T3","type":"phase_truncated_marker_xyz',
)

TERMINATION_EVENTS = _events(
    [
        {"ts": "T1", "type": "phase_init"},
        {
            "ts": "T2",
            "type": "termination",
            "data": {
                "session_id": "20260512T010101Z-99999-fixaaa",
                "termination_reason": "DonePRGreen",
            },
        },
    ],
    '{"ts":"T3","type":"phase_truncated_marker_xyz',
)


@dataclass(frozen=True)
class Case:
    name: str
    argv: tuple[str, ...]
    files: dict[str, str] = field(default_factory=dict)
    env: dict[str, str] = field(default_factory=dict)


CLAUDE_STAMP = {"CLAUDE_CODE_SESSION_ID": "879d8d26-2505-4977-9b87-000000000000"}

FULL_STACK = {
    ".fno/target-state.md": TARGET_STATE,
}

CASES: tuple[Case, ...] = (
    Case("bare_full_stack", ("whoami",), FULL_STACK),
    Case("bare_full_stack_json", ("whoami", "--json"), FULL_STACK),
    Case("bare_full_stack_global_json", ("-J", "whoami"), FULL_STACK),
    Case("bare_empty_project", ("whoami",), {}),
    Case("bare_empty_project_json", ("whoami", "--json"), {}),
    Case(
        "bare_malformed_state",
        ("whoami",),
        {".fno/target-state.md": MALFORMED_STATE},
    ),
    Case(
        "bare_state_file_missing_override",
        ("whoami", "--state-file", "<MISSING>"),
        {".fno/target-state.md": TARGET_STATE},
    ),
    Case(
        "bare_mail_and_mesh",
        ("whoami",),
        {".fno/target-state.md": TARGET_STATE},
        dict(CLAUDE_STAMP, FNO_AGENT_SELF="t-env-worker"),
    ),
    Case(
        "bare_mail_json",
        ("whoami", "--json"),
        {".fno/target-state.md": TARGET_STATE},
        dict(CLAUDE_STAMP),
    ),
    Case(
        "bare_visitor_mismatch",
        ("whoami",),
        {".fno/target-state.md": RESIDENT_STATE},
        {"CLAUDE_CODE_SESSION_ID": "visitor-session"},
    ),
    Case(
        "bare_visitor_mismatch_json",
        ("whoami", "--json"),
        {".fno/target-state.md": RESIDENT_STATE},
        {"CLAUDE_CODE_SESSION_ID": "visitor-session"},
    ),
    Case(
        "bare_provider_route",
        ("whoami",),
        {".fno/target-state.md": TARGET_STATE},
        {"FNO_ROUTE_PROVIDER": "zai"},
    ),
    Case(
        "bare_session_fallback",
        ("whoami",),
        {".fno/session-state.md": SESSION_STATE},
    ),
    Case(
        "bare_both_states_warning",
        ("whoami",),
        {".fno/target-state.md": TARGET_STATE, ".fno/session-state.md": SESSION_STATE},
    ),
    Case(
        "bare_identity_ambiguous",
        ("whoami",),
        {".fno/target-state.md": TARGET_STATE},
        {"CODEX_THREAD_ID": "thread-abc", "CODEX_SESSION_ID": "root-session-xyz"},
    ),
    Case(
        "status_gates_events",
        ("whoami", "status"),
        {
            ".fno/target-state.md": TARGET_STATE,
            ".fno/events.jsonl": PLAIN_EVENTS,
        },
    ),
    Case(
        "status_gates_events_json",
        ("whoami", "status", "--json"),
        {
            ".fno/target-state.md": TARGET_STATE,
            ".fno/events.jsonl": PLAIN_EVENTS,
        },
    ),
    Case(
        "status_inconsistency",
        ("whoami", "status"),
        {".fno/target-state.md": INCONSISTENT_STATE},
    ),
    Case(
        "status_derived_termination",
        ("whoami", "status"),
        {
            ".fno/target-state.md": NOSTATUS_STATE,
            ".fno/events.jsonl": TERMINATION_EVENTS,
        },
    ),
    Case("status_empty_project", ("whoami", "status"), {}),
    Case("help_group", ("whoami", "--help"), {}),
    Case("help_status", ("whoami", "status", "--help"), {}),
    Case("unknown_leaf", ("whoami", "badleaf"), {}),
)


def build_fixture(case: Case, root: Path) -> Path:
    """Materialize the case's project (and fake home) under root."""
    project = root / "project"
    (project / ".fno").mkdir(parents=True)
    for rel, body in case.files.items():
        path = project / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")
    # A seeded state root, not a fresh one: the paths bootstrap answers a
    # first-run migration with its own setup notice and a settings write.
    # That notice is the state layer's, not whoami's, so the fixture seeds
    # the settings file and sentinel a migrated root already has (the same
    # shape paths_testing.use_tmpdir writes).
    state = root / "fake_home" / ".fno"
    state.mkdir(parents=True, exist_ok=True)
    (state / "settings.yaml").write_text(
        f"schema_version: 1\nconfig:\n  state_dir: {state}/\n", encoding="utf-8"
    )
    (state / ".path-migration-done").touch()
    return project


def main() -> None:
    if len(sys.argv) >= 4 and sys.argv[1] == "--fixture":
        case = next(c for c in CASES if c.name == sys.argv[2])
        build_fixture(case, Path(sys.argv[3]))
        return
    if len(sys.argv) == 2 and sys.argv[1] == "--list":
        print(
            json.dumps(
                [
                    {
                        "name": c.name,
                        "argv": list(c.argv),
                        "env": dict(c.env),
                    }
                    for c in CASES
                ]
            )
        )
        return
    sys.exit("usage: whoami_driver.py --list | --fixture NAME ROOT")


if __name__ == "__main__":
    main()
