"""Unit tests for the config.auto_merge typed block (ab-d4c98550).

Parity target: the bash scripts/lib/config.sh auto_merge helpers
(get_auto_merge_enabled / get_auto_merge_strategy / ...). The Python reader
must reproduce the same truth table, including the validation fallbacks
(invalid strategy -> merge, invalid resolution -> opus, invalid remediation
-> attempt). The who-may-merge gate (allowed_invokers / is_allowed_for) was
removed (x-04ab): auto-merge is gated by `enabled` alone.
"""
from __future__ import annotations

from fno.config import AutoMergeBlock, ConfigBlock


def test_defaults_match_bash_defaults():
    b = AutoMergeBlock()
    assert b.enabled is False
    assert b.merge_strategy == "merge"
    assert b.delete_branch_on_merge is True
    assert b.require_checks_pass is True
    assert b.require_fresh_ci is True
    assert b.conflict_resolution == "opus"
    assert b.remediation == "attempt"


def test_invalid_strategy_falls_back_to_merge():
    b = AutoMergeBlock(merge_strategy="banana")
    assert b.merge_strategy == "merge"


def test_present_garbage_flag_behaves_as_false():
    # Bash: get_config returns the garbage string, `== "true"` is false.
    b = AutoMergeBlock(delete_branch_on_merge="nope")
    assert b.delete_branch_on_merge is False


def test_malformed_block_degrades_to_defaults_off():
    # A non-mapping auto_merge: must not break the settings load.
    cfg = ConfigBlock(auto_merge=42)
    assert isinstance(cfg.auto_merge, AutoMergeBlock)
    assert cfg.auto_merge.enabled is False
