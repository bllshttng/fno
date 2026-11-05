#!/usr/bin/env python3
"""Tests for impeccable_stages validator in validate-plan.sh (Phase 02.2).

Acceptance criteria:
  AC1-HP:  impeccable_stages: [craft, critique, harden] -> passes silently
  AC3-ERR: impeccable_stages: [craft, foo, harden] -> exit 1, names 'foo'
  AC4-EDGE: impeccable_stages: [] -> exit 1, "empty list"

Run: python3 -m pytest tests/spec/test_impeccable_stages_validator.py -v
"""
import subprocess
import textwrap
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
VALIDATOR = REPO_ROOT / "scripts" / "validate-plan.sh"


def _make_plan(plan_path: Path, stage_lines: str = "") -> None:
    """Write a minimal valid single-doc plan for the validator."""
    plan_path.write_text(
        textwrap.dedent(f"""\
        ---
        title: Test Plan
        scope: feature
        project: test
        # Pre-gate date: the surface gate refuses an undated plan, and these
        # tests exercise the impeccable_stages rules, not the gates.
        created: 2026-08-01
        execution_mode: sequential
        waves:
          - wave: 1
            mode: sequential
            tasks: [1.1]
        ---

        # Test Plan

        ## Critical Path Trace

        User opens page -> Task 1.1 shows result

        ## Scope Classification

        ```yaml
        scope: feature
        ```

        ### Task 1.1: Build Component

        **Files:** `src/components/Foo.tsx`

        Steps:
        1. Implement component

        Acceptance Criteria:
        - AC1-HP: Given..., when..., then...

        {stage_lines}
        """)
    )


def _run_validator(plan_path: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", str(VALIDATOR), str(plan_path)],
        capture_output=True,
        text=True,
    )


def test_ac1_hp_valid_standard_stages():
    """AC1-HP: [craft, critique, harden] -> validator passes."""
    with tempfile.TemporaryDirectory() as tmp:
        plan_path = Path(tmp) / "my-plan.md"
        _make_plan(plan_path, "impeccable_stages: [craft, critique, harden]")

        result = _run_validator(plan_path)

        assert result.returncode == 0, (
            f"Expected exit 0 for valid stages.\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
        # Section header should appear
        assert "impeccable_stages" in result.stdout


def test_ac3_err_unknown_stage_exits_1():
    """AC3-ERR: [craft, foo, harden] -> exit 1, message names 'foo'."""
    with tempfile.TemporaryDirectory() as tmp:
        plan_path = Path(tmp) / "my-plan.md"
        _make_plan(plan_path, "impeccable_stages: [craft, foo, harden]")

        result = _run_validator(plan_path)

        assert result.returncode == 1, (
            f"Expected exit 1 for unknown stage.\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
        combined = result.stdout + result.stderr
        assert "foo" in combined.lower(), f"Expected 'foo' in output. Got:\n{combined}"
