"""Unit tests for fno.graph.depends - dependency resolver.

Ported from tests/test_graph.py (the adopt/depends_on test battery).
"""
from __future__ import annotations

from pathlib import Path


from fno.graph.depends import (
    _resolve_depends_on,
    _sequence_token,
    _collect_frontmatter_depends,
)


# -- helpers --


def _make_entry(eid: str, plan_path: str, cwd: str = None) -> dict:
    return {"id": eid, "plan_path": plan_path, "cwd": cwd}


def _write_plan_fm(dirpath: Path, name: str, fm_lines: list) -> Path:
    lines = ["---", *fm_lines, "---", "", "# t"]
    p = dirpath / name
    p.write_text("\n".join(lines) + "\n")
    return p


# -- canonical blocker key: blocked_by first, depends_on one-release fallback --


def test_both_keys_takes_blocked_by_and_warns(tmp_path, capsys):
    p = _write_plan_fm(
        tmp_path,
        "p.md",
        ["blocked_by:", "  - ab-44444444", "depends_on:", "  - ab-55555555"],
    )
    values, _dir = _collect_frontmatter_depends(str(p))
    assert values == ["ab-44444444"]
    assert "depends_on" in capsys.readouterr().err


def test_empty_blocked_by_falls_back_without_warning(tmp_path, capsys):
    # `blocked_by: []` is the default spelling on most plans: present but
    # silent, so the legacy key still speaks and no warning fires.
    p = _write_plan_fm(
        tmp_path, "p.md", ["blocked_by: []", "depends_on:", "  - ab-66666666"]
    )
    values, _dir = _collect_frontmatter_depends(str(p))
    assert values == ["ab-66666666"]
    assert capsys.readouterr().err == ""


# -- _parse_frontmatter tests --


# -- _sequence_token tests --


def test_ac1_hp_sequence_token_letter_suffix():
    assert _sequence_token("02a-foo.md") == "02a"
    assert _sequence_token("04b-bar.md") == "04b"


# -- _resolve_depends_on tests --


def test_ac1_hp_resolve_ab_id_missing():
    """AC1-HP: ab-ID not in graph goes to unresolved."""
    entries = []
    resolved, unresolved = _resolve_depends_on(["ab-deadbeef"], entries, Path("."))
    assert resolved == []
    assert unresolved == ["ab-deadbeef"]


def test_ac1_hp_resolve_by_path(tmp_path):
    """AC1-HP: path reference resolves against entries by plan_path."""
    plan_path = str(tmp_path / "plans" / "01-alpha.md")
    entries = [_make_entry("ab-11111111", plan_path)]
    resolved, unresolved = _resolve_depends_on([plan_path], entries, tmp_path)
    assert resolved == ["ab-11111111"]
    assert unresolved == []


def test_ac1_hp_resolve_by_slug(tmp_path):
    """AC1-HP: slug reference resolves against entries by plan_path basename."""
    entries = [_make_entry("ab-22222222", str(tmp_path / "plans" / "01-alpha.md"))]
    resolved, unresolved = _resolve_depends_on(["01-alpha.md"], entries, tmp_path)
    assert resolved == ["ab-22222222"]
    assert unresolved == []


def test_ac1_hp_resolve_by_sequence_token(tmp_path):
    """AC1-HP: bare sequence token resolves within same directory."""
    plan_dir = tmp_path / "batch"
    plan_dir.mkdir()
    entries = [_make_entry("ab-33333333", str(plan_dir / "01-foo.md"))]
    resolved, unresolved = _resolve_depends_on(["01"], entries, plan_dir)
    assert resolved == ["ab-33333333"]
    assert unresolved == []


def test_ac1_hp_resolve_numeric_token_normalize(tmp_path):
    """AC1-HP: bare '1' resolves same as '01' for numeric tokens."""
    plan_dir = tmp_path / "batch"
    plan_dir.mkdir()
    entries = [_make_entry("ab-44444444", str(plan_dir / "01-foo.md"))]
    resolved, unresolved = _resolve_depends_on(["1"], entries, plan_dir)
    assert resolved == ["ab-44444444"]
    assert unresolved == []


def test_ac1_hp_collect_frontmatter_depends_inline_list(tmp_path):
    """AC1-HP: _collect_frontmatter_depends parses inline [a, b] form."""
    p = tmp_path / "plan.md"
    p.write_text("---\ntitle: Y\ndepends_on: [ab-11111111, ab-22222222]\n---\n# Y\n")
    raw, plan_dir = _collect_frontmatter_depends(str(p))
    assert raw == ["ab-11111111", "ab-22222222"]


def test_ac2_collect_frontmatter_depends_directory_returns_empty(tmp_path):
    """AC2: a directory plan_path (folder plans no longer exist) returns empty deps, no crash."""
    plan_dir = tmp_path / "feature"
    plan_dir.mkdir()
    raw, resolved_dir = _collect_frontmatter_depends(str(plan_dir))
    assert raw == []
    assert resolved_dir == plan_dir.parent
