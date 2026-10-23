"""The external workflow-pack delivery journey (AC2-HP / AC2-EDGE).

A pack authored OUTSIDE the core checkout installs through the existing
verify/activate/deactivate owner into an isolated state root, runs its
declared evaluator for real, upgrades, and deactivates with ownership
preserved. The journey keeps the evaluator's actual exit code and positive
marker.

The other product journeys (workspace conformance, context binding,
acceptance evidence) stay in their own suites; this file composes the pack
owner, never a second manifest checker.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest
import yaml
from typer.testing import CliRunner

from fno.plugins.cli import plugins_app

# Role resolution runs in fno-agents (spawn-overlay kind roles-resolve).
pytestmark = pytest.mark.dev_build

FIXTURE_DIR = Path(__file__).resolve().parents[1] / "fixtures" / "external_workflow_pack"
POSITIVE_MARKER = "EVALUATOR PASS"
PACK_ID = "external-workflow-pack"

runner = CliRunner()


def _materialize_external_pack(dest: Path) -> Path:
    """Copy the fixture pack to a source outside the core checkout and render
    its plugin.yaml from the JSON declaration."""
    dest.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((FIXTURE_DIR / "pack.json").read_text(encoding="utf-8"))
    (dest / "plugin.yaml").write_text(yaml.safe_dump(manifest), encoding="utf-8")
    (dest / "workflows").mkdir(exist_ok=True)
    (dest / "workflows" / "handoff.md").write_text("# handoff workflow\n", encoding="utf-8")
    shutil.copy(FIXTURE_DIR / "evaluate.py", dest / "evaluate.py")
    (dest / "evaluate.py").chmod(0o755)
    return dest


def _run_evaluator(pack_dir: Path) -> subprocess.CompletedProcess[str]:
    """Run the pack's DECLARED evaluator command, never a hardcoded one."""
    manifest = yaml.safe_load((pack_dir / "plugin.yaml").read_text(encoding="utf-8"))
    command = manifest["evaluators"][0]["command"]
    return subprocess.run(
        command.split(), cwd=pack_dir, capture_output=True, text=True, timeout=30,
    )


def _plugins(root: Path, *args: str) -> dict:
    res = runner.invoke(plugins_app, ["--root", str(root), *args, "--json"])
    assert res.exit_code == 0, res.output
    return json.loads(res.output)


def _isolated_root(tmp_path: Path) -> Path:
    root = tmp_path / "isolated-state"
    root.mkdir()
    return root


# --- AC2-HP: the whole journey on one pack -----------------------------------


def test_full_pack_journey_verify_activate_evaluate_upgrade_deactivate(tmp_path: Path) -> None:
    root = _isolated_root(tmp_path)
    pack = _materialize_external_pack(tmp_path / "outside-checkout" / PACK_ID)

    verified = _plugins(root, "verify", str(pack))
    assert verified["ok"] is True
    assert all(c["checked"] for c in verified["conditions"])

    activated = _plugins(root, "activate", str(pack))
    receipt = activated
    assert receipt["pack_id"] == PACK_ID
    assert receipt["resolved_version"] == "1.0.0"
    assert receipt["written_paths"], "activation must leave owned-file receipts"
    for written in receipt["written_paths"]:
        assert (root / written).is_file(), written

    evidence = _run_evaluator(pack)
    assert evidence.returncode == 0, evidence.stderr
    assert POSITIVE_MARKER in evidence.stdout

    # Upgrade: a new version of the same pack id re-activates over the old one.
    manifest = yaml.safe_load((pack / "plugin.yaml").read_text(encoding="utf-8"))
    manifest["version"] = "2.0.0"
    (pack / "plugin.yaml").write_text(yaml.safe_dump(manifest), encoding="utf-8")
    upgraded = _plugins(root, "activate", str(pack))
    assert upgraded["resolved_version"] == "2.0.0"
    assert upgraded["pack_digest"] != receipt["pack_digest"]
    assert upgraded["already_active"] is False

    listing = _plugins(root, "ls")
    assert listing["packs"][0]["version"] == "2.0.0"
    assert listing["packs"][0]["activated"] is True

    deactivated = _plugins(root, "deactivate", PACK_ID)
    assert sorted(deactivated["removed"]) == sorted(upgraded["written_paths"])
    assert deactivated["left_alone"] == []
    for written in upgraded["written_paths"]:
        assert not (root / written).exists(), written


# --- AC2-EDGE: refusals preserve recoverable ownership ------------------------


def test_incompatible_pack_is_refused_and_writes_nothing(tmp_path: Path) -> None:
    root = _isolated_root(tmp_path)
    pack = _materialize_external_pack(tmp_path / "outside-checkout" / PACK_ID)
    manifest = yaml.safe_load((pack / "plugin.yaml").read_text(encoding="utf-8"))
    manifest["footnote_compat"] = {"minimum": "999.0.0"}
    (pack / "plugin.yaml").write_text(yaml.safe_dump(manifest), encoding="utf-8")

    res = runner.invoke(plugins_app, ["--root", str(root), "verify", str(pack), "--json"])
    assert res.exit_code == 1
    report = json.loads(res.output)
    compat = next(c for c in report["conditions"] if c["name"] == "footnote-compat-range")
    assert compat["result"] == "failed"

    res = runner.invoke(plugins_app, ["--root", str(root), "activate", str(pack), "--json"])
    assert res.exit_code == 1
    assert "verification_failed" in json.loads(res.output)["error"]
    assert list((root / "plugin").glob("**/*.json")) == []


def test_deactivation_leaves_an_anomalous_path_with_its_owner(tmp_path: Path) -> None:
    root = _isolated_root(tmp_path)
    pack = _materialize_external_pack(tmp_path / "outside-checkout" / PACK_ID)
    activated = _plugins(root, "activate", str(pack))

    # A foreign writer replaced a receipted definition with a symlink: the
    # anomaly is reported, the receipt is retained, exit is non-zero.
    victim = root / activated["written_paths"][0]
    victim.unlink()
    victim.symlink_to(tmp_path / "elsewhere.json")

    res = runner.invoke(plugins_app, ["--root", str(root), "deactivate", PACK_ID, "--json"])
    assert res.exit_code == 1
    outcome = json.loads(res.output)
    assert outcome["removed"] == []
    assert outcome["left_alone"] == activated["written_paths"]
    assert victim.is_symlink()
