"""Lane refusals in the paired-task experiment contract: an unavailable lane
and malformed bank input say so explicitly, never a graded claim built on
unrun or misattributed work.

See docs/architecture/lane-qualification.md for the operator-facing recipe.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.evals.bank import BankError, LaneError, discover_bank, resolve_lane


def test_unavailable_model_profile_refuses_by_name() -> None:
    with pytest.raises(LaneError, match="unknown lane"):
        resolve_lane("no-such-lane-in-config")


def test_malformed_bank_task_refuses_loudly_naming_id_and_file(tmp_path: Path) -> None:
    bad = tmp_path / "broken.yaml"
    bad.write_text("id: broken\ntier: capability\ngrade: []\n", encoding="utf-8")
    with pytest.raises(BankError, match="broken"):
        discover_bank(tmp_path)
