"""The self-handoff writer is the fourth writer of plan frontmatter.

`skills/target/scripts/handoff.sh` section 8d appends this session's id to the
plan's `session_ids`. It is a separate implementation from `_stamp.py` and
`fno do plan mutate`, and it is reachable on its own - so a round-trip guard that
covers only the other two is decorative.

These tests run the real shell function extracted from the shipped script,
not a copy, so the guard cannot drift from the code it protects.
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path


import yaml


def parse_frontmatter(content: str):
    """PyYAML-based stand-in for the retired Python codec reader."""
    m = re.match(r"^---\n(.*?)\n---(?:\n|$)", content, re.DOTALL)
    return (yaml.safe_load(m.group(1)) if m else {}) or {}, "", ""

_REPO_ROOT = Path(__file__).resolve().parents[4]
_HANDOFF = _REPO_ROOT / "skills" / "target" / "scripts" / "handoff.sh"


def _section_8d() -> str:
    """Pull the 8d shell function out of the shipped script."""
    text = _HANDOFF.read_text(encoding="utf-8")
    body = text[text.index("# 8d."):]
    snippet = re.search(r"^_append_plan_session_id\(\) \{\n.*?^\}$", body, re.DOTALL | re.MULTILINE)
    assert snippet, "section 8d function not found in handoff.sh"
    return snippet.group(0)


def _run_8d(plan: Path, sid: str) -> None:
    proc = subprocess.run(
        ["bash", "-c", _section_8d() + '\n_append_plan_session_id "$1" "$2"', "8d", str(plan), sid],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr


def test_block_form_append_keeps_later_keys_parseable(tmp_path: Path) -> None:
    """The regression shape: keys AFTER the block list must survive."""
    plan = tmp_path / "plan.md"
    plan.write_text(
        "---\ntitle: t\nsession_ids:\n  - A\n  - B\nstatus: ready\nurls: [u1]\n---\nbody\n",
        encoding="utf-8",
    )

    _run_8d(plan, "C")

    fields, _block, _rest = parse_frontmatter(plan.read_text(encoding="utf-8"))
    assert fields["session_ids"] == ["A", "B", "C"]
    assert fields["urls"] == ["u1"]
