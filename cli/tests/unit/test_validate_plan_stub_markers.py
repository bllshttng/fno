"""Validator refuses to pass an unfilled decompose-child scaffold (x-edf7 US1/US4).

A `blueprint decompose` child is born `status: stub` with placeholder markers +
an empty-why sentinel; linking its plan_path flips it `ready` and dispatchers
launch against it. The validator is the gate: it must reject any plan still
carrying a stub marker (naming it) and require a non-empty `## Why (from epic)`
on group-child plans. The scaffold writer lives in the native verb now
(crates/fno-agents/src/backlog/decompose.rs); the fixtures below carry the
same markers by hand so the validator contract stays pinned.
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
VALIDATOR = REPO_ROOT / "skills" / "blueprint" / "scripts" / "validate-plan.sh"


def _run(
    plan: Path, *, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(VALIDATOR), str(plan)],
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        env=env,
    )


_FILLED = """---
title: Group 1
status: ready
kind: quick-plan
parent_epic: ab-epic0001
source_doc: big.md
consolidation:
  outcome: proceed_alone
  proceed_alone_against: []
---

# Group 1

## Why (from epic)

Gate launch on a real plan so dispatchers stop stampeding stub scaffolds.

## Context

Real context here.

## Changes

Add the guard in foo.py.

## Files to Modify

- foo.py

## Verification

pytest cli/tests
"""


_UNFILLED = """---
title: "Group 1"
status: idea
kind: quick-plan
created: 2026-10-15
difficulty: low
consolidation:
  # <!-- Consolidation: run the step 2d gate against this child and
  #      record one outcome with a reason per id considered. -->
  outcome: proceed_alone
  proceed_alone_against: []
parent_epic: ab-epic0001
source_doc: big.md
---

# Group 1

## Why (from epic)

<!-- Why (from epic): transcribe the epic's intent line + the Locked
Decisions that bind this child (not a pointer). -->

## Context

Group child of epic `ab-epic0001` (see `big.md`). Covers wave(s) 1-2 of the
epic's Execution Strategy.

## Changes

<!-- Seeded from epic waves 1-2. Fill in the concrete changes this group ships. -->

## Files to Modify

<!-- From the epic's File Ownership Map: the files this group owns. -->

## Verification

<!-- The checks that prove this group's slice works. -->
"""


def test_validator_rejects_unfilled_scaffold(tmp_path):
    plan = tmp_path / "big.group-1.md"
    plan.write_text(_UNFILLED)
    result = _run(plan)
    assert result.returncode == 1, result.stdout
    assert "unfilled stub marker" in result.stdout
    assert "Seeded from epic waves" in result.stdout  # marker is named


def test_validator_flags_empty_why_on_group_child(tmp_path):
    # A group child (parent_epic set) whose ## Why body is empty is rejected even
    # if the other markers were cleared.
    plan = tmp_path / "big.group-1.md"
    plan.write_text(
        "---\ntitle: G\nparent_epic: ab-epic0001\n---\n\n"
        "# G\n\n## Why (from epic)\n\n## Changes\n\nreal\n"
    )
    result = _run(plan)
    assert result.returncode == 1, result.stdout
    assert "empty '## Why (from epic)'" in result.stdout


def test_validator_rejects_a_pre_design_rung(tmp_path):
    """A filled plan that forgot to flip its status to `ready` is rejected.

    The `idea` rung is undispatchable, so nothing would ever pick this child up.
    """
    plan = tmp_path / "big.group-1.md"
    plan.write_text(
        "---\ntitle: G\nstatus: idea\nparent_epic: ab-epic0001\n---\n\n"
        "# G\n\n## Why (from epic)\n\nreal why\n\n## Changes\n\nreal\n"
    )
    result = _run(plan)
    assert result.returncode == 1, result.stdout
    assert "'idea' rung" in result.stdout


def test_validator_passes_filled_child(tmp_path):
    plan = tmp_path / "big.group-1.md"
    plan.write_text(_FILLED)
    result = _run(plan)
    assert result.returncode == 0, result.stdout
    assert "no unfilled stub markers" in result.stdout
    assert "## Why (from epic) is non-empty" in result.stdout


def test_validator_uses_source_cli_when_fno_is_not_on_path(tmp_path):
    plan = tmp_path / "big.group-1.md"
    plan.write_text(_FILLED)
    env = os.environ.copy()
    env["PATH"] = "/usr/bin:/bin"

    result = _run(plan, env=env)

    assert result.returncode == 0, result.stdout
    assert "semantic execution contract valid" in result.stdout
