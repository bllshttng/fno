"""Unit tests for fno._flag_aliases.merge_deprecated_alias (Phase 3,
ab-a04f3f1a). Covers the value-shape matrix the CLI sites exercise: scalar,
required-after-merge, and repeatable-list options."""
from __future__ import annotations

import click
import pytest

from fno._flag_aliases import merge_deprecated_alias


KW = {"canonical_flag": "--session-id", "legacy_flag": "--session"}


def test_legacy_only_warns_and_returns_value(capsys) -> None:
    assert merge_deprecated_alias(None, "s1", **KW) == "s1"
    err = capsys.readouterr().err
    assert "deprecated" in err
    assert "--session-id" in err


def test_both_raises_usage_error() -> None:
    with pytest.raises(click.UsageError):
        merge_deprecated_alias("s1", "s2", **KW)
