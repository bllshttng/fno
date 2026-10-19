"""Opt-in regression against the real Codex 0.145 plugin parser."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tomllib
from pathlib import Path

import pytest

from fno.setup.codex_plugin import CodexPluginError, converge


REPO_ROOT = Path(__file__).resolve().parents[3]
pytestmark = pytest.mark.skipif(
    os.environ.get("FNO_REAL_CODEX_PLUGIN_TEST") != "1",
    reason="set FNO_REAL_CODEX_PLUGIN_TEST=1 to run real Codex 0.145 coverage",
)


def _run(home: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["codex", *args],
        capture_output=True,
        text=True,
        check=False,
        timeout=180,
        env={**os.environ, "CODEX_HOME": str(home)},
    )


@pytest.fixture
def staged_source(tmp_path: Path) -> Path:
    stage = tmp_path / "staged-source"
    for relative in ("skills", "agents", "commands", "hooks", "assets", ".claude-plugin", ".codex-plugin"):
        shutil.copytree(REPO_ROOT / relative, stage / relative)
    descriptor = stage / ".agents/plugins/marketplace.json"
    descriptor.parent.mkdir(parents=True)
    marketplace = json.loads((REPO_ROOT / ".agents/plugins/marketplace.json").read_text())
    marketplace["plugins"][0]["source"] = {"source": "local", "path": "."}
    descriptor.write_text(json.dumps(marketplace), encoding="utf-8")
    return stage


def test_codex_0145_failed_legacy_migration_restores_config_cache_and_marker(
    tmp_path: Path, staged_source: Path,
) -> None:
    home = tmp_path / "codex-home"
    home.mkdir()
    legacy_source = tmp_path / "legacy-source"
    legacy_marketplace = legacy_source / ".agents/marketplaces/footnote-dev"
    descriptor = legacy_marketplace / ".agents/plugins/marketplace.json"
    descriptor.parent.mkdir(parents=True)
    descriptor.write_text(
        json.dumps(
            {
                "name": "footnote-dev",
                "plugins": [
                    {
                        "name": "fno",
                        "source": {"source": "local", "path": "../../.."},
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    config = home / "config.toml"
    config.write_text(
        "# unrelated user comment must survive rollback\n"
        "\n".join(
            (
                "[marketplaces.footnote-dev]",
                'source_type = "local"',
                f'source = "{legacy_marketplace}"',
                "",
                '[plugins."fno@footnote-dev"]',
                "enabled = true",
                "",
            )
        ),
        encoding="utf-8",
    )
    expected_config_bytes = config.read_bytes()
    expected_config = tomllib.loads(config.read_text(encoding="utf-8"))
    legacy_cache = home / "plugins/cache/footnote-dev/fno/0.3.0"
    legacy_cache.mkdir(parents=True)
    cache_bytes = b"legacy working payload\n"
    (legacy_cache / "payload").write_bytes(cache_bytes)
    marker = home / "footnote/plugin-channel.json"
    marker.parent.mkdir(parents=True)
    marker_bytes = b'{"channel":"dev","marketplace":"footnote-dev","source":"legacy"}\n'
    marker.write_bytes(marker_bytes)
    failed_once = False

    def runner(
        argv: list[str], **kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        nonlocal failed_once
        result = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            errors="replace",
            check=False,
            **kwargs,
        )
        env = kwargs.get("env")
        is_live = env is None or Path(str(env["CODEX_HOME"])) == home
        if (
            is_live
            and argv == ["codex", "plugin", "add", "fno@footnote", "--json"]
            and result.returncode == 0
            and not failed_once
        ):
            failed_once = True
            return subprocess.CompletedProcess(
                argv,
                17,
                result.stdout,
                "injected post-mutation plugin add failure",
            )
        return result

    with pytest.raises(CodexPluginError) as caught:
        converge(
            channel="dev",
            runner=runner,
            codex_home=home,
            source_root=staged_source,
        )

    assert caught.value.stage == "plugin-add"
    assert config.read_bytes() == expected_config_bytes
    assert tomllib.loads(config.read_text(encoding="utf-8")) == expected_config
    assert (legacy_cache / "payload").read_bytes() == cache_bytes
    assert marker.read_bytes() == marker_bytes
    assert not (home / "plugins/cache/footnote").exists()
    marketplaces = _run(home, "plugin", "marketplace", "list", "--json")
    assert marketplaces.returncode == 0, marketplaces.stderr
    assert [
        row["name"] for row in json.loads(marketplaces.stdout)["marketplaces"]
    ] == ["footnote-dev"]
