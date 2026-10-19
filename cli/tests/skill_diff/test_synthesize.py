"""Synthesis-output parser coverage (the one non-deterministic seam's contract)."""
from __future__ import annotations

import pytest

from fno.skill_diff import synthesize


def test_parse_nested_braces_not_truncated():
    # hunks carry nested {}; a lazy regex would truncate at the first inner brace.
    p = synthesize.parse_proposal(
        '{"verdict":"propose_pr","hunks":['
        '{"file":"a","new_text":"x","cited_finding_ids":["s1"]},'
        '{"file":"b","new_text":"y","cited_finding_ids":["s2"]}],"justification":"j"}'
    )
    assert len(p.hunks) == 2 and p.justification == "j"


def test_synthesize_timeout_is_parse_error(monkeypatch):
    import subprocess as sp

    def boom(*a, **k):
        raise sp.TimeoutExpired(cmd="claude", timeout=1)

    monkeypatch.setattr(sp, "run", boom)
    with pytest.raises(synthesize.ProposalParseError):
        synthesize.synthesize("prompt", timeout=1)


def test_unknown_verdict_raises():
    with pytest.raises(synthesize.ProposalParseError):
        synthesize.parse_proposal('{"verdict":"ship_it"}')


def test_string_cited_ids_raises():  # would silently become a per-char list
    with pytest.raises(synthesize.ProposalParseError, match="list of strings"):
        synthesize.parse_proposal(
            '{"verdict":"propose_pr","hunks":[{"file":"a","cited_finding_ids":"s1"}]}'
        )


def test_no_json_raises():
    with pytest.raises(synthesize.ProposalParseError):
        synthesize.parse_proposal("the model refused")
