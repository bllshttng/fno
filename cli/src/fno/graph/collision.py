"""Plan file-surface parsing and task partitioning.

Pure-Python primitives consumed by the plugin skills:
  * skills/execute/orchestrator.py  - partition over (task id, files) pairs
  * skills/blueprint/scripts/mutate_doc.py - has_file_surface

Public API:
    HIDDEN_SHARED_OUTPUT_ROOTS      - the generated-output rule's root tuple
    parse_files_to_modify(plan_path) - normalize files from a plan's table
    has_file_surface(plan_path)      - true when a plan states a file surface
    match_shared_root(path, roots)   - the shared-output root a path falls under
    partition(items, shared_roots)   - group items by shared path (task grain)

The file-collision decision itself (thresholds, severity scoring, action
inference, acknowledged reconciliation) is native: the collision engine in
crates/fno-agents/src/backlog/collision.rs, fed by cutover.rs
collision_thresholds. The Python engine that mirrored it died with its
last callers when the graph CLI's collisions sub-app and triage health
went native; this module keeps only what the skills still import.
"""
from __future__ import annotations

import posixpath
import re
import sys
from pathlib import Path
from typing import Collection

# Hidden shared-output roots (the generated-output rule): two tasks writing
# under one of these directories collide even when no literal path overlaps.
# Canonical home is here so the cli's task-grain width walk and the skills
# orchestrator read ONE tuple; the orchestrator imports the name so its own
# callers keep the original spelling.
HIDDEN_SHARED_OUTPUT_ROOTS = (
    ".fno/",
    ".codex/agents/",
    ".gemini/agents/",
    "docs/",
    "internal/",
)

# ---------------------------------------------------------------------------
# File-table parser
# ---------------------------------------------------------------------------

# Recognized headings under which the file list lives. Plans in this repo
# settled on "Files to Modify"; we accept a couple of synonyms in case a
# templating tool ever drifts.
# "file ownership map" is the section /blueprint writes; without it every
# blueprint-generated plan parses to an empty surface and silently cannot collide.
_FILE_HEADINGS = (
    "files to modify",
    "files to change",
    "files touched",
    "file ownership map",
    "files",
)

# Trailing line suffix on a code reference: `path/to/file.py:42` should
# normalize to `path/to/file.py`.
_LINE_SUFFIX_RE = re.compile(r":\d+(-\d+)?$")


def _strip_path(raw: str) -> str:
    """Normalize a file cell from a markdown table.

    Strips backticks, leading/trailing whitespace, tilde paths
    (``~/.fno/settings.yaml`` -> ``~/.fno/settings.yaml``,
    deliberately preserved), parenthetical annotations like ``(template)``,
    and trailing line suffixes.
    """
    s = raw.strip()
    # Drop trailing parenthetical annotations: `path (template)` -> `path`.
    s = re.sub(r"\s*\([^)]*\)\s*$", "", s)
    # Strip backticks and surrounding whitespace.
    s = s.replace("`", "").strip()
    # Strip line suffix.
    s = _LINE_SUFFIX_RE.sub("", s)
    return s


def _is_file_table_row(line: str) -> bool:
    """True if the line looks like a markdown table row with a file in col 1."""
    if not line.startswith("|"):
        return False
    # The header separator row: `| --- | --- |` or `|------|------|`
    if re.match(r"^\|[\s:|-]+\|[\s:|-]*$", line):
        return False
    return True


def _extract_files_from_section(text: str, heading_index: int) -> set[str]:
    """Walk lines after a recognized heading until the next heading or EOF.

    Returns the set of normalized file paths from column 1 of any markdown
    tables encountered. A non-table line is allowed (prose between heading
    and table) but a new ``##`` heading terminates the search.
    """
    lines = text.splitlines()
    files: set[str] = set()
    saw_header = False
    saw_separator = False
    for line in lines[heading_index + 1:]:
        stripped = line.strip()
        if stripped.startswith("##"):
            break
        if not _is_file_table_row(stripped):
            # Reset table parsing once we leave a table; another table can
            # still appear later under the same heading.
            if saw_header and saw_separator and not stripped:
                # Blank line after a table - keep going; another table may
                # follow under the same heading.
                saw_header = False
                saw_separator = False
            continue
        # First valid table row is the header.
        if not saw_header:
            saw_header = True
            continue
        # Second row should be the separator.
        if saw_header and not saw_separator:
            # Tolerate plans that omit the separator (rare but cheap to allow).
            if re.match(r"^\|[\s:|-]+\|", stripped):
                saw_separator = True
                continue
            saw_separator = True  # treat as separator and fall through
        # Data row: split, take column 1.
        parts = [p for p in stripped.strip("|").split("|")]
        if not parts:
            continue
        cell = _strip_path(parts[0])
        if cell:
            files.add(cell)
    return files


def parse_files_to_modify(plan_path: Path) -> set[str]:
    """Extract file paths from the 'Files to Modify' table of a plan.

    Returns a normalized set of repo-relative paths. Strips backticks, line
    suffixes (``:123``), parenthetical annotations, and folder slashes.
    Missing or unreadable plan files return an empty set; the caller treats
    "no parseable files" as "cannot collide" so a malformed plan never
    spurious-blocks a new spec.
    """
    if not plan_path.exists():
        return set()
    if plan_path.is_dir():
        # A folder plan spreads its file tables across 00-INDEX.md and the
        # NN-* phase files. Reading the directory itself raises IsADirectoryError,
        # which _scan_one degrades to an empty set - indistinguishable from a
        # plan that states no surface, so every folder plan bypassed the gate.
        found: set[str] = set()
        for child in sorted(plan_path.glob("*.md")):
            found |= _scan_one(child)
        return found

    return _scan_one(plan_path)


def has_file_surface(plan_path: Path) -> bool:
    """True when a plan states a file surface a collision check can compare.

    False means the check is UNEVALUATED, not clean: an empty surface and a
    clean comparison both yield no findings, so a caller that needs to tell
    "compared, no overlap" from "had nothing to compare" must ask here first.
    """
    return bool(parse_files_to_modify(plan_path))


def match_shared_root(path: str, roots: Collection[str]) -> str | None:
    """The shared-output root a normalized path falls under, or None.

    A path equal to a root or written beneath it shares that generated-output
    directory. Task-grain callers pass their hidden-root list (the
    orchestrator's hidden shared output roots); node-grain callers pass
    nothing, so the rule applies only where the output-root hazard exists.
    Input is normalized here so both callers (partition's grouping and the
    orchestrator's legacy conflict keys) apply the identical rule.
    """
    normalized = posixpath.normpath(str(path).strip().strip("`"))
    if not normalized or normalized == ".":
        return None
    for root in roots:
        base = root.rstrip("/")
        if normalized == base or normalized.startswith(base + "/"):
            return base
    return None


def partition(
    items: list[tuple[str, set[str]]],
    shared_roots: Collection[str] = (),
) -> tuple[list[set[str]], set[str]]:
    """Group items by shared path, so overlapping work can serialize as a unit.

    Union-find over ``(id, paths)`` items keyed by normalized path: two items
    sharing a path land in one group, and so do two items writing under one of
    ``shared_roots`` (the generated-output rule; empty by default). Returns
    ``(groups, unevaluated)`` - disjoint sets of item ids, and the ids with
    no usable path (empty set, or every entry normalizing away). An
    unevaluated item is a singleton group as well:
    "no parseable file list" is its own verdict, never a silent pass (the same
    rule ``_classify_lane_candidate`` applies at node grain).
    """
    parent: dict[str, str] = {}
    for item_id, _ in items:
        # setdefault, not assignment: a caller that repeats an id (two path
        # batches for one item) must not reset the unions already made.
        parent.setdefault(item_id, item_id)

    def find(node: str) -> str:
        while parent[node] != node:
            parent[node] = parent[parent[node]]  # path halving
            node = parent[node]
        return node

    def union(left: str, right: str) -> None:
        left_root = find(left)
        right_root = find(right)
        if left_root != right_root:
            parent[right_root] = left_root

    path_owner: dict[str, str] = {}
    root_owner: dict[str, str] = {}
    evaluated: set[str] = set()
    for item_id, paths in items:
        for raw in paths:
            normalized = posixpath.normpath(str(raw).strip().strip("`"))
            if not normalized or normalized == ".":
                continue
            evaluated.add(item_id)
            union(path_owner.setdefault(normalized, item_id), item_id)
            root = match_shared_root(normalized, shared_roots)
            if root:
                union(root_owner.setdefault(root, item_id), item_id)

    groups: dict[str, set[str]] = {}
    unevaluated: set[str] = set()
    for item_id, _paths in items:
        groups.setdefault(find(item_id), set()).add(item_id)
        # No usable path (raw-empty set, or every entry normalizing away):
        # unevaluated - never a silent concurrent pass.
        if item_id not in evaluated:
            unevaluated.add(item_id)
    ordered = sorted(groups.values(), key=lambda group: sorted(group))
    return ordered, unevaluated


def _scan_one(path: Path) -> set[str]:
    """Read a single markdown file and pull out its files-to-modify set."""
    try:
        text = path.read_text()
    except (OSError, UnicodeDecodeError) as exc:
        # UnicodeDecodeError (a ValueError) escapes a bare OSError catch, so a
        # single non-UTF-8 byte crashed the caller instead of parsing empty -
        # the same containment _read_plan_frontmatter already applies.
        print(f"Warning: collision parser cannot read {path}: {exc}", file=sys.stderr)
        return set()

    found: set[str] = set()
    lines = text.splitlines()
    for idx, line in enumerate(lines):
        stripped = line.strip()
        if not stripped.startswith("##"):
            continue
        heading_text = stripped.lstrip("#").strip().lower()
        if any(heading_text.startswith(h) for h in _FILE_HEADINGS):
            found |= _extract_files_from_section(text, idx)
    return found
