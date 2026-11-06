"""Tests for the shared one-shot LLM boundary."""

from __future__ import annotations


import pytest

from fno.llm import llm_call


def test_llm_call_refuses_unstubbed_pytest_call(monkeypatch):
    monkeypatch.delenv("FNO_LLM_STUB", raising=False)
    with pytest.raises(RuntimeError, match="refusing real claude"):
        llm_call("payload")
