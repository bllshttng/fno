"""Anti-drift guards for the unified config schema.

Mirrors the repo's `fno doctor bundle check` pattern: the model is the single source
of truth, and these tests fail CI the moment a derived artifact drifts.

Three guards plus the outage floors:
  1. registry completeness  - every model leaf has exactly one registry entry.
  2. table freshness        - crates/fno/config_schema.json matches the model.
                              The native `fno config schema` renders the docs
                              from it, and a crates/fno test holds the docs.
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


def test_committed_schema_table_is_fresh() -> None:
    """crates/fno/config_schema.json must equal the table the model generates.

    Regenerate with `cd cli && uv run python -m fno.config.schema_gen --write`.
    """
    table = _repo_root() / "crates" / "fno" / "config_schema.json"
    committed = table.read_text(encoding="utf-8")
    assert committed == schema_gen.table(), (
        "crates/fno/config_schema.json is stale; run "
        "`cd cli && uv run python -m fno.config.schema_gen --write`"
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


def test_native_set_accepts_github_app_keys(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The compiled front door's `config set -g` accepts the three github.*
    keys in one batch and lands them in the global config.toml.

    The schema guards above prove the model side; this proves the writer side
    reads the same table, the way the GitHub App token path needs it to.
    """
    import subprocess
    import tomllib

    from tests.unit._front_dev import front_dev_binary

    front = front_dev_binary()
    if front is None:
        pytest.skip("compiled fno front door not present (build with `cargo build -p fno`)")
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    for key in ("FNO_CONFIG", "FNO_GLOBAL_SETTINGS_PATH", "FNO_STATE_DIR"):
        monkeypatch.delenv(key, raising=False)

    keys = {
        "github.app_id": "1",
        "github.app_installation_id": "42",
        "github.app_key_path": "~/.fno/secrets/github-app.pem",
    }
    proc = subprocess.run(
        [str(front), "config", "set", "-g", *[f"{k}={v}" for k, v in keys.items()]],
        capture_output=True,
        text=True,
        cwd=str(home),
        timeout=120,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr or proc.stdout

    config = tomllib.loads((home / ".fno" / "config.toml").read_text())
    assert config["github"] == {
        "app_id": "1",
        "app_installation_id": "42",
        "app_key_path": "~/.fno/secrets/github-app.pem",
    }
