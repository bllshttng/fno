"""The /fno:law skill is a discoverable one-step chat surface."""

from __future__ import annotations

from pathlib import Path

import yaml


REPO_ROOT = Path(__file__).parents[3]
SKILL = REPO_ROOT / "skills" / "law" / "SKILL.md"


def test_law_skill_is_discoverable_and_one_step() -> None:
    text = SKILL.read_text(encoding="utf-8")

    assert text.startswith("---\n")
    assert "name: law" in text
    # Placement, not just presence: a bare `"requires:" in text` matched the
    # nested key just as happily as the top-level one it replaced, so it would
    # stay green if the block returned to the unsupported top level.
    frontmatter = yaml.safe_load(text.split("---\n")[1])
    assert "requires" not in frontmatter
    assert frontmatter["metadata"]["requires"]["binaries"] == ["fno >= 0.3.1"]
    assert "/fno:law" in text
    assert "fno inbox decisions" in text
    assert "fno inbox law set" in text
    assert "--authority operator" not in text
