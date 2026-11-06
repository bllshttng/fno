"""Behavioral contract: workflow skills follow the authority their helpers enforce.

Each skill in this file once restated a policy its owning implementation had
replaced, so an LLM following the prose could re-run a mutation the helper
refuses or gate a launch the helper frees. These tests assert the skill names
the receipt fields and commands it obeys; skills/agent/tests/test_confirm.sh runs
the confirm helper itself. Positive markers first - a stale prose absence alone
proves nothing about what replaced it.
"""
from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]


def _skill(rel: str) -> str:
    return (REPO_ROOT / rel).read_text(encoding="utf-8")


def _fences(text: str) -> list[tuple[int, str]]:
    """(char_offset, body) of every fenced code block."""
    out = []
    for match in re.finditer(r"^[ \t]*```[a-z]*\n(.*?)^[ \t]*```", text, re.S | re.M):
        out.append((match.start(), match.group(1)))
    return out


# ---- agent: obey confirm-decision.sh (ab-8bed07a0) --------------------------


def test_agent_skill_obeying_helper_is_stated_and_stale_rule_gone():
    text = _skill("skills/agent/SKILL.md")
    hard_rules = text[text.index("Hard rules") :]
    assert "confirm-decision.sh" in hard_rules
    assert "confirm_required" in hard_rules
    assert "always confirms under" not in text
    assert "degrades to `always`" not in text
    assert "degrades to `auto`" in text


# ---- triage: rank mutations only after the approval boundary (AC1-EDGE) ------


def test_triage_mutations_sit_after_the_approval_boundary():
    text = _skill("skills/triage/SKILL.md")
    boundary = text.index("Present to user")
    mutations = ("fno backlog update", "triage apply")
    before = [body for off, body in _fences(text) if off < boundary]
    after = [body for off, body in _fences(text) if off > boundary]
    for fence in before:
        for command in mutations:
            assert command not in fence, f"{command} runs before approval"
    assert any("triage apply" in fence for fence in after), (
        "positive control: the approved apply path must still exist"
    )
