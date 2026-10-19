from __future__ import annotations

from pathlib import Path

from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()

REPO_ROOT = Path(__file__).resolve().parents[4]
PACK = REPO_ROOT / "plugins" / "growth-studio" / "plugin.yaml"


def _invoke(*args: str):
    return runner.invoke(app, list(args))


def _root_env(monkeypatch, tmp_path) -> Path:
    root = tmp_path / "roles"
    root.mkdir()
    monkeypatch.setenv("FNO_ROLES_ROOT", str(root))
    return root


def test_activate_ls_inspect_deactivate_round_trip(monkeypatch, tmp_path):
    _root_env(monkeypatch, tmp_path)

    activated = _invoke("config", "plugins", "activate", str(PACK))
    assert activated.exit_code == 0, activated.output
    assert "activated growth-studio" in activated.output

    listed = _invoke("config", "plugins", "ls")
    assert listed.exit_code == 0, listed.output
    assert "growth-studio" in listed.output
    assert "active" in listed.output
    assert "external.publication" in listed.output

    inspected = _invoke("config", "plugins", "inspect", "growth-studio")
    assert inspected.exit_code == 0, inspected.output
    assert "declares roles:" in inspected.output

    deactivated = _invoke("config", "plugins", "deactivate", "growth-studio")
    assert deactivated.exit_code == 0, deactivated.output
    assert "deactivated growth-studio" in deactivated.output

    listed_after = _invoke("config", "plugins", "ls")
    assert "growth-studio" in listed_after.output
    assert "installed" in listed_after.output


def test_activate_refuses_a_corrupt_pack(monkeypatch, tmp_path):
    _root_env(monkeypatch, tmp_path)
    bad = tmp_path / "broken"
    bad.mkdir()
    (bad / "plugin.yaml").write_text("id: broken\n  bad: indent\n - x\n", encoding="utf-8")
    result = _invoke("config", "plugins", "activate", str(bad))
    assert result.exit_code == 1, result.output
    assert "refused" in result.output
