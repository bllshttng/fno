"""Regression tests for two skills/blueprint/scripts/mutate_doc.py bugs hit on
real /think design docs:

(a) The Architecture path regex's backtick alternative used ``[^`]+``, which
    matches across newlines. A fenced ASCII-art diagram (```` ``` ````) was
    therefore captured as a single multi-line "path". On the reporter's Python
    a long fence line (no ``/``) became one >NAME_MAX path component, so
    ``Path.exists()`` raised ``OSError: File name too long`` during auto mode
    detection. The fix tightens the backtick branch to ``[^`\\n]+`` so a
    candidate never spans a newline; legitimate per-line paths inside a diagram
    (e.g. ``cli/src/fno/main.py``) still match via the bare-path branch.

(b) ``_parse_user_stories`` recognized inline-bold and ``### USx`` heading
    forms but not markdown-table form. A table-style User Stories section
    parsed to ``[]``, which tripped the single "implement feature" default
    task. The fix adds a third anchor that reads ``| USx | description | ... |``
    rows (first cell = id, second cell = description).
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[4]
_SCRIPT_PATH = _REPO_ROOT / "skills" / "blueprint" / "scripts" / "mutate_doc.py"


def _load_mutate_module():
    spec = importlib.util.spec_from_file_location("mutate_doc_fence_table", _SCRIPT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["mutate_doc_fence_table"] = module
    spec.loader.exec_module(module)
    return module


_mutate_doc = _load_mutate_module()


# --------------------------------------------------------------------------
# Bug (a): fenced ASCII-art diagram in ## Architecture
# --------------------------------------------------------------------------

_ARCH_WITH_FENCED_DIAGRAM = """## Architecture

The layout looks like this:

```
fno/
├── .claude-plugin/          # Plugin manifest
├── skills/                  # Skills directory
├── scripts/                 # Validation, metrics
└── cli/src/fno/main.py
```

The real entry point is `cli/src/fno/cli.py`.
"""


def test_mutate_auto_mode_with_fenced_diagram(tmp_path: Path) -> None:
    """Full mutate() in auto mode on a doc with a fenced diagram succeeds cleanly."""
    doc_text = (
        "---\ntitle: example\nstatus: design\n---\n\n# Example\n\n"
        + _ARCH_WITH_FENCED_DIAGRAM
        + "\n## Failure Modes\n\n**Boundaries**\n- x\n\n## Open Questions\n\n- q?\n"
    )
    doc = tmp_path / "spec.md"
    doc.write_text(doc_text, encoding="utf-8")
    rc, proposed = _mutate_doc.mutate(doc, mode="auto", rewrite=False, no_emit=True)
    assert rc == 0, f"mutate failed: {proposed}"


def test_file_ownership_map_no_ascii_art_rows() -> None:
    """The brownfield File Ownership Map must not emit a multi-line ASCII-art row.

    This is where the cross-newline blob actually surfaced as user-visible
    breakage: a single garbage row whose `path` was the whole fenced diagram.
    """
    rows = _mutate_doc._build_file_ownership_map(_ARCH_WITH_FENCED_DIAGRAM, _REPO_ROOT)
    assert "├──" not in rows, f"ASCII-art leaked into a File Ownership Map row:\n{rows}"
    # Every data row must be a single line (3 pipe-delimited cells).
    for line in rows.splitlines():
        if line.startswith("| `"):
            # A well-formed 3-column row has 4 pipes: | path | action | owner |
            assert line.count("|") == 4, f"malformed ownership row: {line!r}"


# --------------------------------------------------------------------------
# Bug (b): markdown-table User Stories
# --------------------------------------------------------------------------

_TABLE_USER_STORIES = """| ID | Story | Acceptance |
|------|-------------------------------|----------------|
| US1 | As a user I can log in | redirected to dashboard |
| US2 | As a user I can log out | session cleared |
| US3 | As an admin I can ban users | user blocked |
"""


def test_parse_user_stories_table_form() -> None:
    """Markdown-table User Stories parse into (id, description) tuples in order."""
    stories = _mutate_doc._parse_user_stories(_TABLE_USER_STORIES)
    ids = [s[0] for s in stories]
    assert ids == ["US1", "US2", "US3"], f"table rows not parsed in order: {stories!r}"
    descriptions = dict(stories)
    assert descriptions["US1"] == "As a user I can log in"
    assert descriptions["US2"] == "As a user I can log out"
    assert descriptions["US3"] == "As an admin I can ban users"


def test_build_execution_strategy_from_table(tmp_path: Path) -> None:
    """A table-form User Stories section yields real tasks, not the default placeholder."""
    from collections import OrderedDict

    sections = OrderedDict()
    sections["User Stories"] = _TABLE_USER_STORIES
    strategy = _mutate_doc._build_execution_strategy(sections)
    assert "no User Stories found" not in strategy, (
        "table-form stories fell through to the single default task"
    )
    # One task per row.
    assert strategy.count("Implement US") == 3, f"expected 3 tasks, got:\n{strategy}"
