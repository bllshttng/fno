from __future__ import annotations

import csv
import importlib
import re
from collections import Counter
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[3]
BASELINE = REPO_ROOT / "scripts" / "ci" / "command-baseline.txt"
MAP = REPO_ROOT / "scripts" / "ci" / "command-collapse-map.tsv"


def _leaves() -> set[str]:
    return {
        line.split(" !", 1)[0].strip()
        for line in BASELINE.read_text().splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }


def _rows() -> list[dict[str, str]]:
    with MAP.open(newline="") as handle:
        return list(csv.DictReader(handle, delimiter="\t"))


def _python_rows() -> list[dict[str, str]]:
    """The rows a Python group owns. The agents rows stay as the native menu's
    old-spelling contract, and no Python group or baseline line backs them."""
    return [row for row in _rows() if row["current-leaf"].split()[0] != "agents"]


def _mapped_leaves() -> set[str]:
    return {row["current-leaf"] for row in _rows()}


def test_map_matches_the_uncollapsed_click_action_inventory():
    import click

    from fno._lazy_group import typer_group_shape
    from fno.cli import COLLAPSE_KEEP, LAZY_SUBCOMMANDS
    from fno._lazy_group import iter_group_leaves, NATIVE_SERVED_LEAVES

    live: set[str] = set()
    for group in COLLAPSE_KEEP:
        import_path = LAZY_SUBCOMMANDS[group][0]
        module_name, _, attr_name = import_path.rpartition(":")
        obj = getattr(importlib.import_module(module_name), attr_name)
        command = typer_group_shape(obj)
        context = click.Context(command, info_name=group)
        live.update(path for path, _sub in iter_group_leaves(command, context, group))
    live.update(NATIVE_SERVED_LEAVES)

    mapped = {
        leaf
        for leaf in _mapped_leaves()
        if leaf.split()[0] in COLLAPSE_KEEP or leaf in NATIVE_SERVED_LEAVES
    }
    assert live == mapped


def test_live_baseline_matches_the_projected_allocation():
    leaves = _leaves()
    rows = _python_rows()
    mapped_groups = {row["current-leaf"].split()[0] for row in rows}
    projected = {
        row["current-leaf"] if row["tier"] == "KEEP" else row["current-leaf"].split()[0]
        for row in rows
    }
    assert {leaf for leaf in leaves if leaf.split()[0] in mapped_groups} == projected
    # The fno-agents rows are the Rust half's live enumeration of the binary's
    # own verb surface, defended row-by-row by the Rust ratchet, not part of
    # the fno-py leaf inventory the 135 bound caps.
    agents_rows = {leaf for leaf in leaves if leaf.split()[0] == "fno-agents"}
    assert len(leaves) - len(agents_rows) <= 135
    assert "fno-agents" in leaves


def test_runtime_keep_registry_matches_the_checked_in_allocation():
    from fno.cli import COLLAPSE_KEEP

    expected: dict[str, set[str]] = {group: set() for group in COLLAPSE_KEEP}
    group_sizes = Counter(row["current-leaf"].split()[0] for row in _python_rows())
    for row in _python_rows():
        tokens = row["current-leaf"].split()
        if tokens[0] in COLLAPSE_KEEP and row["tier"] == "KEEP" and group_sizes[tokens[0]] > 1:
            expected[tokens[0]].add(tokens[1])
    assert COLLAPSE_KEEP == expected


# The Python enumerator's refusal-template tests retired with the leg its
# ratchet polled (the native door's manifest-drift refusals are a different
# contract, covered in crates/fno/src/repo_lint/tests.rs).
