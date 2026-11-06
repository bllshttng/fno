"""Validator's decisions_acknowledged gate: a live ruling with no matching entry.

Change 3 of x-953b. `check_consolidation_file` cross-checks the plan's
`decisions_acknowledged` list against the node's LIVE decision-index rulings
(read through the native decisions door, not through the shape door - the
plan-schema door has no access to the index). Each fixture points a fresh
subprocess at a hermetic decision store the way the door reads one:
`$FNO_HOME` carries `decisions.jsonl` and `$FNO_CONFIG` names a config.toml
whose top-level `state_dir` pins the graph read (the Rust reader parses TOML
only, and warns on a yaml candidate), then seeds `decisions.jsonl` directly,
in the envelope the native decide door writes - so no graph or carveout root
needs to exist for the node named in `claims:`.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
VALIDATOR = REPO_ROOT / "scripts" / "validate-plan.sh"
_EXPIRY_REF_UNSET = object()


def _run(plan: Path, state_dir: Path) -> subprocess.CompletedProcess[str]:
    (state_dir.parent / "config.toml").write_text(
        f'state_dir = "{state_dir}"\n'
    )
    env = dict(os.environ)
    env["FNO_CONFIG"] = str(state_dir.parent / "config.toml")
    env["FNO_HOME"] = str(state_dir)
    return subprocess.run(
        ["bash", str(VALIDATOR), str(plan)],
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        env=env,
    )


def _plan(frontmatter: str) -> str:
    return (
        f"---\n{frontmatter}---\n\n# T\n\n"
        "## Context\n\nReal context here.\n\n"
        "## Changes\n\nAdd the guard in foo.py.\n\n"
        "## Files to Modify\n\n- foo.py\n\n"
        "## Verification\n\npytest cli/tests\n"
    )


def _seed_decision(
    state_dir: Path,
    *,
    decision_id: str,
    subject: str,
    expiry_ref: object = _EXPIRY_REF_UNSET,
    authority_source: str = "beastmode",
) -> None:
    """Append one decision-index row directly, bypassing record_decision.

    `record_decision` also writes a durable events journal and projects onto
    a graph node - both real side effects this fixture does not want. The
    index is the only store the decisions door needs here, so writing it
    directly is the smaller, correct fixture.
    """
    state_dir.mkdir(parents=True, exist_ok=True)
    index = state_dir / "decisions.jsonl"
    data = {
        "decision_id": decision_id,
        "decision": "VERDICT",
        "subject": subject,
        "authority_source": authority_source,
    }
    if expiry_ref is not _EXPIRY_REF_UNSET:
        data["expiry_ref"] = expiry_ref
    row = {
        "ts": "2026-08-01T00:00:00.000000Z",
        "type": "operator_decision",
        "source": "target",
        "data": data,
    }
    with index.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(row) + "\n")


def test_missing_acknowledgment_on_a_post_gate_plan_errors(tmp_path):
    state_dir = tmp_path / "fno-home"
    _seed_decision(state_dir, decision_id="d-abcd1234", subject="x-abcd")

    plan = tmp_path / "new.md"
    plan.write_text(_plan(
        "title: T\nstatus: ready\nkind: quick-plan\nclaims: x-abcd\ncreated: 2026-08-23\n"
        "consolidation:\n"
        "  outcome: proceed_alone\n"
        "  proceed_alone_against: []\n"
    ))
    result = _run(plan, state_dir)
    assert result.returncode == 1, result.stdout
    assert "decisions_acknowledged is missing d-abcd1234" in result.stdout


def test_clean_plan_names_how_many_it_checked(tmp_path):
    state_dir = tmp_path / "fno-home"
    _seed_decision(state_dir, decision_id="d-abcd1234", subject="x-abcd")

    plan = tmp_path / "clean.md"
    plan.write_text(_plan(
        "title: T\nstatus: ready\nkind: quick-plan\nclaims: x-abcd\ncreated: 2026-08-23\n"
        "consolidation:\n"
        "  outcome: proceed_alone\n"
        "  proceed_alone_against: []\n"
        "  decisions_acknowledged:\n"
        "    - decision_id: d-abcd1234\n"
        "      reason: routine, not a verdict on this work\n"
    ))
    result = _run(plan, state_dir)
    assert result.returncode == 0, result.stdout
    assert "1/1 decision(s) acknowledged" in result.stdout
