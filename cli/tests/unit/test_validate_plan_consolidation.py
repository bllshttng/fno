"""Validator consolidation-block gate: negative and grandfather paths.

The happy path lives in the stub-marker and project-warning fixtures. These
tests pin the failure modes a silent gate would miss: a new plan with no
block, a legacy plan grandfathered to a warn, an undatable plan, and an
outcome with no recorded decision.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

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


def test_a_repeated_outcome_key_is_not_silently_last_wins(tmp_path):
    # PyYAML takes the LAST of a repeated key with no diagnostic, so a block
    # recording two outcomes would parse as one and discard the other.
    plan = tmp_path / "dup.md"
    plan.write_text(
        _plan(
            "title: T\nstatus: ready\nkind: quick-plan\ncreated: 2026-08-18\n"
            "consolidation:\n"
            "  outcome: absorb\n"
            "  outcome: proceed_alone\n"
            "  absorbed:\n"
            "    - id: x-ab12\n"
            "      reason: same lock\n"
        )
    )
    result = _run(plan)
    assert result.returncode == 1, result.stdout
    assert "more than one `outcome:` line" in result.stdout


def test_absorb_with_empty_absorbed_list_errors(tmp_path):
    plan = tmp_path / "empty-absorb.md"
    plan.write_text(
        _plan(
            "title: T\nstatus: ready\nkind: quick-plan\ncreated: 2026-08-18\n"
            "consolidation:\n"
            "  outcome: absorb\n"
            "  absorbed: []\n"
        )
    )
    result = _run(plan)
    assert result.returncode == 1, result.stdout
    assert "absorbed: list is empty" in result.stdout


def test_plan_created_on_the_gate_date_is_grandfathered(tmp_path):
    # Strictly-after boundary: a plan created ON the gate date predates the
    # gate reaching its author. Nine live plans carry that date, and erroring
    # on them would refuse every one at /execute time.
    plan = tmp_path / "on-gate.md"
    plan.write_text(_plan("title: T\nstatus: ready\nkind: quick-plan\ncreated: 2026-08-17\n"))
    result = _run(plan)
    assert result.returncode == 0, result.stdout
    assert "not after the 2026-08-17 gate" in result.stdout


def test_a_plan_the_gate_cannot_date_is_refused_not_grandfathered(tmp_path):
    # The gate fires on a date being present AND after it, so a plan carrying
    # no readable date at all would be grandfathered forever - a silent,
    # permanent opt-out for anyone who just omits the key.
    plan = tmp_path / "junk-date.md"
    plan.write_text(_plan("title: T\nstatus: ready\nkind: quick-plan\ncreated: last tuesday\n"))
    out = _run(plan)
    assert out.returncode == 1, out.stdout
    assert "no readable date" in out.stdout

    missing = tmp_path / "no-created.md"
    missing.write_text(_plan("title: T\nstatus: ready\nkind: quick-plan\n"))
    out = _run(missing)
    assert out.returncode == 1, out.stdout
    assert "no readable date" in out.stdout


def test_a_file_with_no_frontmatter_says_the_gate_did_not_run(tmp_path):
    # A file with no plan header is a different defect with a different owner.
    # The gate cannot date it, so it names that rather than passing silently -
    # a clean-looking gate on an unreadable file is the failure this whole
    # check exists to avoid.
    plan = tmp_path / "headerless.md"
    plan.write_text("# T\n\nno frontmatter at all\n")
    out = _run(plan)
    assert "consolidation gate did not run" in out.stdout
    assert "no --- frontmatter block" in out.stdout
    assert "consolidation outcome is" not in out.stdout


def test_a_dated_filename_dates_a_plan_whose_frontmatter_does_not(tmp_path):
    # Eight live plans carry the date in the name and nothing in frontmatter.
    # The name is real evidence, so it grandfathers them rather than blocking
    # work on plans written months before the gate existed.
    plan = tmp_path / "2026-05-15-old-plan.md"
    plan.write_text(_plan("title: T\nstatus: ready\nkind: quick-plan\n"))
    out = _run(plan)
    assert out.returncode == 0, out.stdout
    assert "not after the 2026-08-17 gate" in out.stdout

    # And the same name shape does NOT excuse a plan written after the gate.
    fresh = tmp_path / "20260820-new-plan.md"
    fresh.write_text(_plan("title: T\nstatus: ready\nkind: quick-plan\n"))
    out = _run(fresh)
    assert out.returncode == 1, out.stdout
    assert "no consolidation: block in frontmatter" in out.stdout


def test_append_outcome_in_a_written_plan_is_a_contradiction(tmp_path):
    # append records that the content went onto the other node and no second
    # plan was written. This file existing says otherwise.
    plan = tmp_path / "append.md"
    plan.write_text(
        _plan(
            "title: T\nstatus: ready\nkind: quick-plan\ncreated: 2026-08-20\n"
            "consolidation:\n"
            "  outcome: append\n"
            "  appended_to:\n"
            "    - id: x-ab12\n"
            "      reason: belongs on that node\n"
        )
    )
    out = _run(plan)
    assert out.returncode == 1
    assert "a plan file was written" in out.stdout


def test_consolidation_entry_accepts_legacy_dashless_literal():
    """A bare "<prefix><4hex>" legacy id references like its dashed kin.

    A config prefix with no separator once minted such literals; a candidate
    the graph holds must be recordable here, or the gate cannot say what it
    considered. The binding check is the graph's own predicate, so any shape
    the graph mints (dashed or legacy bare) passes; only a non-id shape is
    refused. Prose extraction keeps its own stricter grammar.
    """
    from fno.plan.schema import ConsolidationEntry

    assert ConsolidationEntry(id="x4d12", reason="parent epic, not a fold")
    assert ConsolidationEntry(id="a3f9c1d2", reason="legacy bare id references too")
    with pytest.raises(Exception):
        ConsolidationEntry(id="9abc", reason="not an id shape at all")


# -- the `rejected` key (x-6f98): a plan ruling against a foreign node --


def test_rejected_entries_pass_the_gate_beside_any_outcome(tmp_path):
    plan = tmp_path / "rejected.md"
    plan.write_text(
        _plan(
            "title: T\nstatus: ready\nkind: quick-plan\ncreated: 2026-08-18\n"
            "consolidation:\n"
            "  outcome: proceed_alone\n"
            "  rejected:\n"
            "    - id: x-bbbb\n"
            "      reason: opposite direction, nothing reads its fields\n"
        )
    )
    result = _run(plan)
    assert result.returncode == 0, result.stdout
    assert "consolidation outcome is proceed_alone" in result.stdout


def test_rejected_entry_with_a_non_node_id_errors_naming_the_section(tmp_path):
    plan = tmp_path / "rejected-bad-id.md"
    plan.write_text(
        _plan(
            "title: T\nstatus: ready\nkind: quick-plan\ncreated: 2026-08-18\n"
            "consolidation:\n"
            "  outcome: proceed_alone\n"
            "  rejected:\n"
            "    - id: bbbb\n"
            "      reason: missing the prefix\n"
        )
    )
    result = _run(plan)
    assert result.returncode == 1, result.stdout
    assert "is not a node id" in result.stdout
    assert "(rejected)" in result.stdout
