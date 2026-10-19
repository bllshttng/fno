from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SKILL = ROOT / "skills" / "think" / "SKILL.md"


def test_think_runs_a_fixed_three_step_process() -> None:
    """The process is identical every run; only the brief varies."""
    text = SKILL.read_text(encoding="utf-8")
    assert "fno do think inspect" in text
    assert "Investigate primary sources" in text
    assert "Write one Markdown file" in text
    assert "fno do plan path" in text
