"""Unit tests for ``detect_project`` in ``fno.graph._intake``.

Specifically locks in the tilde-expansion fix: historical graph entries
with ``cwd: ~/code/me/foo`` must match an absolute ``repo_root()`` of
``/Users/.../code/me/foo``. Without ``os.path.expanduser``, the comparison
silently never matches and triage scoping falls through to global view.
"""
from __future__ import annotations

import os
from unittest.mock import patch

from fno.graph._intake import detect_project


def _entry(project: str, cwd: str) -> dict:
    return {"id": f"ab-{project}001", "project": project, "cwd": cwd}


def test_detect_project_matches_tilde_form_cwd(tmp_path):
    """A graph entry stored with `~/code/me/foo` matches an abs repo_root."""
    fake_root = "/Users/testuser/code/me/foo"
    entries = [_entry("foo", "~/code/me/foo")]

    with patch("fno.graph._intake.repo_root", return_value=fake_root), \
         patch.dict(os.environ, {"HOME": "/Users/testuser"}):
        result = detect_project(entries)

    assert result == "foo", (
        "tilde-form cwd should expanduser-match the absolute repo_root; "
        f"got {result!r}"
    )
