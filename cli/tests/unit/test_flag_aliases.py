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


def test_direct_call_optioninfo_defaults_treated_as_not_passed(capsys) -> None:
    """Direct (non-Click) calls of a Typer command leave unfilled params
    holding their OptionInfo declaration defaults. Those must behave like
    None: no warning, no both-passed error, coerced to None on return.
    Regression for tests/integration/test_retro_pr_scope.py-style callers."""
    import typer

    oi = typer.Option(None, "--pr", hidden=True)
    assert merge_deprecated_alias(7, oi, **{"canonical_flag": "--pr-number", "legacy_flag": "--pr"}) == 7
    assert merge_deprecated_alias(oi, oi, **{"canonical_flag": "--pr-number", "legacy_flag": "--pr"}) is None
    assert capsys.readouterr().err == ""
