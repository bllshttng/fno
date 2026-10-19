"""Keeper-harness thread spawns must divert to the Python dispatch.

The Rust client's thread match handles claude/codex/opencode and refuses
every other name, while the keeper arms live only in ``cmd_spawn`` ->
``_lane_b_thread_spawn``. Without the ``_is_keeper_thread_spawn`` carve-out,
an installed binary answers a working lane with "fno has not built this
harness's keeper lane spawn arm yet".
"""
from __future__ import annotations

from fno.agents.rust_runtime import _is_keeper_thread_spawn, _KEEPER_THREAD_HARNESSES


def test_keeper_roster_matches_the_capability_lane() -> None:
    """The literal tuple and the capability contract must agree exactly, so a
    harness whose thread lane is the keeper always reaches its Python arm."""
    from fno.agents.harness_map import thread_lane
    from fno.harness_names import SPAWN_HARNESSES

    keeper_roster = {h for h in SPAWN_HARNESSES if thread_lane(h) == "keeper"}
    assert keeper_roster == set(_KEEPER_THREAD_HARNESSES)
    assert "opencode" not in keeper_roster, "opencode's row regressed to keeper"


def test_keeper_thread_predicate_reads_every_argv_spelling() -> None:
    """Long, ``=``-joined and glued ``-Hgrok`` spellings divert (Click parses
    them all; the Rust client matches ``-H`` only as an exact token). A
    non-keeper thread, a pane spawn, and tokens past the ``--`` fence never
    divert."""
    for argv in (
        ["spawn", "wk-grok", "--harness", "grok", "--substrate", "thread", "seed"],
        ["spawn", "wk-grok", "-H", "grok", "--substrate=thread", "seed"],
        ["spawn", "wk-grok", "-Hgrok", "--substrate", "thread", "seed"],
    ):
        assert _is_keeper_thread_spawn("spawn", argv), argv
    for argv in (
        ["spawn", "w", "--harness", "claude", "--substrate", "thread", "seed"],
        ["spawn", "w", "--harness", "grok", "seed"],
        ["spawn", "w", "--harness", "claude", "--", "--harness", "grok"],
        ["spawn", "w", "--harness", "grok", "--", "--substrate", "thread"],
    ):
        assert not _is_keeper_thread_spawn("spawn", argv), argv
