"""Tests for fno.plan._doc - markdown plan-doc parser."""

from __future__ import annotations

from pathlib import Path

import pytest

from fno.plan._doc import FrontmatterError, load_plan


# ---------------------------------------------------------------------------
# Fixtures / helpers
# ---------------------------------------------------------------------------

SIMPLE_PLAN = """\
---
status: design
title: My Feature
---

## Overview

This is the overview body.

## Architecture

Architecture content here.

## Failure Modes

Something might break.
"""

PLAN_NESTED_HEADINGS = """\
---
status: design
---

## Top Level Section

Top body.

### Sub Heading

Sub content, not a top-level section.

#### Deep Heading

Deep content.

## Second Section

Second body.
"""

PLAN_MALFORMED_YAML = """\
---
status: [invalid: yaml: here
title: blah
---

## Overview

body
"""

PLAN_EMPTY_SECTIONS = """\
---
status: design
---

## Section One

## Section Two

Some content.
"""

PLAN_MULTIPLE_CODE_FENCES = """\
---
status: design
---

## Setup

Before code.

```python
## Not a heading in code
def foo():
    pass
```

After code.

```yaml
## Also not a heading
key: value
```

Final text.

## Teardown

Teardown body.
"""


def write_plan(tmp_path: Path, content: str, name: str = "plan.md") -> Path:
    p = tmp_path / name
    p.write_text(content, encoding="utf-8")
    return p


# ---------------------------------------------------------------------------
# AC-HP: Basic parsing
# ---------------------------------------------------------------------------


def test_ac_hp_load_plan_parses_frontmatter(tmp_path: Path) -> None:
    """AC1-HP: frontmatter is parsed into a dict."""
    p = write_plan(tmp_path, SIMPLE_PLAN)
    doc = load_plan(p)
    assert isinstance(doc.frontmatter, dict)
    assert doc.frontmatter["status"] == "design"
    assert doc.frontmatter["title"] == "My Feature"


def test_ac_hp_sections_ordered_by_document_order(tmp_path: Path) -> None:
    """AC2-HP: sections preserve document order."""
    p = write_plan(tmp_path, SIMPLE_PLAN)
    doc = load_plan(p)
    section_names = list(doc.sections.keys())
    assert section_names == ["Overview", "Architecture", "Failure Modes"]


def test_ac_hp_section_body_trimmed(tmp_path: Path) -> None:
    """AC6-HP: section body is trimmed of leading/trailing blank lines."""
    p = write_plan(tmp_path, SIMPLE_PLAN)
    doc = load_plan(p)
    body = doc.sections["Architecture"]
    assert body == "Architecture content here."


# ---------------------------------------------------------------------------
# AC: Code-block fenced headings must NOT be detected as sections
# ---------------------------------------------------------------------------


def test_ac_multiple_code_fences_handled(tmp_path: Path) -> None:
    """AC1: multiple code fences in same section all handled correctly."""
    p = write_plan(tmp_path, PLAN_MULTIPLE_CODE_FENCES)
    doc = load_plan(p)
    assert list(doc.sections.keys()) == ["Setup", "Teardown"]
    body = doc.sections["Setup"]
    assert "## Not a heading in code" in body
    assert "## Also not a heading" in body


# ---------------------------------------------------------------------------
# AC: Nested headings (###) are NOT top-level sections
# ---------------------------------------------------------------------------


def test_ac_nested_headings_not_sections(tmp_path: Path) -> None:
    """AC5: ### headings are NOT top-level sections."""
    p = write_plan(tmp_path, PLAN_NESTED_HEADINGS)
    doc = load_plan(p)
    assert "Sub Heading" not in doc.sections
    assert "Deep Heading" not in doc.sections
    assert "Top Level Section" in doc.sections
    assert "Second Section" in doc.sections


# ---------------------------------------------------------------------------
# AC: FrontmatterError on malformed YAML with line number
# ---------------------------------------------------------------------------


def test_ac_err_frontmatter_error_has_line_number(tmp_path: Path) -> None:
    """AC3-ERR: FrontmatterError.line is populated from yaml exception."""
    p = write_plan(tmp_path, PLAN_MALFORMED_YAML)
    with pytest.raises(FrontmatterError) as exc_info:
        load_plan(p)
    assert exc_info.value.line is not None
    assert isinstance(exc_info.value.line, int)


def test_ac_empty_section_body(tmp_path: Path) -> None:
    """AC6-HP: A section with no body (immediately followed by next ##) returns empty string."""
    p = write_plan(tmp_path, PLAN_EMPTY_SECTIONS)
    doc = load_plan(p)
    assert "Section One" in doc.sections
    assert doc.sections["Section One"] == ""
