from __future__ import annotations

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]


def test_done_delivery_archives_claimless_finalized_manifest() -> None:
    helper = (REPO_ROOT / "hooks/helpers/init-target-state.sh").read_text()
    finalized_whitelist = helper.split('event.get("type") == "session_finalized"', 1)[1]
    finalized_whitelist = finalized_whitelist.split("):", 1)[0]
    assert '"DoneDelivery"' in finalized_whitelist
