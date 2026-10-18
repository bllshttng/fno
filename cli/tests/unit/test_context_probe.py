"""Tests for the Python bridge to the Rust-owned context probe."""
from __future__ import annotations

from fno.context_probe import ContextReading, probe_context


def test_probe_returns_the_native_context_receipt(monkeypatch, tmp_path):
    transcript = tmp_path / "t.jsonl"
    calls = []

    def native(verb, args, *, timeout):
        calls.append((verb, args, timeout))
        return None, {
            "used_tokens": 23,
            "window_tokens": 100,
            "used_pct": 23,
            "model": "codex-test-model",
        }

    monkeypatch.setattr("fno.rust_binary.call_binary_json", native)

    assert probe_context(transcript_path=transcript) == ContextReading(
        23, 100, 23, "codex-test-model"
    )
    assert calls == [
        ("context-run", ["--probe", "--transcript", str(transcript), "--json"], 5)
    ]


def test_probe_rejects_malformed_native_receipts(monkeypatch, tmp_path):
    monkeypatch.setattr(
        "fno.rust_binary.call_binary_json",
        lambda *a, **k: (None, {"used_tokens": 23}),
    )

    assert probe_context(transcript_path=tmp_path / "t.jsonl") is None
