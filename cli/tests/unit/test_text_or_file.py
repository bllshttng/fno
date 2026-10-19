"""`read_text_arg`: the one seam every --*-file flag rides.

Contract: inline only, file only, `-` is stdin, both at once is refused,
and an unreadable file is a clean refusal - never a traceback.

Filter: `fno doctor test cli/tests/unit/test_text_or_file.py`
"""
from __future__ import annotations

import pytest
from click.exceptions import Exit as ClickExit

from fno.text_or_file import read_text_arg


def test_file_only_reads_utf8(tmp_path):
    p = tmp_path / "body.md"
    p.write_text('say "hi"\nwith newlines\n', encoding="utf-8")
    assert read_text_arg(None, p) == 'say "hi"\nwith newlines\n'


def test_both_given_refuses(tmp_path):
    # A readable file, so the missing-file refusal cannot stand in for this one.
    p = tmp_path / "body.md"
    p.write_text("from the file\n", encoding="utf-8")
    with pytest.raises(ClickExit) as exc:
        read_text_arg("inline", p, what="the body")
    assert exc.value.exit_code == 1


def test_missing_file_refuses_cleanly(tmp_path):
    with pytest.raises(ClickExit) as exc:
        read_text_arg(None, tmp_path / "absent.md")
    assert exc.value.exit_code == 1
