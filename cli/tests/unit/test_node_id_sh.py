"""Tests for scripts/lib/node-id.sh, the shared shell id classifier.

The classifier is the shell-side counterpart of fno.graph._constants.
is_wellformed_node_id. The two live in different languages on purpose: the
shell resolvers keep a legacy fallback for environments where the fno Python
package is unavailable, so they cannot defer to Python.
"""
from __future__ import annotations

import shlex
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[3]
NODE_ID_SH = REPO / "scripts" / "lib" / "node-id.sh"


def _kind(arg: str) -> str:
    cmd = f'source "{NODE_ID_SH}"; node_id_kind {shlex.quote(arg)}'
    out = subprocess.run(
        ["bash", "-c", cmd], capture_output=True, text=True, check=True
    )
    return out.stdout.strip()


@pytest.mark.parametrize(
    "arg",
    [
        "ab-55ba9adb",
        "xd863",
    ],
)
def test_classifies_fno(arg):
    assert _kind(arg) == "fno"


@pytest.mark.parametrize("arg", ["ENG-441", "PROJ-88", "owner/repo#123"])
def test_classifies_external(arg):
    assert _kind(arg) == "external"
