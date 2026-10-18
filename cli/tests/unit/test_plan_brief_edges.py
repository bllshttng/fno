"""Per-task ``blocked_by`` edges in the Execution Strategy parser.

Task identity is normalized in one place (``fno.plan.brief``), so the edge
key must survive that normalization for the orchestrator to see it, and
``validate_task_edges`` is the shared unknown-id + cycle check both the
parser's callers and validate-plan.sh consume.
"""
from __future__ import annotations

import pytest

from fno.plan.brief import (
    BriefParseError,
    parse_execution_strategy,
    validate_task_edges,
)


def _strategy_yaml(tasks: str) -> str:
    return f"```yaml\ntasks:\n{tasks}\nwaves:\n  - wave: 1\n    tasks: ['1.1']\n```"


class TestParseBlockedBy:
    def test_declared_edge_survives_normalization(self):
        yaml_text = _strategy_yaml(
            "  - id: '1.1'\n"
            "    title: Setup\n"
            "  - id: '2.1'\n"
            "    title: Follow-up\n"
            "    blocked_by: ['1.1']\n"
        )
        parsed = parse_execution_strategy(yaml_text)
        by_id = {t["id"]: t for t in parsed["tasks"]}
        assert by_id["2.1"]["blocked_by"] == ["1.1"]
        assert by_id["1.1"]["blocked_by"] == []

    def test_explicit_empty_key_is_marked_declared(self):
        parsed = parse_execution_strategy(
            _strategy_yaml("  - id: '1.1'\n    title: Setup\n    blocked_by: []\n")
        )
        assert parsed["tasks"][0]["blocked_by"] == []
        assert parsed["tasks"][0]["blocked_by_declared"] is True

    def test_non_list_refused(self):
        with pytest.raises(BriefParseError, match="blocked_by must be a list"):
            parse_execution_strategy(
                _strategy_yaml("  - id: '1.1'\n    blocked_by: '1.2'\n")
            )


class TestValidateTaskEdges:
    def _parsed(self, *rows: tuple[str, list[str]]) -> dict:
        return {
            "tasks": [
                {"id": tid, "blocked_by": deps} for tid, deps in rows
            ]
        }

    def test_cycle_includes_inherited_previous_wave_blocker(self):
        parsed = {
            "tasks": [
                {"id": "1.1", "blocked_by": ["2.1"], "blocked_by_declared": True},
                {"id": "2.1", "blocked_by": [], "blocked_by_declared": False},
            ],
            "waves": [
                {"wave": 1, "tasks": ["1.1"]},
                {"wave": 2, "tasks": ["2.1"]},
            ],
        }
        errors = validate_task_edges(parsed)
        assert len(errors) == 1
        assert "cycle" in errors[0]
        assert "1.1" in errors[0] and "2.1" in errors[0]
