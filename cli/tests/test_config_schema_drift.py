"""Anti-drift guards for the unified config schema.

Mirrors the repo's `fno doctor bundle check` pattern: the model is the single source
of truth, and these tests fail CI the moment a derived artifact drifts.

Three guards plus the outage floors:
  1. registry completeness  - every model leaf has exactly one registry entry.
  2. docs freshness         - docs/configuration-guide.md and
                              docs/config.example.toml match the generator.
  3. bash-default equality  - each `get_config "K" "D"` whose config.K is a
                              modeled leaf has D equal to the model default.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

from fno.config import RecoveryBlock
from fno.config import registry as _registry
from fno.config import schema_gen


def _repo_root() -> Path:
    """Walk up from this test file until docs/configuration-guide.md is found."""
    here = Path(__file__).resolve()
    for parent in here.parents:
        if (parent / "docs" / "configuration-guide.md").exists():
            return parent
    raise AssertionError("could not locate repo root (docs/configuration-guide.md)")


def test_registry_is_complete_and_exact() -> None:
    """Every model leaf has a registry entry and vice-versa (the drift-killer)."""
    leaves = set(schema_gen.all_leaf_paths())
    reg = set(_registry.FIELD_META)
    missing = leaves - reg
    extra = reg - leaves
    assert not missing, f"model leaves missing a registry entry: {sorted(missing)}"
    assert not extra, f"registry entries with no model leaf: {sorted(extra)}"


def test_provider_outage_config_defaults_and_minima_are_fail_closed() -> None:
    defaults = RecoveryBlock()
    assert defaults.watchdog.enabled is False
    assert defaults.watchdog.mode == "report"
    # The DEFAULT stays 2; the FLOOR is 1 - quorum=1 is an operator's positive
    # decision that a lone worker's outage is authority enough.
    assert defaults.provider_outage_quorum == 2
    assert RecoveryBlock(provider_outage_quorum=1).provider_outage_quorum == 1
    assert defaults.provider_outage_fup_window_seconds == 300
    assert defaults.provider_outage_529_count == 3
    assert defaults.provider_outage_529_span_seconds == 120
    assert defaults.provider_outage_529_cross_session_window_seconds == 600
    assert defaults.provider_outage_pane_freshness_seconds == 120
    assert defaults.provider_outage_evidence_freshness_seconds == 600
    assert defaults.provider_outage_reset_grace_seconds == 120
    assert defaults.provider_health_marker_ttl_seconds == 120
    # The flat spelling every config written before the split carries.
    legacy = RecoveryBlock(watchdog="handoff")
    assert (legacy.watchdog.enabled, legacy.watchdog.mode) == (True, "handoff")
    assert RecoveryBlock(watchdog="off").watchdog.enabled is False

    for field, value in {
        "provider_outage_quorum": 0,
        "provider_outage_fup_window_seconds": 299,
        "provider_outage_529_count": 2,
        "provider_outage_529_span_seconds": 119,
        "provider_outage_529_cross_session_window_seconds": 599,
        "provider_outage_pane_freshness_seconds": 119,
        "provider_outage_evidence_freshness_seconds": 599,
        "provider_outage_reset_grace_seconds": 119,
        "provider_health_marker_ttl_seconds": 119,
    }.items():
        with pytest.raises(ValueError, match=field):
            RecoveryBlock.model_validate({field: value})


def test_committed_docs_are_fresh() -> None:
    """docs/configuration-guide.md must equal the generator's output.

    Regenerate with `fno config schema --markdown --write`.
    """
    docs = _repo_root() / "docs" / "configuration-guide.md"
    committed = docs.read_text(encoding="utf-8")
    assert committed == schema_gen.render_markdown(), (
        "docs/configuration-guide.md is stale; run "
        "`fno config schema --markdown --write`"
    )


def test_committed_example_toml_is_fresh() -> None:
    """docs/config.example.toml must equal the generator's output.

    Regenerate with `fno config schema --toml --write`.
    """
    example = _repo_root() / "docs" / "config.example.toml"
    committed = example.read_text(encoding="utf-8")
    assert committed == schema_gen.render_example_toml(), (
        "docs/config.example.toml is stale; run "
        "`fno config schema --toml --write`"
    )


def _model_defaults() -> dict[str, object]:
    """Map config.<dotted> -> model default for every leaf (bash reads config.K)."""
    return {leaf.path: leaf.default for leaf in schema_gen.iter_leaves()}


def _normalize(value: object) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return ""
    if isinstance(value, (list, dict)):
        return "" if not value else str(value)
    return str(value)


def test_bash_get_config_defaults_match_model() -> None:
    """For each `get_config "K" "D"` whose config.K is a modeled leaf, D must
    equal the model default (cheap dual-reader drift guard).

    Keys that are NOT modeled leaves (legacy / session-input / dead) are
    skipped - this guard only protects the keys that exist in both readers.
    """
    root = _repo_root()
    defaults = _model_defaults()
    pattern = re.compile(
        r'get_config(?:_or_default)?\s+"([a-zA-Z0-9_.]+)"\s+"([^"]*)"'
    )
    mismatches: list[str] = []
    checked = 0
    for sub in ("scripts", "skills", "hooks"):
        base = root / sub
        if not base.is_dir():
            continue
        for path in base.rglob("*.sh"):
            if "__pycache__" in str(path):
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except OSError:
                continue
            for key, bash_default in pattern.findall(text):
                # Model leaves are flat now; a bash key may carry the legacy
                # `config.` prefix or not - normalize either to the flat leaf.
                leaf = key[len("config.") :] if key.startswith("config.") else key
                if leaf not in defaults:
                    continue  # not a modeled leaf; out of scope for this guard
                # A key-PRESENCE probe deliberately passes an empty default so
                # unset reads as "" (the caller distinguishes set from unset).
                # It is exempt by its trailing marker, never by evasion: the
                # marker is the reviewable claim that this is a probe.
                probe_line = next(
                    (
                        ln
                        for ln in text.splitlines()
                        if f'get_config "{key}"' in ln and "presence-probe" in ln
                    ),
                    None,
                )
                if probe_line:
                    continue
                checked += 1
                if _normalize(defaults[leaf]) != bash_default:
                    mismatches.append(
                        f"{path.relative_to(root)}: get_config \"{key}\" "
                        f"default={bash_default!r} != model default "
                        f"{_normalize(defaults[leaf])!r}"
                    )
    assert checked > 0, "no modeled get_config defaults found to check (guard inert?)"
    assert not mismatches, "bash/model default drift:\n" + "\n".join(mismatches)
