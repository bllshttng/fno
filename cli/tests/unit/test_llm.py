"""Tests for the shared one-shot LLM boundary."""

from __future__ import annotations

import subprocess

import pytest

from fno.llm import llm_call


def _completed(cmd):
    return subprocess.CompletedProcess(cmd, 0, stdout='{"result":"ok"}', stderr="")


def test_llm_call_builds_schema_constrained_argv(monkeypatch):
    captured = {}

    def runner(cmd, **kwargs):
        captured["cmd"] = cmd
        captured.update(kwargs)
        return _completed(cmd)

    monkeypatch.setenv("ANTHROPIC_API_KEY", "present")
    result = llm_call(
        "payload",
        schema={"type": "object"},
        system_prompt="JSON only",
        model="claude-haiku-4-5",
        timeout=12,
        check=True,
        bare_when_api_key=True,
        runner=runner,
    )

    assert result.returncode == 0
    assert captured["cmd"][:3] == ["claude", "-p", "--bare"]
    assert captured["cmd"][captured["cmd"].index("--model") + 1] == "claude-haiku-4-5"
    assert captured["input"] == "payload"
    assert captured["timeout"] == 12
    assert captured["check"] is True


def test_llm_call_refuses_unstubbed_pytest_call(monkeypatch):
    monkeypatch.delenv("FNO_LLM_STUB", raising=False)
    with pytest.raises(RuntimeError, match="refusing real claude"):
        llm_call("payload")
