"""Unit tests for fno.plan.criteria - the canonical acceptance compiler.

Covers the acceptance contract:
- AC2-HP: unlabeled criteria get stable AC{n} identifiers in document order.
- AC4-ERR: a duplicate explicit identifier is a hard error naming both
  source locations.
- AC6-EDGE: a non-empty section that yields no criterion refuses with
  supported-shape guidance; a truly empty section returns [].
- AC9-UX: a numbered criterion is accepted as-is and synthesized as AC1
  execution metadata (no author rewrite required).
"""
from __future__ import annotations

import pytest

from fno.plan.criteria import (
    CriteriaParseError,
    compile_criteria,
)


# ---------------------------------------------------------------------------
# AC6-EDGE: empty vs malformed non-empty
# ---------------------------------------------------------------------------


class TestEmptyAndMalformed:
    def test_truly_empty_section_returns_empty(self) -> None:
        assert compile_criteria("") == []
        assert compile_criteria("   \n\n  \n") == []

    def test_non_empty_zero_shapes_raises_with_guidance(self) -> None:
        # Prose with no marker, no heading, no table, no Given/When/Then: none
        # of the six accepted families. Must refuse, not degrade to a fallback.
        body = "This is just prose sentences with no criterion shape.\nMore prose here.\n"
        with pytest.raises(CriteriaParseError) as exc:
            compile_criteria(body)
        msg = str(exc.value)
        # Actionable guidance names the supported shapes.
        assert "Given/When/Then" in msg or "supported" in msg.lower()


# ---------------------------------------------------------------------------
# Legacy bold backward compatibility (AC3-COMPAT surface)
# ---------------------------------------------------------------------------


class TestLegacyBold:

    def test_legacy_bold_tagged_suffix_preserved(self) -> None:
        # The documented legacy form ``**AC1-HP (api):** ...`` keeps the base
        # code and kind; the ``(api)`` tag is metadata, not part of either.
        body = "**AC1-HP (api):** Given valid input, the module returns the result.\n"
        criteria = compile_criteria(body)
        assert len(criteria) == 1
        assert criteria[0].code == "AC1-HP"
        assert criteria[0].kind == "HP"
        assert criteria[0].text == "Given valid input, the module returns the result."

    def test_bare_marker_with_no_statement_is_dropped(self) -> None:
        # A marker that yields no statement is not a criterion; a task that
        # references its code then fails resolution rather than stamping ready
        # against empty behavior.
        body = "**AC1-HP:** Real behavior.\n**AC2-ERR:**\n"
        criteria = compile_criteria(body)
        assert [c.code for c in criteria] == ["AC1-HP"]
        assert criteria[0].text == "Real behavior."

    def test_letter_suffixed_code_compiles_not_dropped(self) -> None:
        # A letter-suffixed number (AC3b, AC3b-HP) is a valid sub-criterion
        # spelling. It used to match neither AC regex and compile to nothing -
        # silently dropped from the contract with no error - so a design could
        # lose an acceptance criterion between /think and /execute. It now compiles,
        # and a typed AC3b-HP does not collide with a sibling AC3-HP.
        body = (
            "**AC3-HP:** Base criterion.\n\n"
            "**AC3b-HP:** Sub-criterion requiring the four-language self-test.\n\n"
            "**AC7a:** A bare letter-suffixed criterion.\n"
        )
        criteria = compile_criteria(body)
        assert [c.code for c in criteria] == ["AC3-HP", "AC3b-HP", "AC7a"]
        assert [c.kind for c in criteria] == ["HP", "HP", "GENERAL"]


# ---------------------------------------------------------------------------
# AC9-UX + AC2-HP: numbered items, synthesized identifiers, determinism
# ---------------------------------------------------------------------------


class TestNumberedAndSynthesis:

    def test_unlabeled_synthesized_in_document_order(self) -> None:
        body = (
            "1. First behavior.\n"
            "2. Second behavior.\n"
            "3. Third behavior.\n"
        )
        criteria = compile_criteria(body)
        assert [c.code for c in criteria] == ["AC1", "AC2", "AC3"]
        assert [c.text for c in criteria] == [
            "First behavior.",
            "Second behavior.",
            "Third behavior.",
        ]

    def test_explicit_code_preserved_amid_unlabeled(self) -> None:
        # An explicit legacy code is preserved verbatim; unlabeled neighbors
        # still synthesize AC{n} without colliding with the explicit number.
        body = (
            "**AC5-HP:** Explicit named criterion.\n\n"
            "- An unlabeled bulleted criterion.\n"
        )
        criteria = compile_criteria(body)
        assert [c.code for c in criteria] == ["AC5-HP", "AC1"]
        assert criteria[0].kind == "HP"
        assert criteria[1].kind == "GENERAL"


# ---------------------------------------------------------------------------
# AC4-ERR: duplicate explicit identifiers
# ---------------------------------------------------------------------------


class TestDuplicateExplicit:
    def test_duplicate_explicit_raises_naming_both_locations(self) -> None:
        body = (
            "**AC1-HP:** First use.\n\n"
            "**AC1-HP:** Second use of the same code.\n"
        )
        with pytest.raises(CriteriaParseError) as exc:
            compile_criteria(body)
        msg = str(exc.value)
        assert "AC1-HP" in msg
        # Both source locations are named (line 1 and line 3).
        assert "line 1" in msg
        assert "line 3" in msg


# ---------------------------------------------------------------------------
# Table boundaries + contiguous Given/When/Then boundaries
# ---------------------------------------------------------------------------


class TestBoundaries:
    def test_table_header_and_separator_are_not_criteria(self) -> None:
        body = (
            "| Given | When | Then |\n"
            "|---|---|---|\n"
            "| a | b | c |\n"
            "| d | e | f |\n"
        )
        criteria = compile_criteria(body)
        # Two data rows -> two criteria; header and separator excluded.
        assert len(criteria) == 2

    def test_contiguous_gwt_block_stays_one_criterion(self) -> None:
        # A label followed by a contiguous GWT block is ONE criterion; a blank
        # line ends the block (a second label+GWT is a separate criterion).
        body = (
            "First scenario\n"
            "Given a\n"
            "When b\n"
            "Then c\n"
            "\n"
            "Second scenario\n"
            "Given d\n"
            "When e\n"
            "Then f\n"
        )
        criteria = compile_criteria(body)
        assert len(criteria) == 2
        assert "First scenario" in criteria[0].text
        assert "Then c" in criteria[0].text
        assert "Second scenario" in criteria[1].text

    def test_fenced_code_block_does_not_yield_spurious_criteria(self) -> None:
        # A list-like line inside a criterion's fenced code example is not a
        # criterion; the two real numbered items compile as AC1 and AC2.
        body = (
            "1. First behavior.\n"
            "   ```\n"
            "   - --verbose\n"
            "   2. also not a criterion\n"
            "   ```\n"
            "2. Second behavior.\n"
        )
        criteria = compile_criteria(body)
        assert [c.code for c in criteria] == ["AC1", "AC2"]
        assert criteria[0].text == "First behavior."
        assert criteria[1].text == "Second behavior."
