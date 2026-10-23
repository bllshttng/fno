"""Unit tests for fno.plan.brief - pure logic, no filesystem or subprocess."""

from __future__ import annotations

from collections import OrderedDict

import pytest

from fno.plan._doc import PlanDoc


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

SAMPLE_EXECUTION_YAML = """
execution_mode: mixed
waves:
  - wave: 1
    mode: sequential
    name: "Foundation"
    tasks: [1.1]
  - wave: 2
    mode: sequential
    name: "Surface"
    tasks: [2.1]

tasks:
  - id: "1.1"
    title: "Foundation module"
    surface:
      - cli/src/fno/sample/core.py
      - cli/tests/unit/sample/test_core.py
    verify: "uv run pytest cli/tests/unit/sample/test_core.py -v"
    acceptance: [AC2-HP]
    notes: "Build the core module."

  - id: "2.1"
    title: "CLI entry point"
    surface:
      - cli/src/fno/plan/brief.py
      - cli/tests/unit/plan/test_brief.py
    verify: "uv run pytest cli/tests/unit/plan/test_brief.py -v"
    acceptance: [AC2-HP, AC2-ERR, AC2-UI, AC2-EDGE, AC2-FR]
    notes: "Build the CLI command for brief generation."
"""

SAMPLE_AC_SECTION = """\
**AC2-HP:** Given a plan with task 2.1, when I call brief, then I get markdown.

**AC2-ERR:** Given an unknown task-id, when I call brief, then exit code is 2.

**AC2-UI:** With --format json, output matches the fixed schema.
"""

SAMPLE_LOCKED_DECISIONS = """\
1. **Use stdlib json.** Use the stdlib json module. *Why:* zero extra dependency. *How to apply:* import json.

2. **Fail-open on untagged entries.** Entries without surface tags are included. *Why:* avoids silent brief shrinkage. *How to apply:* include all untagged entries.
"""

SAMPLE_FAILURE_MODES = """\
**Boundaries**
- The system must reject missing plan files with exit code 1
- The system must reject unknown task-ids with exit code 2 and list valid ids

**Errors**
- The system must surface YAML parse failures with exit code 3
"""

SAMPLE_OVERVIEW = """\
This is the first paragraph of the overview. It provides project context for workers.

Second paragraph with more detail.
"""

SAMPLE_PATTERNS = """\
| Pattern | Source | Why reuse |
|---|---|---|
| `resolve_repo_root()` | fno.paths | path anchoring |
| `TyperRunner` | cli/src/fno/plan/brief.py | consistent test pattern |
"""


def _make_plan_doc(extra_sections: dict[str, str] | None = None) -> PlanDoc:
    """Return a minimal PlanDoc with Execution Strategy + standard sections."""
    sections: OrderedDict[str, str] = OrderedDict()
    sections["Overview"] = SAMPLE_OVERVIEW
    sections["Acceptance Criteria"] = SAMPLE_AC_SECTION
    sections["Locked Decisions (DO NOT revisit)"] = SAMPLE_LOCKED_DECISIONS
    sections["Failure Modes"] = SAMPLE_FAILURE_MODES
    sections["Patterns to Reuse"] = SAMPLE_PATTERNS
    sections["Execution Strategy"] = SAMPLE_EXECUTION_YAML
    if extra_sections:
        sections.update(extra_sections)
    return PlanDoc(
        frontmatter={"status": "ready", "feature": "test feature"},
        sections=sections,
    )


# ---------------------------------------------------------------------------
# Tests for parse_execution_strategy
# ---------------------------------------------------------------------------


class TestParseExecutionStrategy:
    @pytest.mark.parametrize(
        ("yaml_text", "message"),
        [
            ("tasks:\n  - not-a-mapping\n", "task 1 must be a mapping"),
        ],
    )
    def test_structural_type_errors_raise(self, yaml_text: str, message: str) -> None:
        from fno.plan.brief import BriefParseError, parse_execution_strategy

        with pytest.raises(BriefParseError, match=message):
            parse_execution_strategy(yaml_text)


# ---------------------------------------------------------------------------
# Tests for extract_overview_paragraph
# ---------------------------------------------------------------------------


class TestExtractOverviewParagraph:
    def test_first_paragraph(self) -> None:
        from fno.plan.brief import extract_overview_paragraph

        result = extract_overview_paragraph(SAMPLE_OVERVIEW)
        assert result.startswith("This is the first paragraph")
        assert "Second paragraph" not in result


# ---------------------------------------------------------------------------
# Tests for filter_locked_decisions
# ---------------------------------------------------------------------------


class TestFilterLockedDecisions:
    def test_relevant_fail_open_untagged(self) -> None:
        """AC2-EDGE: when no entries have surface tags, all are included (fail-open)."""
        from fno.plan.brief import parse_locked_decisions, filter_entries

        entries = parse_locked_decisions(SAMPLE_LOCKED_DECISIONS)
        # All entries are untagged (no explicit surface mentions matching surface_paths)
        result = filter_entries(entries, "relevant", surface_paths=["some/other/file.py"])
        # Fail-open: all untagged entries included
        assert len(result) == 2


# ---------------------------------------------------------------------------
# Tests for parse_acceptance_criteria
# ---------------------------------------------------------------------------


class TestParseAcceptanceCriteria:
    def test_adapter_routes_non_bold_shapes_through_canonical_compiler(self) -> None:
        # AC7-INV: the brief reads criteria through the same compiler mutation
        # and validation use, so a numbered-shape section (no bold AC token)
        # yields synthesized AC{n} codes rather than being invisible.
        from fno.plan.brief import parse_acceptance_criteria

        body = (
            "1. Given a numbered criterion, when compiled, then it is visible.\n"
            "2. A second numbered criterion.\n"
        )
        entries = parse_acceptance_criteria(body)
        assert [e["code"] for e in entries] == ["AC1", "AC2"]
        assert all(e["ac_type"] == "GENERAL" for e in entries)
        assert all(e["text"] for e in entries)

    def test_adapter_swallows_malformed_section(self) -> None:
        # The adapter never crashes the brief generator: a malformed non-empty
        # section degrades to [] (strict refusal is finalization's job).
        from fno.plan.brief import parse_acceptance_criteria

        body = "Just prose with no criterion shape at all.\nMore prose.\n"
        assert parse_acceptance_criteria(body) == []

    def test_filter_globally_untagged_included_with_tagged_task(self) -> None:
        """Regression: tagged-task brief must still include globally-untagged ACs.

        Reported by Gemini (medium) and Codex P2 on PR #283 at brief.py:389.
        A task with `acceptance: [AC1-HP]` previously dropped any AC code not in
        the task's list - including codes that no task referenced at all. The
        spec says "ACs explicitly tagged for this task OR untagged (fail-open)";
        globally-untagged means: code does not appear in ANY task's acceptance.
        """
        from fno.plan.brief import filter_acs_for_task

        entries = [
            {"code": "AC1-HP", "ac_type": "HP", "text": "task 1 happy path", "tags": []},
            {"code": "AC2-HP", "ac_type": "HP", "text": "task 2 happy path", "tags": []},
            {"code": "AC4-EDGE", "ac_type": "EDGE", "text": "global edge case", "tags": []},
        ]
        # Task 1 lists [AC1-HP]; Task 2 lists [AC2-HP]; AC4-EDGE is in no task.
        globally_tagged = {"AC1-HP", "AC2-HP"}
        result = filter_acs_for_task(entries, ["AC1-HP"], globally_tagged)
        codes = [e["code"] for e in result]
        assert "AC1-HP" in codes  # explicit tag for this task
        assert "AC2-HP" not in codes  # tagged for another task - correctly excluded
        assert "AC4-EDGE" in codes  # globally untagged - fail-open include


# ---------------------------------------------------------------------------
# Tests for build_brief (AC2-HP integration of pure logic)
# ---------------------------------------------------------------------------


class TestBuildBrief:
    def test_build_brief_markdown(self) -> None:
        """AC2-HP: build_brief returns a BriefResult with non-empty markdown output."""
        from fno.plan.brief import build_brief

        doc = _make_plan_doc()
        result = build_brief(
            doc,
            task_id="2.1",
            include_failure_modes="relevant",
            include_locked_decisions="relevant",
        )
        assert result.project_context
        assert result.task_spec["id"] == "2.1"
        assert result.task_spec["title"] == "CLI entry point"
        assert result.verify_command == "uv run pytest cli/tests/unit/plan/test_brief.py -v"
        assert result.files  # file list populated from surface

    def test_build_brief_has_acs(self) -> None:
        from fno.plan.brief import build_brief

        doc = _make_plan_doc()
        result = build_brief(
            doc,
            task_id="2.1",
            include_failure_modes="relevant",
            include_locked_decisions="relevant",
        )
        codes = [ac["code"] for ac in result.acceptance_criteria]
        assert "AC2-HP" in codes

    def test_build_brief_failure_modes_none(self) -> None:
        from fno.plan.brief import build_brief

        doc = _make_plan_doc()
        result = build_brief(
            doc, task_id="2.1", include_failure_modes="none", include_locked_decisions="all"
        )
        assert result.failure_modes == []

    def test_build_brief_unknown_task_raises(self) -> None:
        """AC2-ERR: unknown task-id raises BriefError with valid ids."""
        from fno.plan.brief import build_brief, BriefError

        doc = _make_plan_doc()
        with pytest.raises(BriefError) as exc_info:
            build_brief(
                doc,
                task_id="9.9",
                include_failure_modes="relevant",
                include_locked_decisions="relevant",
            )
        err_msg = str(exc_info.value)
        assert "9.9" in err_msg or "not found" in err_msg.lower()
        # Should list valid task-ids
        assert "1.1" in err_msg or "2.1" in err_msg

    def test_build_brief_patterns_filtered_by_surface(self) -> None:
        from fno.plan.brief import build_brief

        doc = _make_plan_doc()
        result = build_brief(
            doc,
            task_id="2.1",
            include_failure_modes="relevant",
            include_locked_decisions="relevant",
        )
        # task 2.1 has brief.py in surface; pattern with source=brief.py should appear
        sources = [p["source"] for p in result.patterns]
        assert any("brief.py" in s for s in sources)

    def test_build_brief_malformed_exec_strategy_raises(self) -> None:
        """AC2-FR: malformed Execution Strategy YAML raises appropriate error."""
        from fno.plan.brief import build_brief

        sections: OrderedDict[str, str] = OrderedDict()
        sections["Overview"] = SAMPLE_OVERVIEW
        sections["Acceptance Criteria"] = SAMPLE_AC_SECTION
        sections["Locked Decisions (DO NOT revisit)"] = SAMPLE_LOCKED_DECISIONS
        sections["Failure Modes"] = SAMPLE_FAILURE_MODES
        sections["Execution Strategy"] = "tasks:\n  - id: [unclosed list\n    title: bad\n"
        doc = PlanDoc(frontmatter={"status": "ready"}, sections=sections)
        with pytest.raises(Exception):
            build_brief(
                doc,
                task_id="1.1",
                include_failure_modes="relevant",
                include_locked_decisions="relevant",
            )


# ---------------------------------------------------------------------------
# Tests for to_json_dict
# ---------------------------------------------------------------------------


class TestToJsonDict:
    def test_json_schema_keys(self) -> None:
        """AC2-UI: JSON output has the fixed schema keys."""
        from fno.plan.brief import build_brief

        doc = _make_plan_doc()
        result = build_brief(
            doc,
            task_id="2.1",
            include_failure_modes="relevant",
            include_locked_decisions="relevant",
        )
        d = result.to_json_dict()
        required_keys = {
            "project_context",
            "task_spec",
            "acceptance_criteria",
            "locked_decisions",
            "failure_modes",
            "files",
            "patterns",
            "verify_command",
        }
        assert required_keys <= set(d.keys())


# ---------------------------------------------------------------------------
# Tests for to_markdown
# ---------------------------------------------------------------------------


class TestToMarkdown:
    def test_markdown_contains_required_sections(self) -> None:
        """AC2-HP: markdown output has all required structural sections."""
        from fno.plan.brief import build_brief

        doc = _make_plan_doc()
        result = build_brief(
            doc,
            task_id="2.1",
            include_failure_modes="relevant",
            include_locked_decisions="relevant",
        )
        md = result.to_markdown()
        assert "project context" in md.lower() or "overview" in md.lower()
        assert "CLI entry point" in md  # task title
        assert "brief.py" in md  # surface file
        assert "uv run pytest" in md  # verify command
