"""The capability-table config override (x-244c): whole-row, both readers.

Cases: AC1-HP (a config row corrects a bundled row with no code change),
AC1-ERR (a malformed override keeps the bundled row and names itself),
AC2-HP (precedence: project-local before global, first candidate wins per
name). The Rust reader must answer the SAME overrides - the resolved-row
parity cases live with the Rust reader's tests; these pin the Python
contract.
"""

from __future__ import annotations

from copy import deepcopy
from pathlib import Path

import pytest

from fno.agents import harness_map

# A full valid `direct` strategy for agy, the live specimen the probe uses.
AGY_DIRECT = """
[harness.agy.model_switch_strategy]
kind = "direct"
tokens = ["--model {model}", "--effort {effort}"]
effort_labels = {}
status_command = "/status"
status_pattern = 'Model:\\s+(?P<model>\\S+).*?effort\\s+(?P<effort>low|medium|high)'
"""


@pytest.fixture
def bundled_state():
    """Snapshot the module map + warnings; restore after the test."""
    rows = deepcopy(harness_map._HARNESS_CAPS)
    warnings = list(harness_map.OVERRIDE_WARNINGS)
    yield
    harness_map._HARNESS_CAPS.clear()
    harness_map._HARNESS_CAPS.update(rows)
    harness_map.OVERRIDE_WARNINGS[:] = warnings


def _write(path: Path, body: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    return path


def test_malformed_override_keeps_bundled_row_and_names_itself(
    monkeypatch, tmp_path: Path, bundled_state
) -> None:
    """AC1-ERR: a typo in one harness's block never un-configures it."""
    bad = AGY_DIRECT.replace('status_command = "/status"', "status_command = 'status'")
    _write(tmp_path / "project/.fno/config.toml", bad)
    monkeypatch.chdir(tmp_path / "project")
    monkeypatch.delenv("FNO_GLOBAL_SETTINGS_PATH", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path / "home"))

    harness_map.reload_capability_overrides()

    assert harness_map.capabilities("agy")["model_switch_strategy"]["kind"] == "unsupported"
    assert len(harness_map.OVERRIDE_WARNINGS) == 1
    assert "agy" in harness_map.OVERRIDE_WARNINGS[0]
    assert "config.toml" in harness_map.OVERRIDE_WARNINGS[0]


def test_override_for_unknown_roster_name_is_refused_by_name(
    monkeypatch, tmp_path: Path, bundled_state
) -> None:
    _write(
        tmp_path / "project/.fno/config.toml",
        "[harness.brandnewharness]\nunheard_of_field = true\n",
    )
    monkeypatch.chdir(tmp_path / "project")
    monkeypatch.delenv("FNO_GLOBAL_SETTINGS_PATH", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path / "home"))

    harness_map.reload_capability_overrides()

    assert "brandnewharness" not in harness_map._HARNESS_CAPS
    assert not harness_map.is_declared("brandnewharness")
    assert any("brandnewharness" in w for w in harness_map.OVERRIDE_WARNINGS)


def test_project_local_wins_over_global_per_name(
    monkeypatch, tmp_path: Path, bundled_state
) -> None:
    """AC2-HP: project-local before global; first candidate wins per name."""
    _write(tmp_path / "project/.fno/config.toml", AGY_DIRECT)
    global_cfg = (
        "[harness.agy.model_switch_strategy]\n"
        'kind = "menu_walk"\n'
        'tokens = ["/model {model}", "/effort {effort_label}"]\n'
        "effort_labels = { low = '3', medium = '2', high = '2', xhigh = '2', max = '2' }\n"
        'status_command = "/status"\n'
        "status_pattern = 'x'\n"
    )
    # The env var names a settings path whose SIBLING config.toml is the
    # global candidate, the same resolution the Rust reader uses.
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(tmp_path / "global/settings.yaml"))
    _write(tmp_path / "global/config.toml", global_cfg)
    monkeypatch.chdir(tmp_path / "project")
    monkeypatch.setenv("HOME", str(tmp_path / "home"))

    harness_map.reload_capability_overrides()

    strategy = harness_map.capabilities("agy")["model_switch_strategy"]
    assert strategy["kind"] == "direct"
    assert len([w for w in harness_map.OVERRIDE_WARNINGS if "agy" in w]) == 0

