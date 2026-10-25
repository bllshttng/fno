"""Shared reader for the immutable target session manifest.

One typed contract for every consumer of ``.fno/target-state.md`` so the
resume-bind primitive and the orienter cannot drift apart on what a manifest
looks like. The manifest is YAML frontmatter (parsed here) PLUS body ``key:
value`` lines that init writes below the frontmatter for the body-only
identity fields. Both halves are merged here.

The manifest is the identity anchor for native resume : its
``harness``/``harness_session_id``/``fno_id``/``graph_node_id``/
``target_claim_holder`` tuple is what a resumed process must match to rebind
the node claim.
"""
from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Optional

import yaml


# Body keys appended below the frontmatter (init-target-state.sh writes them as
# `key: value` lines, NOT YAML frontmatter), so a frontmatter-only read never
# sees them. Kept here as the single source of the body-key set.
BODY_KEYS = ("graph_node_id", "target_claim_key", "target_claim_holder")


def _frontmatter(path: Path) -> Optional[dict[str, Any]]:
    """The manifest file's YAML frontmatter, or None on any fault.

    The whoami family's state loader is native now, so this module owns its
    frontmatter read: parse the delimited block with yaml.safe_load, return
    {} when no delimiters are present, None when the file is missing,
    unreadable, or malformed (a read degrades, never refuses).
    """
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None
    if not text.startswith("---"):
        return {}
    rest = text[3:].lstrip("\n")
    idx = rest.find("\n---")
    if idx == -1:
        return None
    try:
        parsed = yaml.safe_load(rest[:idx])
    except yaml.YAMLError:
        return None
    return parsed if isinstance(parsed, dict) else {}


def read_target_manifest(project_root: Path) -> Optional[dict[str, Any]]:
    """Merged session manifest: frontmatter + the body ``key: value`` lines.

    Returns the frontmatter dict enriched with any body-only keys
    (``graph_node_id`` / ``target_claim_*``) the frontmatter did not already
    carry. ``None`` when no manifest exists. Never raises.

    ``project_root`` pins both the frontmatter read and the body read to the
    SAME manifest (detected from the root, not cwd, which can differ under
    ``FNO_REPO_ROOT`` or a subdirectory).
    """
    from fno import paths as _paths

    manifest = _paths.target_state_path_or_legacy(project_root)
    raw: Optional[dict[str, Any]] = _frontmatter(manifest)
    try:
        text = manifest.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return raw
    for key in BODY_KEYS:
        if raw and raw.get(key):
            continue
        m = re.search(rf"^{key}\s*:\s*(.+)$", text, re.MULTILINE)
        if m:
            val = m.group(1).strip().strip("\"'")
            if val and val != "null":
                raw = raw or {}
                raw[key] = val
    return raw


def manifest_identity(raw: Optional[dict[str, Any]]) -> Optional[dict[str, str]]:
    """The minimal identity tuple a resume-bind needs, or None if incomplete.

    Returns ``{harness, harness_session_id, fno_id, graph_node_id,
    target_claim_key, target_claim_holder}`` when every field is present and
    non-null, else None. A manifest missing any of these cannot prove a resumed
    process belongs to this target attempt, so the rebind must refuse.
    """
    if not raw:
        return None
    fields = (
        "harness",
        "harness_session_id",
        "fno_id",
        "graph_node_id",
        "target_claim_key",
        "target_claim_holder",
    )
    ident: dict[str, str] = {}
    for f in fields:
        val = raw.get(f)
        val = str(val).strip().strip("\"'") if val is not None else ""
        if not val or val == "null":
            return None
        ident[f] = val
    return ident
