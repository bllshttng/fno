"""Integration tests for orchestrator single-doc plan support.

Covers:
- AC4-EDGE (single-doc): single-doc plan with ## Execution Strategy YAML
  parses to the expected wave/task structure
- Malformed Execution Strategy YAML -> None plus a section diagnostic
"""
from __future__ import annotations

import sys
from pathlib import Path
from textwrap import dedent

import pytest

# Add the orchestrator's parent to path so we can import it directly.
_ORCHESTRATOR_DIR = Path(__file__).resolve().parents[3] / "skills" / "execute"
sys.path.insert(0, str(_ORCHESTRATOR_DIR))

# Add the fno package to path
_CLI_SRC = Path(__file__).resolve().parents[3] / "cli" / "src"
sys.path.insert(0, str(_CLI_SRC))

from orchestrator import (  # noqa: E402
    load_plan_strategy,
    parse_execution_strategy,
)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

MINIMAL_STRATEGY_YAML = dedent("""\
    execution_mode: sequential
    waves:
      - wave: 1
        mode: sequential
        tasks: [1.1]
        reason: "single task"
""")

MULTI_WAVE_YAML = dedent("""\
    execution_mode: mixed
    waves:
      - wave: 1
        mode: sequential
        tasks: [1.1]
        reason: "foundation"
      - wave: 2
        mode: parallel
        tasks: [2.1, 2.2]
        reason: "parallel work"
""")


def _make_single_doc_plan(tmp_path: Path, strategy_yaml: str = MINIMAL_STRATEGY_YAML) -> Path:
    """Create a minimal single-doc plan with ## Execution Strategy section."""
    plan_file = tmp_path / "my-feature.md"
    plan_file.write_text(
        "---\n"
        "status: ready\n"
        "execution_mode: sequential\n"
        "---\n"
        "\n"
        "# My Feature\n"
        "\n"
        "## Overview\n"
        "\n"
        "Test feature overview.\n"
        "\n"
        "## Execution Strategy\n"
        "\n"
        "```yaml\n"
        f"{strategy_yaml}"
        "```\n"
    )
    return plan_file


# ---------------------------------------------------------------------------
# AC4-EDGE: single-doc plan parses correctly
# ---------------------------------------------------------------------------


def test_single_doc_multi_wave_plan(tmp_path: Path) -> None:
    """Single-doc plan with multiple waves parses all waves correctly."""
    plan_file = _make_single_doc_plan(tmp_path, MULTI_WAVE_YAML)

    strategy = load_plan_strategy(str(plan_file))

    assert strategy is not None
    assert len(strategy.waves) == 2
    assert strategy.waves[1].tasks == ["2.1", "2.2"]


def test_public_orchestrator_parser_uses_canonical_strategy_loader(tmp_path: Path) -> None:
    plan_file = tmp_path / "canonical.md"
    plan_file.write_text(
        dedent("""\
            ---
            status: ready
            ---

            # Canonical plan

            ## Execution Strategy

            Explanatory prose is allowed before the manifest.

            ```yml
            execution_mode: sequential
            waves:
              - wave: 1
                mode: sequential
                tasks: ["1.1"]
              - wave: 2
                mode: sequential
                tasks: ["2.1"]
            tasks:
              - id: "1.1"
                title: First
                surface: [src/a.py]
                verify: pytest tests/test_a.py
                acceptance: [AC1]
              - id: "2.1"
                title: Second
                surface: [src/b.py]
                verify: pytest tests/test_b.py
                acceptance: [AC2]
            ```
        """),
        encoding="utf-8",
    )

    strategy = parse_execution_strategy(str(plan_file))

    assert strategy is not None
    assert [wave.tasks for wave in strategy.waves] == [["1.1"], ["2.1"]]


# ---------------------------------------------------------------------------
# Malformed Execution Strategy YAML in single-doc -> returns None + stderr
# ---------------------------------------------------------------------------


def test_malformed_execution_strategy_in_single_doc_returns_none(
    tmp_path: Path, capsys: pytest.CaptureFixture
) -> None:
    """Single-doc with invalid Execution Strategy YAML returns None and emits structured error."""
    plan_file = tmp_path / "malformed.md"
    plan_file.write_text(dedent("""\
        ---
        status: ready
        ---

        # Malformed Plan

        ## Execution Strategy

        ```yaml
        execution_mode: sequential
        waves:
          - wave: 1
            bad_indent:
              this: is: broken: yaml: [unclosed
        ```
    """))

    result = load_plan_strategy(str(plan_file))

    assert result is None
    _, err = capsys.readouterr()
    # Should emit something about the section name
    assert "Execution Strategy" in err or "malformed" in err.lower() or "yaml" in err.lower()
