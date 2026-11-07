from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]


def _read(relative: str) -> str:
    return (REPO_ROOT / relative).read_text(encoding="utf-8")


def test_target_validates_quick_plans_before_execution() -> None:
    target = _read("skills/target/SKILL.md")
    init_state = _read("skills/target/references/init-state.md")

    assert "Skip for single-file" not in target
    assert "Quick-plan exception" not in init_state
    assert "validate-plan.sh" in target
