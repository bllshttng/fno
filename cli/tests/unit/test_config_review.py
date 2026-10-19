"""Tests for config.review - the loop-check review-gate config block.

Covers the Python half of control-plane step 2 (ab-f1c5a9ed): the
`config.review.github_apps` schema (x-4baa; `required_bots` is now a legacy
alias). The authoritative consumer is the Rust `fno-agents loop-check` verb
(its own hand-rolled parser is tested in crates/fno-agents); this block exists
so `fno config get` and the Pydantic schema agree on the key's shape and
fail-closed semantics.

Semantics under test:
  - key absent  -> None (no review gate; the Rust effective default is [],
    cv-6537099f - Python must not invent a default list)
  - explicit [] -> [] (the declared no-review-gate path, US3)
  - non-list    -> None + warning (fail closed, AC3-ERR)
  - required_bots is a legacy alias for github_apps (github_apps wins if both)
  - peers scalar-or-map coercion; a map missing `provider` fails LOUD
  - identity-free peers select the head-pinned local-attestation gate
  - explicit peer identities preserve the legacy posted-GitHub-review gate
"""
from __future__ import annotations

from pathlib import Path

import pytest


def _write_settings(tmp_path: Path, content: str) -> Path:
    settings_dir = tmp_path / ".fno"
    settings_dir.mkdir(parents=True, exist_ok=True)
    settings_file = settings_dir / "settings.yaml"
    settings_file.write_text(content, encoding="utf-8")
    return settings_file


def _load(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, content: str):
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    settings_file = _write_settings(tmp_path, content)
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))

    from fno import config as config_mod

    # The declaration key is unchanged by a content rewrite; drop the entry.
    config_mod._load_settings_at.cache_clear()
    return config_mod.load_settings()


def test_review_required_bots_explicit_empty(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Explicit [] is preserved (declared no-review-gate, distinct from absent)."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n    required_bots: []\n",
    )
    assert settings.review.required_bots == []


def test_review_required_bots_scalar_is_singleton(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A bare scalar gates on that one login (parity with the Rust reader); a
    bracket-less typo must not fail OPEN to no-gate (codex P1 on #205)."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n    required_bots: gemini\n",
    )
    assert settings.review.required_bots == ["gemini"]
    assert settings.review.github_apps == ["gemini"]


def test_review_bare_key_fails_closed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A bare `required_bots:` (YAML null) must not disable the gate."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n    required_bots:\n",
    )
    assert settings.review.required_bots is None


# --- Cross-model review panel: agent_providers + cross_model (ab-6c8f4c61) ---


def test_review_cross_model_enabled_non_bool_fails_safe(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A non-boolean enabled coerces to False (false-enabled is the dangerous way)."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n    cross_model:\n      enabled: banana\n",
    )
    assert settings.review.cross_model.enabled is False


# --- github_apps rename + required_bots alias (x-4baa US1) ---


def test_github_apps_wins_over_required_bots(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """When both are set, github_apps wins (Locked Decision 2)."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n"
        "    github_apps: [new-bot]\n    required_bots: [old-bot]\n",
    )
    assert settings.review.github_apps == ["new-bot"]
    assert settings.review.required_bots == ["new-bot"]


def test_github_apps_absent_is_no_gate(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Absent -> None (no gate); the old chatgpt-codex-connector default is gone."""
    settings = _load(tmp_path, monkeypatch, "schema_version: 1\n")
    assert settings.review.github_apps is None


# --- peers / peer_identity / peer_token_env (x-4baa US2) ---


def test_peers_map_missing_provider_fails_loud(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A map entry with no `provider` is a loud config error, not a silent skip."""
    with pytest.raises(Exception, match="provider"):
        _load(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  review:\n    peers:\n"
            "      - identity: fno-codex-bot\n    peer_identity: x\n",
        )


def test_peers_without_identity_selects_local_attestation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A peer is another harness; no second GitHub identity is required."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n    peers: [codex]\n",
    )
    assert settings.review.peers == ["codex"]
    assert settings.review.peer_identity is None


def test_routed_claude_peer_without_identity_loads_for_local_attestation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n    peers:\n"
        '      - provider: claude\n        model: "zai,glm-5.2"\n',
    )
    assert settings.review.peers == [
        {"provider": "claude", "model": "zai,glm-5.2"}
    ]


# --- claude->routed-model peer lane (x-ef41) ---


def test_peers_bare_claude_no_model_fails_loud(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC-ERR: a claude peer with no model route is the author's own model ->
    reject at load, citing the distinct-model trust invariant."""
    with pytest.raises(Exception, match="distinct-model"):
        _load(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  review:\n    peers:\n"
            "      - provider: claude\n    peer_identity: fno-peer-bot\n",
        )


@pytest.mark.parametrize("route", ["anthropic,claude-opus", "claude,sonnet"])
def test_peers_claude_same_model_route_fails_loud(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, route: str
) -> None:
    """A claude peer routing back to the author's own provider (anthropic/claude)
    is not a distinct model -> rejected at load (defense-in-depth: the runtime
    also fail-safes an unknown provider to None)."""
    with pytest.raises(Exception, match="distinct-model"):
        _load(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  review:\n    peers:\n"
            f'      - provider: claude\n        model: "{route}"\n'
            "    peer_identity: fno-peer-bot\n",
        )


# --- optional_apps: honored-if-present, never required (x-4baa) ---


def test_optional_apps_defaults_empty(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Absent -> the RAW field stays None (the lane probe reads its
    truthiness) while the RESOLVED list carries the built-in honored-if-present
    logins, independent of the required gate."""
    from fno.config import DEFAULT_OPTIONAL_APPS, resolved_optional_apps

    settings = _load(tmp_path, monkeypatch, "schema_version: 1\n")
    assert settings.review.optional_apps is None
    assert resolved_optional_apps(settings.review) == list(DEFAULT_OPTIONAL_APPS)


# --- parser parity on non-string malformed values (codex P1 on #205) ---


def test_github_apps_numeric_scalar_gates_closed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A stray numeric scalar becomes a (never-matching) singleton, matching the
    Rust text reader - a required-gate typo fails CLOSED, not open to no-gate."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n    github_apps: 123\n",
    )
    assert settings.review.github_apps == ["123"]


# --- reviewers: local-attestation gate (x-e703, Phase 2) ---


def test_reviewers_strips_leading_slash(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`/code-review` and `code-review` are the same reviewer (slash stripped)."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n    reviewers: [/code-review, declare]\n",
    )
    assert settings.review.reviewers == ["code-review", "declare"]


def test_reviewers_unresolvable_fails_loud(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An unresolvable reviewer name raises loudly naming it (AC2-ERR / AC3-ERR):
    a typo must never silently drop to a never-green gate."""
    with pytest.raises(Exception) as excinfo:
        _load(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  review:\n    reviewers: [teleport]\n",
        )
    assert "teleport" in str(excinfo.value)


# --- agent_harnesses rename + agent_providers alias ---


def test_agent_harnesses_wins_over_agent_providers(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Both set: canonical wins and the two are never merged."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  review:\n"
        "    agent_harnesses: {code-reviewer: codex}\n"
        "    agent_providers: {silent-failure-hunter: gemini}\n",
    )
    assert settings.review.agent_harnesses == {"code-reviewer": "codex"}
    assert "silent-failure-hunter" not in settings.review.agent_harnesses


# --- AC6: a non-author GitHub approval is a sufficient producer ---


def test_github_approval_satisfies_defaults_on(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Absent key -> True: the one producer a stranger's GitHub project emits
    with no footnote machinery ships ON."""
    settings = _load(tmp_path, monkeypatch, "schema_version: 1\nconfig:\n  review: {}\n")
    assert settings.review.github_approval_satisfies is True


def _verdict(name: str, author_approval: bool | None) -> dict:
    row = {
        "producer": "github_app",
        "name": name,
        "verdict": "reviewed",
        "human_approval": True,
        "reviewed_sha": "h1",
        "freshness": "fresh",
    }
    if author_approval is not None:
        row["author_approval"] = author_approval
    return row


def test_ac6_hp_non_author_approval_derives_reviewed() -> None:
    """bob's approval on alice's PR, flag on: the state derivation counts it."""
    from fno.pr._reviews import _derive_review_state

    assert (
        _derive_review_state("covered", [_verdict("bob", False)], set(), True) == "reviewed"
    )


def test_ac6_err_author_approval_is_never_counted() -> None:
    """alice approving her own PR stays recorded and never derives reviewed,
    so a run where no approval was collected at all cannot pass this either."""
    from fno.pr._reviews import _derive_review_state

    assert (
        _derive_review_state("covered", [_verdict("alice", True)], set(), True)
        == "unreviewed"
    )


def test_ac6_pre_field_row_stays_excluded_even_with_flag_on() -> None:
    """A verdict serialized before author_approval existed carries no marker;
    the absent marker reads as the exclude side (fail-closed), flag or not."""
    from fno.pr._reviews import _derive_review_state

    assert (
        _derive_review_state("covered", [_verdict("bob", None)], set(), True)
        == "unreviewed"
    )


# --- AC7: the round budget config ---


def test_max_rounds_below_one_fails_loud(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Zero or negative fails the load, never silently zeroes the budget - a
    missing cap would let every round refund into an unbounded review."""
    with pytest.raises(Exception):
        _load(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  review:\n    max_rounds: 0\n",
        )


# --- review.posture: the nine-rung ladder ---


def _review(content: str):
    """A ReviewBlock parsed from a review-block YAML fragment."""
    from fno.config import ReviewBlock

    import yaml as _yaml

    return ReviewBlock(**_yaml.safe_load(content))


def test_posture_defaults_unset_and_bare_install_is_default_floor() -> None:
    """No posture leaf anywhere: the shipped default is self_review (rung 3),
    source=default, and NO migration command (nothing was inferred)."""
    from fno.config import resolve_review_posture

    block = _review("{}")
    assert block.posture is None
    resolved = resolve_review_posture(block)
    assert resolved.value == "self_review"
    assert resolved.rank == 3
    assert resolved.source == "default"
    assert resolved.migration is None
    assert not resolved.automerge_blocked


def test_posture_typo_fails_loud() -> None:
    """A misspelled rung must never read as unset (unset infers and can only
    land on a real rung; a typo falling through would ship a different rung
    than the operator wrote)."""
    import pytest as _pytest

    from fno.config import ReviewBlock

    with _pytest.raises(Exception, match="not one of"):
        ReviewBlock(posture="self_reviewk")


def test_posture_legacy_github_app_infers_self_and_github() -> None:
    """The shipped self_review_required floor composes with the App gate."""
    from fno.config import resolve_review_posture

    resolved = resolve_review_posture(
        _review("github_apps: [chatgpt-codex-connector]\n")
    )
    assert resolved.value == "self_and_github"
    assert resolved.source == "legacy"
    assert resolved.migration == (
        "fno config set review.posture self_and_github --local"
    )
    assert resolved.signals == ("github_apps", "self_review_required")


def test_posture_legacy_declared_none_infers_tests_pass() -> None:
    """An explicit [] gate is the declared PR+CI-only path: rung 2, not 1."""
    from fno.config import resolve_review_posture

    resolved = resolve_review_posture(_review("required_bots: []\n"))
    assert resolved.value == "tests_pass"
    assert resolved.source == "legacy"
    assert resolved.automerge_blocked


def test_posture_legacy_explicit_floor_optout_infers_no_review() -> None:
    """An explicit self_review_required=false with no other lane read as
    no-gate before the ladder existed; the inference preserves the opt-out
    visibly instead of silently hardening it."""
    from fno.config import resolve_review_posture

    resolved = resolve_review_posture(_review("self_review_required: false\n"))
    assert resolved.value == "no_review"
    assert resolved.automerge_blocked
    assert resolved.migration == "fno config set review.posture no_review --local"


def test_posture_legacy_floor_off_downgrades_composition() -> None:
    """An explicit floor opt-out keeps the inference from claiming a self lane
    the config explicitly declined."""
    from fno.config import resolve_review_posture

    resolved = resolve_review_posture(
        _review("github_apps: [codex]\nself_review_required: false\n")
    )
    assert resolved.value == "github_review"


def test_posture_sigma_and_declare_never_count_as_a_self_lane() -> None:
    """sigma is retired (refuses at init) and declare is a self-cert; neither
    may make a legacy config LOOK reviewed."""
    from fno.config import resolve_review_posture

    resolved = resolve_review_posture(_review("reviewers: [sigma, declare]\n"))
    assert resolved.value == "self_review"
    assert resolved.source == "default"
    assert "reviewers" not in resolved.signals
