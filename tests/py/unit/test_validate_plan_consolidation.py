"""Validator consolidation-block gate: negative and grandfather paths.

The happy path lives in the stub-marker and project-warning fixtures. These
tests pin the failure modes a silent gate would miss: a new plan with no
block, a legacy plan grandfathered to a warn, an undatable plan, and an
outcome with no recorded decision.
"""
from __future__ import annotations

import subprocess
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
VALIDATOR = REPO_ROOT / "scripts" / "validate-plan.sh"


def _run(plan: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(VALIDATOR), str(plan)],
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
    )


def _plan(frontmatter: str) -> str:
    # The full-body shape the execution-contract leg accepts (mirrors the
    # stub-marker suite's _FILLED fixture), so these tests isolate the
    # consolidation gate rather than re-tripping the execution contract.
    return (
        f"---\n{frontmatter}---\n\n# T\n\n"
        "## Context\n\nReal context here.\n\n"
        "## Changes\n\nAdd the guard in foo.py.\n\n"
        "## Files to Modify\n\n- foo.py\n\n"
        "## Verification\n\npytest cli/tests\n"
    )


def test_new_plan_without_block_errors(tmp_path):
    plan = tmp_path / "new.md"
    plan.write_text(_plan("title: T\nstatus: ready\nkind: quick-plan\ncreated: 2026-08-18\n"))
    result = _run(plan)
    assert result.returncode == 1, result.stdout
    assert "no consolidation: block" in result.stdout
    # F2: the missing block must not abort the validator - later checks and
    # the summary still run.
    assert "Errors:" in result.stdout


def test_plan_created_on_the_gate_date_is_grandfathered(tmp_path):
    # Strictly-after boundary: a plan created ON the gate date predates the
    # gate reaching its author. Nine live plans carry that date, and erroring
    # on them would refuse every one at /execute time.
    plan = tmp_path / "on-gate.md"
    plan.write_text(_plan("title: T\nstatus: ready\nkind: quick-plan\ncreated: 2026-08-17\n"))
    result = _run(plan)
    assert result.returncode == 0, result.stdout
    assert "not after the 2026-08-17 gate" in result.stdout


# -- the `rejected` key (x-6f98): a plan ruling against a foreign node --
