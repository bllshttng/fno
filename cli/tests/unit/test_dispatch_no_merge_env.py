"""x-9d11: the dispatch refusal carrier rides resolve_dispatch's env.

The choke point every spawn surface that consumes the resolver's tuple (skill
spawn.sh, the dispatch.py pane, advance's `fno agents spawn`) must set
TARGET_NO_MERGE whenever the resolved command carries the refusal, and must
rewrite the legacy bare `no-merge` token to the flag. Lanes that shell
`fno agents spawn` WITHOUT the resolver (recovery respawn, keep_going) set the
env directly at their spawn sites. Without a carrier, a worker that drops the
flag post-compaction folds no refusal at init and a configured auto-merge
stands unrevoked.

x-8151/d-450caaeb: the vocabulary is the canonical merge_posture table
(authored in the Rust tree, shipped here as generated package data), so these
tests are pure Python - no binary needed - while the Rust engine's tests in
crates/fno-agents/src/merge_posture.rs pin the same semantics from the same
table.
"""


import pytest

from fno.agents.harness_map import resolve_dispatch


def test_legacy_bare_token_is_rewritten_and_carries_env():
    dispatch = resolve_dispatch(
        harness="claude",
        node_id="x-1",
        command="/target no-merge {id}",
        trigger="autonomous",
    )
    # Rewritten to the flag spelling: the fold and every env match key on
    # `--no-merge`, so a stale operator template keeps its refusal.
    assert "--no-merge" in dispatch["command"]
    assert " no-merge " not in f" {dispatch['command']} "
    assert dispatch["env"].get("TARGET_NO_MERGE") == "1"


# --------------------------------------------------------------------------- #
# x-8151: apply_to_environ - the Python lane's env application over the owner
# --------------------------------------------------------------------------- #


@pytest.fixture(autouse=True)
def _clean_carrier(monkeypatch):
    monkeypatch.delenv("TARGET_NO_MERGE", raising=False)


# --------------------------------------------------------------------------- #
# x-8151: merge_posture on resolve_dispatch - the resolver owns the carrier
# for every rung, and the dispatch shell stops re-deriving it
# --------------------------------------------------------------------------- #


def test_posture_allow_overrides_the_builtin_and_never_edits_templates():
    # allow beats a no-merge target config on the builtin rung.
    dispatch = resolve_dispatch(
        harness="claude",
        node_id="x-1",
        merge_posture="allow",
        dispatch_cfg={"auto_merge": False},
        trigger="autonomous",
    )
    assert dispatch["command"] == "/target x-1"
    assert "TARGET_NO_MERGE" not in dispatch["env"]
    # An explicit template is never edited under allow: a refusal it carries
    # wins (every refusal outranks every grant).
    explicit = resolve_dispatch(
        harness="claude",
        node_id="x-1",
        command="/target --no-merge {id}",
        merge_posture="allow",
        trigger="autonomous",
    )
    assert explicit["command"] == "/target --no-merge x-1"
    assert explicit["env"]["TARGET_NO_MERGE"] == "1"


def test_posture_from_config_reads_grant_and_degrades_to_no_merge():
    granted = resolve_dispatch(
        harness="claude",
        node_id="x-1",
        command="/target {id}",
        merge_posture="from-config",
        dispatch_cfg={"auto_merge": True},
        trigger="autonomous",
    )
    assert "--no-merge" not in granted["command"]
    refused = resolve_dispatch(
        harness="claude",
        node_id="x-1",
        command="/target {id}",
        merge_posture="from-config",
        dispatch_cfg={},
        trigger="autonomous",
    )
    assert refused["command"] == "/target --no-merge x-1"


def test_posture_unknown_value_fails_closed():
    from fno.agents.harness_map import DispatchResolveError

    with pytest.raises(DispatchResolveError, match="unknown merge posture"):
        resolve_dispatch(
            harness="claude",
            node_id="x-1",
            merge_posture="merge-now",
            trigger="autonomous",
        )
