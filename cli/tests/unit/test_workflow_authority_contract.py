"""Behavioral contract: workflow skills follow the authority their helpers enforce.

Each skill in this file once restated a policy its owning implementation had
replaced, so an LLM following the prose could re-run a mutation the helper
refuses or gate a launch the helper frees. These tests assert the skill names
the receipt fields and verbs it obeys; skills/agent/tests/test_confirm.sh runs
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


def test_agent_skill_obeying_helper_is_stated_and_stale_rule_gone():
    text = _skill("skills/agent/SKILL.md")
    hard_rules = text[text.index("Hard rules") :]
    assert "confirm-decision.sh" in hard_rules
    assert "confirm_required" in hard_rules
    assert "always confirms under" not in text
    assert "degrades to `always`" not in text
    assert "degrades to `auto`" in text


def test_agent_skill_preserves_explicit_consent_gates():
    text = _skill("skills/agent/SKILL.md")
    assert "confirm even the free lane" in text
    assert "always confirms (destructive)" in text


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


def test_lead_checkin_names_the_merge_finish_line():
    # The check-in printed open-PR indicators and a lever list that never
    # merged, so a lead following the skill read a ready PR as report-only and
    # escalated it instead of merging. The merge law makes the team the
    # merger, so the skill must carry the verb, the gate, and the merge rule
    # together: a lever the file cannot name is a law rediscovered by
    # exhaustion.
    text = _skill("skills/lead/SKILL.md")
    checkin = text[text.index("## The check-in body") :]
    checkin = checkin[: checkin.index("## Recording a ruling")]
    assert "fno do pr status" in checkin
    assert "fno do pr merge" in checkin
    assert "the team merges green" in checkin


def test_review_empty_diff_guard_resolves_the_named_target():
    text = _skill("skills/review/SKILL.md")
    guard = text[text.index("### 2a. Empty-diff guard") :]
    guard = guard[: guard.index("### 2b")]
    assert "REVIEW_TARGET" in text[text.index("## Step 1") : text.index("## Step 2")]
    assert 'if [ -z "$REVIEW_TARGET" ]' in guard
    assert 'git log "$BASE".."$REVIEW_TARGET"' in guard
