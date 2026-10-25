#!/usr/bin/env python3
"""Live oracle for the whoami family port (step 2 of the port protocol,
docs/architecture/dual-implementation-inventory.md).

Two modes:

  --case NAME ROOT  build case NAME's fixture under ROOT, run the Python
                    whoami leg there through the real top-level app, and
                    relay (stdout, stderr, exit code) byte-verbatim. The
                    parity test invokes this under FNO_CAPTURE_GOLDEN=1 to
                    freeze goldens from the old leg on the same fixture the
                    native side runs.
  --capture DIR     run every case in a fresh tempdir and freeze the
                    normalized (stdout, stderr, exit) triples into DIR.
                    Refuses once cli/src/fno/agent is deleted: a golden can
                    only be captured while the old leg runs.

The case table below is the single source both legs share. Identity env is
scrubbed then stamped per case, so every identity-bearing enrichment (model,
context, role, registry agent name, mail unread) floors to absent except the
lines a case stamps explicitly. Stamps name ONE harness family per case, and
the one ambiguity case contradicts a single family with two ids: the process
walk proves a family, never a value, so those two shapes answer identically
under any ancestry.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "cli" / "src"))

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

WALKER_STATE = """---
status: LOOPING
current_phase: dispatching
session_id: 20260512T000000Z-88888-walkbb
in_flight: 3
done: 1
---
# Megawalk Walker State (fixture)
"""

SESSION_STATE = """---
status: IN_PROGRESS
current_phase: think
session_id: 20260511T235959Z-77777-thinkc
---
# Session State (planning fixture)
"""

FLEET_MISSION = """---
mission_id: fleet-fixture-001
title: "Fixture mission"
status: running
wave_current: 2
wave_total: 4
projects:
  footnote:
    cwd: __PROJECT_ROOT__
---
# Fleet Mission (fixture)
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
    fleet: bool = False


CLAUDE_STAMP = {"CLAUDE_CODE_SESSION_ID": "879d8d26-2505-4977-9b87-000000000000"}

FULL_STACK = {
    ".fno/target-state.md": TARGET_STATE,
    ".fno/megawalk-state.md": WALKER_STATE,
}

CASES: tuple[Case, ...] = (
    Case("bare_full_stack", ("whoami",), FULL_STACK, fleet=True),
    Case("bare_full_stack_json", ("whoami", "--json"), FULL_STACK, fleet=True),
    Case("bare_full_stack_global_json", ("-J", "whoami"), FULL_STACK, fleet=True),
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
        "bare_suppressed_layers",
        ("whoami", "--no-walker", "--no-fleet"),
        FULL_STACK,
        fleet=True,
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
)


def scrubbed_env(home: Path, stamps: dict[str, str]) -> dict[str, str]:
    """The host env minus every name the render reads for identity, plus the
    case stamps and a fixture-scoped HOME. The scrub list is read from the
    same tables the resolver reads, so a new marker cannot leak into a
    golden."""
    from fno.harness_identity import (
        AMBIENT_IDENTITY_ENV,
        HARNESS_SESSION_MARKERS,
        LEGACY_HARNESS_SESSION_MARKERS,
    )

    env = dict(os.environ)
    for marker, _family in (*HARNESS_SESSION_MARKERS, *LEGACY_HARNESS_SESSION_MARKERS):
        env.pop(marker, None)
    # The ambient table is wider than the markers (companion/transcript
    # lineage names a hosted session inherits); every one of them is
    # identity-adjacent, so all of them go.
    for name in AMBIENT_IDENTITY_ENV:
        env.pop(name, None)
    for name in (
        "FNO_AGENT_SELF",
        "FNO_WORKER_NAME",
        "FNO_ROUTE_PROVIDER",
        "FNO_HARNESS_SESSION_ID",
        "FNO_INBOX_ROOT",
        "FNO_BUS_DIR",
        "FNO_CONFIG",
        "FNO_STATE_DIR",
        "FNO_CODEX_SESSIONS_DIR",
        "FNO_TEST_HERMETIC",
    ):
        env.pop(name, None)
    env["HOME"] = str(home)
    env["PYTHONPATH"] = str(REPO / "cli" / "src")
    env.update(stamps)
    return env


def build_fixture(case: Case, root: Path) -> Path:
    """Materialize the case's project (and fake home) under root."""
    project = root / "project"
    (project / ".fno").mkdir(parents=True)
    for rel, body in case.files.items():
        path = project / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")
    if case.fleet:
        fleet_root = root / "fake_home" / ".fno" / "fleet" / "fleet-fixture-001"
        fleet_root.mkdir(parents=True)
        (fleet_root / "00-INDEX.md").write_text(
            FLEET_MISSION.replace("__PROJECT_ROOT__", str(project.resolve())),
            encoding="utf-8",
        )
    # A seeded state root, not a fresh one: the paths bootstrap answers a
    # first-run migration with its own setup notice and a settings write.
    # That notice is the state layer's, not whoami's, so the fixture seeds
    # the settings file and sentinel a migrated root already has (the same
    # shape paths_testing.use_tmpdir writes) and fleet_dir() still resolves
    # to fake_home/.fno/fleet.
    state = root / "fake_home" / ".fno"
    state.mkdir(parents=True, exist_ok=True)
    (state / "settings.yaml").write_text(
        f"schema_version: 1\nconfig:\n  state_dir: {state}/\n", encoding="utf-8"
    )
    (state / ".path-migration-done").touch()
    return project


def run_case(case: Case, root: Path) -> tuple[str, str, int]:
    """Run one case in this process: build the fixture, chdir, invoke the
    real app, return (stdout, stderr, exit code)."""
    project = build_fixture(case, root)
    env = scrubbed_env(root / "fake_home", case.env)
    os.environ.clear()
    os.environ.update(env)
    os.chdir(project)
    from typer.testing import CliRunner

    from fno.cli import app

    argv = [str(root / "does-not-exist.md") if a == "<MISSING>" else a for a in case.argv]
    result = CliRunner().invoke(app, argv)
    return result.stdout, result.stderr, result.exit_code


def normalize(text: str, root: Path) -> str:
    """Fold what the host owns, never what the contract owns: the fixture
    path (in resolved and raw spellings, and macOS's /private prefix), and
    the PyYAML parser detail in the malformed-state refusal, whose wording
    is the YAML library's, not the command's."""
    resolved = str(root.resolve())
    for token in (f"/private{resolved}", resolved, str(root)):
        text = text.replace(token, "<FIXTURE>")
    marker = "error: malformed state file: "
    idx = text.find(marker)
    if idx != -1:
        anchor = text.find("<FIXTURE>", idx)
        if anchor != -1:
            tail = text.find(": ", anchor)
            if tail != -1:
                text = text[:tail] + ": <REASON>\n"
    return text


def capture(golden_dir: Path) -> None:
    """Freeze every case from the live leg. Refuses when the leg is gone."""
    try:
        import fno.agent.state  # noqa: F401
    except ImportError:
        sys.exit("capture refused: the Python whoami leg is deleted")
    golden_dir.mkdir(parents=True, exist_ok=True)
    for case in CASES:
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            proc = subprocess.run(
                [sys.executable, str(Path(__file__).resolve()), "--case", case.name, str(root)],
                capture_output=True,
                text=True,
            )
        out = normalize(proc.stdout, root)
        err = normalize(proc.stderr, root)
        (golden_dir / f"{case.name}.out").write_text(out, encoding="utf-8")
        (golden_dir / f"{case.name}.err").write_text(err, encoding="utf-8")
        (golden_dir / f"{case.name}.exit").write_text(f"{proc.returncode}\n", encoding="utf-8")
        print(f"{case.name}: exit {proc.returncode}, {len(out)}B out, {len(err)}B err")


def main() -> None:
    if len(sys.argv) >= 4 and sys.argv[1] == "--case":
        out, err, code = run_case(
            next(c for c in CASES if c.name == sys.argv[2]), Path(sys.argv[3])
        )
        sys.stdout.write(out)
        sys.stderr.write(err)
        sys.exit(code)
    if len(sys.argv) == 3 and sys.argv[1] == "--capture":
        capture(Path(sys.argv[2]))
        return
    sys.exit("usage: whoami_driver.py --case NAME ROOT | --capture GOLDEN_DIR")


if __name__ == "__main__":
    main()
