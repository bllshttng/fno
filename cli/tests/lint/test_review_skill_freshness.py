"""Contract tests for the review skill's active-instruction preflight."""

from __future__ import annotations

from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
REVIEW_SKILL = ROOT / "skills" / "review" / "SKILL.md"


def test_active_skill_freshness_runs_before_review_router() -> None:
    text = REVIEW_SKILL.read_text(encoding="utf-8")
    probe = 'fno doctor plugin-file "$SKILL_DIR/SKILL.md"'
    router = "## Step 1: Resolve the mode"

    assert probe in text
    assert "PLUGIN_FILE_STALE" in text
    assert text.index(probe) < text.index(router)
