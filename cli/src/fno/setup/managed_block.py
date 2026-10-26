"""Marker-fenced footnote block for a host AGENTS.md / CLAUDE.md: the readers.

The block is a `<!-- fno:begin v=N -->` / `<!-- fno:end -->` fence. Nothing
writes it any more; these readers stay for the two callers that inspect a
host file. ``fno doctor`` reads ``stamped_version`` to flag a block older than
``BLOCK_VERSION`` (advisory), and ``context_audit`` reads ``extract_block``.
"""
from __future__ import annotations

import re
from typing import Optional

# Bump when the block body below changes so `fno doctor` flags stale copies.
BLOCK_VERSION = 1

_BEGIN_PREFIX = "<!-- fno:begin"
_END_MARKER = "<!-- fno:end -->"
_BEGIN_RE = re.compile(r"<!-- fno:begin v=(\d+) -->")

# The pointer set (discretion: keep it a pointer set, not a manual). Covers
# forbidden state surfaces, the two-surfaces model, the W2 fold-in policy,
# worktree-first, and the cancel signal.
_BODY = """\
## footnote

This repo uses the footnote delivery pipeline. Two surfaces that compose:

- **`fno` CLI** - atomic state ops: `fno backlog` (the feature graph), `fno do pr`,
  `fno agents mail`, `fno backlog carveout`. Run `fno help` for the catalog.
- **`/fno:*` commands** - orchestration: `/fno:target` (idea -> shipped PR),
  `/fno:think`, `/fno:review`, `/fno:ship pr`, `/fno:fix`.

Never hand-edit these state files (a hook rejects it): `~/.fno/graph.json` (use
`fno backlog`) and `.fno/target-state.md` (immutable after `fno do target init`).

Worktree-first: for repo work use a dedicated feature worktree; keep the main
checkout pullable. Cancel a running pipeline by writing the sentinel with
`printf 'author: <who>\nreason: <why>\n' > .fno/.target-cancelled` (a bare
`touch` also works and reads as unattributed).

Spot a small pre-existing bug while building? Fold the fix into the current PR as
its own atomic commit. Capture non-small finds with `fno backlog carveout add`."""


def _begin_marker(version: int) -> str:
    return f"<!-- fno:begin v={version} -->"


def render_block(version: int = BLOCK_VERSION) -> str:
    """The full fenced block, markers included."""
    return f"{_begin_marker(version)}\n{_BODY}\n{_END_MARKER}"


def stamped_version(text: str) -> Optional[int]:
    """The version stamped in a file's block, or None if it carries no block."""
    m = _BEGIN_RE.search(text)
    return int(m.group(1)) if m else None


def marker_state(text: str) -> str:
    """`none`, `both` for one ordered pair, or `malformed`."""
    begin_count = text.count(_BEGIN_PREFIX)
    end_count = text.count(_END_MARKER)
    if begin_count == 0 and end_count == 0:
        return "none"
    if (
        begin_count == 1
        and end_count == 1
        and text.find(_BEGIN_PREFIX) < text.find(_END_MARKER)
    ):
        return "both"
    return "malformed"


def extract_block(text: str) -> Optional[str]:
    """Return Footnote's exact fenced directive block, if it is well formed."""
    if marker_state(text) != "both":
        return None
    begin = text.find(_BEGIN_PREFIX)
    end = text.find(_END_MARKER, begin)
    if begin == -1 or end == -1:
        return None
    return text[begin : end + len(_END_MARKER)]
