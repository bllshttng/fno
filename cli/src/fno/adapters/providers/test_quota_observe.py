"""The quota-config loader leaf tests that outlive the deleted signal.

The observe/defer split and its probe fixtures moved to the Rust quota route
(``crates/fno-agents/src/backlog/quota_route.rs``); what stays here is the
config layer: ``QuotaConfig`` defaults and the loader's leaf-by-leaf salvage.

Run: cd cli && uv run pytest src/fno/adapters/providers/test_quota_observe.py -v
"""
from __future__ import annotations

from fno.adapters.providers.model import QuotaConfig


class TestQuotaConfigDefaults:
    def test_observe_defaults_off(self) -> None:
        assert QuotaConfig().observe is False

    def test_observe_survives_a_neighbouring_invalid_leaf(self) -> None:
        """The loader salvages leaf by leaf, so a bad sibling key must not
        discard an armed observe."""
        from fno.adapters.providers.loader import _quota_leaf_valid

        assert _quota_leaf_valid("observe", True) is True
        assert _quota_leaf_valid("defer_threshold_pct", 900.0) is False
