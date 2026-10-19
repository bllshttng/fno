"""Unit tests for the shared dispatch provider/model resolver.

Covers the precedence chain, harness inference, and the empty-flag rejections
behind AC1-ERR / AC2-ERR / AC1-EDGE.
"""

from __future__ import annotations

import pytest

from fno.dispatch_flags import (
    DispatchFlagError,
    infer_invoking_harness,
    reject_empty_model,
    resolve_dispatch_harness,
)
_ALL_MARKERS = (
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
)


@pytest.fixture(autouse=True)
def _clear_harness_env(monkeypatch):
    """Every test starts with no ambient harness marker set."""
    for m in _ALL_MARKERS:
        monkeypatch.delenv(m, raising=False)


# --- explicit provider (top of precedence) --------------------------------


def test_explicit_beats_harness_marker():
    # AC2-EDGE-adjacent: an explicit flag outranks the inferred harness.
    env = {"CODEX_SESSION_ID": "abc"}
    assert resolve_dispatch_harness("claude", env=env) == ("claude", "explicit")


def test_explicit_empty_provider_rejected():
    with pytest.raises(DispatchFlagError):
        resolve_dispatch_harness("   ")


# --- builtin default (bottom) ---------------------------------------------


def test_multiple_markers_are_ambiguous():
    # Inference never guesses: two markers -> None (caller falls to builtin).
    env = {"CODEX_SESSION_ID": "b", "GEMINI_SESSION_ID": "c"}
    assert infer_invoking_harness(env) is None
    assert resolve_dispatch_harness(None, env=env) == ("claude", "builtin-default")


# --- harness inference (middle) -------------------------------------------


def test_canonical_harness_name_drives_inference():
    assert infer_invoking_harness({"FNO_HARNESS_NAME": "codex"}) == "codex"
    assert resolve_dispatch_harness(None, env={"FNO_HARNESS_NAME": "codex"}) == (
        "codex",
        "harness-inferred",
    )


# --- model flag validation -------------------------------------------------


def test_empty_model_rejected():
    # AC2-ERR: empty/whitespace model is a usage error.
    with pytest.raises(DispatchFlagError):
        reject_empty_model("   ")


def test_model_passthrough():
    # An absent --model is not an error, and a present one survives verbatim
    # (no fuzzy resolve of dots/colons/commas).
    assert reject_empty_model(None) is None
    assert reject_empty_model("zai,glm-5.2") == "zai,glm-5.2"
