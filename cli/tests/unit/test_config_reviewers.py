"""Tests for the `_RESOLVABLE_REVIEWERS` descriptor table (node x-cdc7).

The table was a bare name set. A name answers "is this reviewer spelled
correctly"; it cannot answer "can this reviewer actually run, here", which is
the question that let a `reviewers: [sigma]` gate go unsatisfiable-by-
construction on PR #618 and surface only at the stop gate, after the work.

Promoting the set to a table must not change what the validator accepts or the
message it prints on a typo - that is a separate, already-tested contract
(test_config_review.py). These tests pin the new value shape, and the one rule
that must never soften: `declare` is visibly a self-certification.
"""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.config import (
    _RESOLVABLE_REVIEWERS,
    load_settings,
    resolvable_reviewers,
)


def _settings(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, body: str):
    cfg = tmp_path / "settings.yaml"
    cfg.write_text(body)
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    return load_settings()


def test_declare_is_the_only_self_cert():
    """AC5: `declare` satisfies the gate while asserting nothing, so every
    surface that prints it can tell that apart from a real review."""
    self_cert = {n for n, d in _RESOLVABLE_REVIEWERS.items() if d.asserts == "self-cert"}
    assert self_cert == {"declare"}
    assert _RESOLVABLE_REVIEWERS["declare"].requires == "none"


def test_code_review_is_self_servable():
    """`/code-review` is self-servable: a session that wrote the diff runs the
    owned fno review lane and attests. requires=none (not operator) is what
    keeps an unattended run from being refused at init for naming it.

    No per-harness map: the owned lane is the invocation on every harness,
    which is the portability the sigma-retirement recharter bought. The
    harness's own native command (claude /code-review, codex /review) remains
    the operator's explicit choice (docs/architecture/review-lanes.md), never
    something this table recommends, since a recommendation depending on a
    harness transport is the fragile part the recharter removed."""
    d = _RESOLVABLE_REVIEWERS["code-review"]
    assert d.kind == "local-attestation"
    assert d.requires == "none"
    assert d.invocations is None
    assert d.invocation == "/fno:review"


def test_validator_rejection_message_lists_the_names(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The typo message must keep naming the allowed reviewers, not repr the
    descriptors - `sorted()` over a mapping yields keys, which is why the
    validator needed no edit."""
    with pytest.raises(Exception) as exc:
        _settings(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  review:\n    reviewers: [teleport]\n",
        )
    assert "['code-review', 'declare', 'sigma']" in str(exc.value)


# --- `fno config doctor --review`, the diagnostic twin of the init refusal ---


def _doctor_review(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, reviewers: str, env: dict):
    cfg = tmp_path / "settings.yaml"
    cfg.write_text(f"schema_version: 1\nconfig:\n  review:\n    reviewers: {reviewers}\n")
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    for var in ("CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID", "CODEX_SESSION_ID",
                "GEMINI_SESSION_ID", "TARGET_UNATTENDED", "FNO_BG", "FNO_AGENT_SELF"):
        monkeypatch.delenv(var, raising=False)
    for k, v in env.items():
        monkeypatch.setenv(k, v)
    from fno.cli import app

    return CliRunner().invoke(app, ["config", "doctor", "--review"])


def test_doctor_review_reports_sigma_retired(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """A retired reviewer is unavailable on every session, attended or not -
    unlike a capability gap, no session can ever satisfy it."""
    r = _doctor_review(tmp_path, monkeypatch, "[sigma]", {"CLAUDE_CODE_SESSION_ID": "s1"})
    assert r.exit_code == 1
    assert "unavailable: sigma" in r.output
    assert "sigma is retired" in r.output
    assert "run `/fno:review`" in r.output
    assert "`declare` is never substituted for you" in r.output


def _doctor_peers(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    peers_toml: str,
    env: dict[str, str],
):
    cfg = tmp_path / "config.toml"
    cfg.write_text(f"[review]\npeers = {peers_toml}\n")
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    for var in (
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
        "TARGET_UNATTENDED",
        "FNO_BG",
        "FNO_AGENT_SELF",
    ):
        monkeypatch.delenv(var, raising=False)
    for key, value in env.items():
        monkeypatch.setenv(key, value)
    from fno.cli import app

    return CliRunner().invoke(app, ["config", "doctor", "--review"])


def test_doctor_review_reports_identity_free_cross_model_peer(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    r = _doctor_peers(
        tmp_path,
        monkeypatch,
        '["codex", {provider = "claude", model = "zai,glm-5.2"}]',
        {"CODEX_THREAD_ID": "c1"},
    )
    assert r.exit_code == 0, r.output
    assert "local peer gate: satisfiable" in r.output
    assert "claude via zai,glm-5.2" in r.output
    assert "codex" in r.output and "same model" in r.output


def test_doctor_review_refuses_only_same_model_local_peer(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    r = _doctor_peers(
        tmp_path,
        monkeypatch,
        '["codex"]',
        {"CODEX_THREAD_ID": "c1"},
    )
    assert r.exit_code == 1
    assert "local peer gate: unavailable" in r.output
    assert "same model" in r.output


# --- the refusal message's two hints (AC10-ERR / AC11-ERR / AC12-HP) ---


def test_a_near_miss_names_the_reviewer_it_resembles(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """AC10-ERR."""
    with pytest.raises(Exception) as exc:
        _settings(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  review:\n    reviewers: [sgima]\n",
        )
    assert "Did you mean 'sigma'?" in str(exc.value)


def test_a_wrong_key_value_points_at_the_adjacent_key(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """AC11-ERR: `coderabbit` belongs to external_reviewers, not this gate."""
    with pytest.raises(Exception) as exc:
        _settings(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  review:\n    reviewers: [coderabbit]\n",
        )
    assert "config.review.external_reviewers" in str(exc.value)


# --- config.review.reviewer_registry (AC9-INV) ---


def _toml_settings(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, body: str):
    """Load a FLAT config.toml - the real file shape (no `config:` wrapper)."""
    cfg = tmp_path / "config.toml"
    cfg.write_text(body)
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    # The declaration key is unchanged by a content rewrite; drop the entry.
    from fno.config import _load_settings_at

    _load_settings_at.cache_clear()
    return load_settings()


_REGISTRY_TOML = """\
[review]
reviewers = ["/my-security-skill"]

[review.reviewer_registry.my-security-skill]
kind = "harness-skill"
requires = "skill"
invocation = "/my-security-skill"
asserts = "invocation"
"""


def test_a_registered_reviewer_resolves(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    s = _toml_settings(tmp_path, monkeypatch, _REGISTRY_TOML)
    assert s.review.reviewers == ["my-security-skill"]
    d = s.review.reviewer_registry["my-security-skill"]
    assert (d.kind, d.requires, d.asserts) == ("harness-skill", "skill", "invocation")


def test_a_registry_entry_cannot_shadow_a_builtin(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """A project redefining `sigma` as a self-cert would weaken a shipped gate."""
    s = _toml_settings(
        tmp_path,
        monkeypatch,
        '[review.reviewer_registry.sigma]\nkind = "harness-skill"\n'
        'requires = "none"\ninvocation = "/nope"\nasserts = "self-cert"\n',
    )
    assert resolvable_reviewers(s.review.reviewer_registry)["sigma"] is (
        _RESOLVABLE_REVIEWERS["sigma"]
    )


def test_a_wrong_typed_probe_list_raises(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """Mirrors the Rust side's Unparseable: a guardrail that disappears when you
    typo it is not a guardrail."""
    with pytest.raises(Exception):
        _toml_settings(tmp_path, monkeypatch, '[done_probes]\na = "b"\n')
