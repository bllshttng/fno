from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest
from typer.testing import CliRunner

import fno.setup.codex_plugin as codex_plugin
from fno.setup.codex_plugin import (
    DEV_MARKETPLACE,
    DEV_PLUGIN_ID,
    LEGACY_DEV_MARKETPLACE,
    LEGACY_DEV_PLUGIN_ID,
    MARKETPLACE,
    PLUGIN_ID,
    RELEASE_PLUGIN_ID,
    CodexPluginError,
    ConvergenceResult,
    _validate_candidate,
    converge,
    inspect_freshness,
    parse_state,
    plugin_payload_digest,
)


def test_release_and_dev_share_one_identity() -> None:
    assert MARKETPLACE == DEV_MARKETPLACE == "footnote"
    assert PLUGIN_ID == DEV_PLUGIN_ID == RELEASE_PLUGIN_ID == "fno@footnote"
    assert LEGACY_DEV_MARKETPLACE == "footnote-dev"
    assert LEGACY_DEV_PLUGIN_ID == "fno@footnote-dev"


def test_candidate_validation_uses_an_explicit_isolated_codex_home(
    tmp_path: Path,
) -> None:
    source = _source(tmp_path)
    live_home = tmp_path / "live-codex-home"
    candidate_homes: set[Path] = set()

    def runner(argv: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        env = kwargs.get("env")
        assert isinstance(env, dict)
        home = Path(str(env["CODEX_HOME"]))
        assert home != live_home
        candidate_homes.add(home)
        if argv[1:4] == ["plugin", "marketplace", "add"]:
            return _cp(argv, {"marketplaceName": "footnote"})
        if argv[1:4] == ["plugin", "list", "--available"]:
            return _cp(
                argv,
                {
                    "installed": [],
                    "available": [
                        {
                            "pluginId": "fno@footnote",
                            "version": "0.3.0",
                        }
                    ],
                },
            )
        if argv == ["codex", "plugin", "add", "fno@footnote", "--json"]:
            cache = home / "plugins/cache/footnote/fno/0.3.0"
            shutil.copytree(source, cache)
            return _cp(argv, {"pluginId": "fno@footnote"})
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(
                argv,
                {
                    "marketplaces": [
                        {
                            "name": "footnote",
                            "root": str(source),
                            "marketplaceSource": {
                                "sourceType": "local",
                                "source": str(source),
                            },
                        }
                    ]
                },
            )
        if argv == ["codex", "plugin", "list", "--json"]:
            return _cp(
                argv,
                {
                    "installed": [
                        _plugin(
                            "fno@footnote",
                            source=str(source),
                            source_type="local",
                        )
                    ]
                },
            )
        return _cp(argv, {}, rc=1, err=f"unexpected {argv}")

    _validate_candidate(runner, source=str(source), ref=None)

    assert len(candidate_homes) == 1
    assert not live_home.exists()


def _cp(
    argv: list[str], payload: object, *, rc: int = 0, err: str = ""
) -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(argv, rc, json.dumps(payload), err)


def _plugin(plugin_id: str, *, source: str, source_type: str) -> dict[str, object]:
    return {
        "pluginId": plugin_id,
        "marketplaceName": plugin_id.split("@", 1)[1],
        "version": "0.3.0",
        "installed": True,
        "enabled": True,
        "marketplaceSource": {"sourceType": source_type, "source": source},
    }


def _source(tmp_path: Path) -> Path:
    root = tmp_path / "source"
    (root / ".codex-plugin").mkdir(parents=True)
    (root / ".codex-plugin" / "plugin.json").write_text(
        json.dumps({"name": "fno", "version": "0.3.0"}), encoding="utf-8"
    )
    script = root / "scripts" / "release" / "sync-version.sh"
    script.parent.mkdir(parents=True)
    script.write_text("#!/bin/sh\n", encoding="utf-8")
    return root


def _dev_marketplace(source: Path) -> Path:
    return source


class _StatefulCodex:
    def __init__(self, source: Path) -> None:
        self.source = source
        self.marketplaces: list[dict[str, object]] = []
        self.plugins: list[dict[str, object]] = []
        self.fail_git_plugin_once = False
        self.fail_local_marketplace = False
        self.fail_plugin_remove_after_mutation_once = False
        self.retain_removed_plugin_disabled_once = False
        self.fail_legacy_plugin_once = False

    def add_marketplace(self, *, name: str, source: str, source_type: str) -> None:
        self.marketplaces.append(
            {
                "name": name,
                "root": source,
                "marketplaceSource": {
                    "sourceType": source_type,
                    "source": source,
                },
            }
        )

    def __call__(
        self, argv: list[str], **_kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        if argv == [str(self.source / "scripts/release/sync-version.sh"), "--check"]:
            return _cp(argv, {})
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(argv, {"marketplaces": self.marketplaces})
        if argv == ["codex", "plugin", "list", "--json"]:
            return _cp(argv, {"installed": self.plugins, "available": []})
        if argv[1:4] == ["plugin", "marketplace", "remove"]:
            self.marketplaces[:] = [row for row in self.marketplaces if row["name"] != argv[4]]
            return _cp(argv, {})
        if argv[1:4] == ["plugin", "marketplace", "add"]:
            added_source = argv[4]
            source_type = "git" if added_source == "bllshttng/fno" else "local"
            if self.fail_local_marketplace and source_type == "local":
                return _cp(argv, {}, rc=17, err="injected rollback add failure")
            name = (
                LEGACY_DEV_MARKETPLACE
                if Path(added_source).name == LEGACY_DEV_MARKETPLACE
                else MARKETPLACE
            )
            self.add_marketplace(name=name, source=added_source, source_type=source_type)
            return _cp(argv, {})
        if argv[1:4] == ["plugin", "marketplace", "upgrade"]:
            return _cp(argv, {})
        if argv[1:3] == ["plugin", "remove"]:
            if self.retain_removed_plugin_disabled_once:
                self.retain_removed_plugin_disabled_once = False
                for row in self.plugins:
                    if row["pluginId"] == argv[3]:
                        row["enabled"] = False
                return _cp(argv, {})
            self.plugins[:] = [row for row in self.plugins if row["pluginId"] != argv[3]]
            if self.fail_plugin_remove_after_mutation_once:
                self.fail_plugin_remove_after_mutation_once = False
                return _cp(argv, {}, rc=17, err="injected post-mutation remove failure")
            return _cp(argv, {})
        if argv[1:3] == ["plugin", "add"]:
            plugin_id = argv[3]
            if self.fail_legacy_plugin_once and plugin_id == LEGACY_DEV_PLUGIN_ID:
                self.fail_legacy_plugin_once = False
                return _cp(argv, {}, rc=17, err="legacy identity cannot be re-added")
            marketplace_name = plugin_id.split("@", 1)[1]
            marketplace = next(
                row for row in self.marketplaces if row["name"] == marketplace_name
            )
            marketplace_source = marketplace["marketplaceSource"]
            assert isinstance(marketplace_source, dict)
            if (
                self.fail_git_plugin_once
                and marketplace_source["sourceType"] == "git"
            ):
                self.fail_git_plugin_once = False
                return _cp(argv, {}, rc=19, err="injected candidate replacement failure")
            self.plugins.append(
                _plugin(
                    plugin_id,
                    source=str(marketplace_source["source"]),
                    source_type=str(marketplace_source["sourceType"]),
                )
            )
            return _cp(argv, {})
        return _cp(argv, {}, rc=1, err=f"unexpected {argv}")


def test_parse_rejects_enabled_plugin_that_is_not_installed() -> None:
    row = _plugin("fno@footnote", source="bllshttng/fno", source_type="git")
    row["installed"] = False
    with pytest.raises(CodexPluginError, match="enabled plugin is not installed"):
        parse_state('{"marketplaces": []}', json.dumps({"installed": [row]}))


def test_release_convergence_removes_dev_then_installs_and_verifies(tmp_path: Path) -> None:
    source = _source(tmp_path)
    codex_home = tmp_path / "codex-home"
    marketplaces: list[dict[str, object]] = []
    plugins = [_plugin("fno@footnote-dev", source=str(source), source_type="local")]
    calls: list[tuple[list[str], dict[str, object]]] = []

    def runner(argv: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append((argv, kwargs))
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(argv, {"marketplaces": marketplaces})
        if argv == ["codex", "plugin", "list", "--json"]:
            return _cp(argv, {"installed": plugins, "available": []})
        if argv == ["codex", "plugin", "remove", "fno@footnote-dev", "--json"]:
            marker = codex_home / "footnote" / "plugin-channel.json"
            assert not marker.exists()
            plugins.clear()
            return _cp(argv, {"removed": True})
        if argv == [
            "codex",
            "plugin",
            "marketplace",
            "add",
            "bllshttng/fno",
            "--ref",
            "stable",
            "--json",
        ]:
            marketplaces.append(
                {
                    "name": "footnote",
                    "marketplaceSource": {
                        "sourceType": "git",
                        "source": "https://github.com/bllshttng/fno.git",
                    },
                }
            )
            return _cp(argv, {"name": "footnote"})
        if argv == ["codex", "plugin", "marketplace", "upgrade", "footnote", "--json"]:
            return _cp(argv, {"upgraded": True})
        if argv == ["codex", "plugin", "add", "fno@footnote", "--json"]:
            plugins.append(
                _plugin(
                    "fno@footnote",
                    source="https://github.com/bllshttng/fno.git",
                    source_type="git",
                )
            )
            return _cp(argv, {"pluginId": "fno@footnote"})
        if argv == [str(source / "scripts/release/sync-version.sh"), "--check"]:
            return _cp(argv, {"checked": True})
        return _cp(argv, {}, rc=1, err=f"unexpected {argv}")

    result = converge(
        channel="release",
        refresh=False,
        validate_candidate=False,
        runner=runner,
        codex_home=codex_home,
        source_root=source,
    )

    assert (result.action, result.plugin_id, result.version) == (
        "installed",
        RELEASE_PLUGIN_ID,
        "0.3.0",
    )
    assert [call[0] for call in calls] == [
        ["codex", "plugin", "marketplace", "list", "--json"],
        ["codex", "plugin", "list", "--json"],
        [str(source / "scripts/release/sync-version.sh"), "--check"],
        ["codex", "plugin", "remove", "fno@footnote-dev", "--json"],
        [
            "codex",
            "plugin",
            "marketplace",
            "add",
            "bllshttng/fno",
            "--ref",
            "stable",
            "--json",
        ],
        ["codex", "plugin", "marketplace", "upgrade", "footnote", "--json"],
        ["codex", "plugin", "add", "fno@footnote", "--json"],
        ["codex", "plugin", "marketplace", "list", "--json"],
        ["codex", "plugin", "list", "--json"],
    ]
    assert calls[2][1]["cwd"] == source


def test_release_convergence_is_source_aware_noop(tmp_path: Path) -> None:
    source = _source(tmp_path)
    home = tmp_path / "codex-home"
    marker = home / "footnote/plugin-channel.json"
    marker.parent.mkdir(parents=True)
    marker.write_text(
        json.dumps(
            {
                "channel": "release",
                "marketplace": MARKETPLACE,
                "source": "bllshttng/fno",
                "ref": "stable",
            }
        ),
        encoding="utf-8",
    )
    calls: list[list[str]] = []
    command_homes: list[Path] = []

    def runner(argv: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(argv)
        env = kwargs.get("env")
        assert isinstance(env, dict)
        command_homes.append(Path(str(env["CODEX_HOME"])))
        if "marketplace" in argv:
            return _cp(
                argv,
                {
                    "marketplaces": [
                        {
                            "name": "footnote",
                            "marketplaceSource": {
                                "sourceType": "git",
                                "source": "https://github.com/bllshttng/fno.git",
                            },
                        }
                    ]
                },
            )
        return _cp(
            argv,
            {
                "installed": [
                    _plugin(
                        "fno@footnote",
                        source="https://github.com/bllshttng/fno.git",
                        source_type="git",
                    )
                ],
                "available": [],
            },
        )

    result = converge(
        channel="release",
        validate_candidate=False,
        runner=runner,
        codex_home=home,
        source_root=source,
    )
    assert result.action == "no-op"
    assert calls == [
        ["codex", "plugin", "marketplace", "list", "--json"],
        ["codex", "plugin", "list", "--json"],
    ]
    assert command_homes == [home] * len(calls)


def test_marker_missing_ref_is_not_trusted_as_noop(tmp_path: Path) -> None:
    """A machine converged before --ref stable existed carries no "ref" key.

    Trusting that marker as a no-op would leave it pinned to `main` forever;
    the marker mismatch must force a real re-converge that adds the ref.
    """
    source = _source(tmp_path)
    home = tmp_path / "codex-home"
    marker = home / "footnote/plugin-channel.json"
    marker.parent.mkdir(parents=True)
    marker.write_text(
        json.dumps(
            {
                "channel": "release",
                "marketplace": MARKETPLACE,
                "source": "bllshttng/fno",
            }
        ),
        encoding="utf-8",
    )
    marketplaces: list[dict[str, object]] = [
        {
            "name": "footnote",
            "marketplaceSource": {
                "sourceType": "git",
                "source": "https://github.com/bllshttng/fno.git",
            },
        }
    ]
    plugins = [
        _plugin(
            RELEASE_PLUGIN_ID,
            source="https://github.com/bllshttng/fno.git",
            source_type="git",
        )
    ]
    calls: list[list[str]] = []

    def runner(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(argv)
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(argv, {"marketplaces": marketplaces})
        if argv == ["codex", "plugin", "list", "--json"]:
            return _cp(argv, {"installed": plugins, "available": []})
        if argv == [str(source / "scripts/release/sync-version.sh"), "--check"]:
            return _cp(argv, {"checked": True})
        if argv == ["codex", "plugin", "remove", RELEASE_PLUGIN_ID, "--json"]:
            plugins[:] = [p for p in plugins if p["pluginId"] != RELEASE_PLUGIN_ID]
            return _cp(argv, {"removed": True})
        if argv == ["codex", "plugin", "marketplace", "remove", "footnote", "--json"]:
            marketplaces[:] = [m for m in marketplaces if m["name"] != "footnote"]
            return _cp(argv, {"removed": True})
        if argv == [
            "codex",
            "plugin",
            "marketplace",
            "add",
            "bllshttng/fno",
            "--ref",
            "stable",
            "--json",
        ]:
            marketplaces.append(
                {
                    "name": "footnote",
                    "marketplaceSource": {
                        "sourceType": "git",
                        "source": "https://github.com/bllshttng/fno.git",
                    },
                }
            )
            return _cp(argv, {"name": "footnote"})
        if argv == ["codex", "plugin", "marketplace", "upgrade", "footnote", "--json"]:
            return _cp(argv, {"upgraded": True})
        if argv == ["codex", "plugin", "add", RELEASE_PLUGIN_ID, "--json"]:
            plugins.append(
                _plugin(
                    RELEASE_PLUGIN_ID,
                    source="https://github.com/bllshttng/fno.git",
                    source_type="git",
                )
            )
            return _cp(argv, {"pluginId": RELEASE_PLUGIN_ID})
        return _cp(argv, {}, rc=1, err=f"unexpected {argv}")

    result = converge(
        channel="release",
        validate_candidate=False,
        runner=runner,
        codex_home=home,
        source_root=source,
    )

    assert result.action != "no-op"
    assert [
        "codex",
        "plugin",
        "marketplace",
        "add",
        "bllshttng/fno",
        "--ref",
        "stable",
        "--json",
    ] in calls
    marker_payload = json.loads(marker.read_text(encoding="utf-8"))
    assert marker_payload["ref"] == "stable"


def test_empty_selected_version_is_repaired_instead_of_verified_noop(
    tmp_path: Path,
) -> None:
    source = _source(tmp_path)
    row = _plugin(
        RELEASE_PLUGIN_ID,
        source="https://github.com/bllshttng/fno.git",
        source_type="git",
    )
    row["version"] = ""
    plugins = [row]
    marketplaces: list[dict[str, object]] = [
        {
            "name": "footnote",
            "marketplaceSource": {
                "sourceType": "git",
                "source": "https://github.com/bllshttng/fno.git",
            },
        }
    ]

    def runner(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(argv, {"marketplaces": marketplaces})
        if argv == ["codex", "plugin", "list", "--json"]:
            return _cp(argv, {"installed": plugins, "available": []})
        if argv == ["codex", "plugin", "remove", RELEASE_PLUGIN_ID, "--json"]:
            plugins.clear()
            return _cp(argv, {"removed": True})
        if argv == [str(source / "scripts/release/sync-version.sh"), "--check"]:
            return _cp(argv, {})
        if argv == ["codex", "plugin", "marketplace", "remove", "footnote", "--json"]:
            marketplaces.clear()
            return _cp(argv, {"removed": True})
        if argv == [
            "codex",
            "plugin",
            "marketplace",
            "add",
            "bllshttng/fno",
            "--ref",
            "stable",
            "--json",
        ]:
            marketplaces.append(
                {
                    "name": "footnote",
                    "marketplaceSource": {
                        "sourceType": "git",
                        "source": "https://github.com/bllshttng/fno.git",
                    },
                }
            )
            return _cp(argv, {"name": "footnote"})
        if argv == [
            "codex",
            "plugin",
            "marketplace",
            "upgrade",
            "footnote",
            "--json",
        ]:
            return _cp(argv, {"upgraded": True})
        if argv == ["codex", "plugin", "add", RELEASE_PLUGIN_ID, "--json"]:
            plugins.append(
                _plugin(
                    RELEASE_PLUGIN_ID,
                    source="https://github.com/bllshttng/fno.git",
                    source_type="git",
                )
            )
            return _cp(argv, {"pluginId": RELEASE_PLUGIN_ID})
        return _cp(argv, {}, rc=1, err=f"unexpected {argv}")

    result = converge(
        channel="release",
        validate_candidate=False,
        runner=runner,
        codex_home=tmp_path / "codex-home",
        source_root=source,
    )

    assert result.action == "repaired"
    assert result.version == "0.3.0"


def test_packaged_release_install_does_not_require_local_plugin_source(
    tmp_path: Path,
) -> None:
    source = tmp_path / "ordinary-project"
    (source / ".codex-plugin").mkdir(parents=True)
    (source / ".codex-plugin" / "plugin.json").write_text(
        json.dumps({"name": "unrelated", "version": "9.9.9"}), encoding="utf-8"
    )
    marketplaces: list[dict[str, object]] = []
    plugins: list[dict[str, object]] = []
    calls: list[list[str]] = []

    def runner(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(argv)
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(argv, {"marketplaces": marketplaces})
        if argv == ["codex", "plugin", "list", "--json"]:
            return _cp(argv, {"installed": plugins, "available": []})
        if argv == [
            "codex",
            "plugin",
            "marketplace",
            "add",
            "bllshttng/fno",
            "--ref",
            "stable",
            "--json",
        ]:
            marketplaces.append(
                {
                    "name": "footnote",
                    "marketplaceSource": {
                        "sourceType": "git",
                        "source": "https://github.com/bllshttng/fno.git",
                    },
                }
            )
            return _cp(argv, {"name": "footnote"})
        if argv == [
            "codex",
            "plugin",
            "marketplace",
            "upgrade",
            "footnote",
            "--json",
        ]:
            return _cp(argv, {"upgraded": True})
        if argv == ["codex", "plugin", "add", RELEASE_PLUGIN_ID, "--json"]:
            plugins.append(
                _plugin(
                    RELEASE_PLUGIN_ID,
                    source="https://github.com/bllshttng/fno.git",
                    source_type="git",
                )
            )
            return _cp(argv, {"pluginId": RELEASE_PLUGIN_ID})
        return _cp(argv, {}, rc=1, err=f"unexpected {argv}")

    result = converge(
        channel="release",
        validate_candidate=False,
        runner=runner,
        codex_home=tmp_path / "codex-home",
        source_root=source,
    )

    assert result.version == "0.3.0"
    assert not any("sync-version.sh" in part for call in calls for part in call)


def test_convergence_preserves_unrelated_fno_named_plugins(tmp_path: Path) -> None:
    source = _source(tmp_path)
    unrelated = _plugin(
        "fno@company-tools", source="company/tools", source_type="git"
    )
    plugins = [unrelated]
    marketplaces: list[dict[str, object]] = []
    calls: list[list[str]] = []

    def runner(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(argv)
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(argv, {"marketplaces": marketplaces})
        if argv == ["codex", "plugin", "list", "--json"]:
            return _cp(argv, {"installed": plugins, "available": []})
        if argv == [str(source / "scripts/release/sync-version.sh"), "--check"]:
            return _cp(argv, {})
        if "marketplace" in argv:
            if "add" in argv:
                marketplaces.append(
                    {
                        "name": "footnote",
                        "marketplaceSource": {
                            "sourceType": "git",
                            "source": "bllshttng/fno",
                        },
                    }
                )
            return _cp(argv, {})
        if argv == ["codex", "plugin", "add", RELEASE_PLUGIN_ID, "--json"]:
            plugins.append(
                _plugin(
                    RELEASE_PLUGIN_ID,
                    source="bllshttng/fno",
                    source_type="git",
                )
            )
            return _cp(argv, {})
        return _cp(argv, {}, rc=1, err=f"unexpected {argv}")

    result = converge(
        channel="release",
        validate_candidate=False,
        runner=runner,
        codex_home=tmp_path / "codex-home",
        source_root=source,
    )

    assert result.plugin_id == RELEASE_PLUGIN_ID
    assert unrelated in plugins
    assert ["codex", "plugin", "remove", "fno@company-tools", "--json"] not in calls


def test_dev_convergence_switches_from_release_and_writes_requested_marker(
    tmp_path: Path,
) -> None:
    source = _source(tmp_path)
    dev_marketplace = _dev_marketplace(source)
    codex_home = tmp_path / "codex-home"
    marketplaces: list[dict[str, object]] = []
    plugins = [
        _plugin(
            RELEASE_PLUGIN_ID,
            source="https://github.com/bllshttng/fno.git",
            source_type="git",
        )
    ]
    calls: list[list[str]] = []

    def runner(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(argv)
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(argv, {"marketplaces": marketplaces})
        if argv == ["codex", "plugin", "list", "--json"]:
            return _cp(argv, {"installed": plugins, "available": []})
        if argv == ["codex", "plugin", "remove", RELEASE_PLUGIN_ID, "--json"]:
            assert not (codex_home / "footnote" / "plugin-channel.json").exists()
            plugins.clear()
            return _cp(argv, {"removed": True})
        if argv == [
            "codex",
            "plugin",
            "marketplace",
            "add",
            str(dev_marketplace),
            "--json",
        ]:
            marketplaces.append(
                {
                    "name": DEV_MARKETPLACE,
                    "marketplaceSource": {
                        "sourceType": "local",
                        "source": str(dev_marketplace),
                    },
                }
            )
            return _cp(argv, {"name": DEV_MARKETPLACE})
        if argv == ["codex", "plugin", "add", DEV_PLUGIN_ID, "--json"]:
            plugins.append(
                _plugin(
                    DEV_PLUGIN_ID,
                    source=str(dev_marketplace),
                    source_type="local",
                )
            )
            return _cp(argv, {"pluginId": DEV_PLUGIN_ID})
        return _cp(argv, {}, rc=1, err=f"unexpected {argv}")

    result = converge(
        channel="dev",
        validate_candidate=False,
        runner=runner,
        codex_home=codex_home,
        source_root=source,
    )

    assert (result.channel, result.action, result.plugin_id, result.version) == (
        "dev",
        "repaired",
        DEV_PLUGIN_ID,
        "0.3.0",
    )
    assert calls == [
        ["codex", "plugin", "marketplace", "list", "--json"],
        ["codex", "plugin", "list", "--json"],
        ["codex", "plugin", "remove", RELEASE_PLUGIN_ID, "--json"],
        ["codex", "plugin", "marketplace", "add", str(dev_marketplace), "--json"],
        ["codex", "plugin", "add", DEV_PLUGIN_ID, "--json"],
        ["codex", "plugin", "marketplace", "list", "--json"],
        ["codex", "plugin", "list", "--json"],
    ]


def test_freshness_detects_same_version_payload_drift(tmp_path: Path) -> None:
    source = _source(tmp_path)
    dev_marketplace = _dev_marketplace(source)
    home = tmp_path / "codex-home"
    (home / "footnote").mkdir(parents=True)
    (home / "footnote" / "plugin-channel.json").write_text(
        json.dumps(
            {
                "channel": "dev",
                "marketplace": DEV_MARKETPLACE,
                "source": str(dev_marketplace),
            }
        ),
        encoding="utf-8",
    )
    source_payload = source / "skills" / "target" / "SKILL.md"
    source_payload.parent.mkdir(parents=True)
    source_payload.write_text("new source\n", encoding="utf-8")
    cache = home / "plugins" / "cache" / DEV_MARKETPLACE / "fno" / "0.3.0"
    (cache / ".codex-plugin").mkdir(parents=True)
    shutil.copy2(
        source / ".codex-plugin" / "plugin.json",
        cache / ".codex-plugin" / "plugin.json",
    )
    cached_payload = cache / "skills" / "target" / "SKILL.md"
    cached_payload.parent.mkdir(parents=True)
    cached_payload.write_text("old cache\n", encoding="utf-8")

    def runner(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(
                argv,
                {
                    "marketplaces": [
                        {
                            "name": DEV_MARKETPLACE,
                            "marketplaceSource": {
                                "sourceType": "local",
                                "source": str(dev_marketplace),
                            },
                        }
                    ]
                },
            )
        return _cp(
            argv,
            {
                "installed": [
                    _plugin(
                        DEV_PLUGIN_ID,
                        source=str(dev_marketplace),
                        source_type="local",
                    )
                ],
                "available": [],
            },
        )

    report = inspect_freshness(runner=runner, codex_home=home, source_root=source)
    assert report["status"] == "stale"
    assert report["issue"] == "payload-drift"
    assert report["source_version"] == report["cache_version"] == "0.3.0"
    assert report["source_digest"] != report["cache_digest"]
    assert report["remedy"] == "fno config plugin install codex --force"


def test_freshness_refuses_legacy_duplicate_state_before_digest(tmp_path: Path) -> None:
    source = _source(tmp_path)
    dev_marketplace = _dev_marketplace(source)
    home = tmp_path / "codex-home"
    (home / "footnote").mkdir(parents=True)
    (home / "footnote" / "plugin-channel.json").write_text(
        json.dumps(
            {
                "channel": "dev",
                "marketplace": DEV_MARKETPLACE,
                "source": str(dev_marketplace),
            }
        ),
        encoding="utf-8",
    )

    def runner(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        if "marketplace" in argv:
            return _cp(argv, {"marketplaces": []})
        return _cp(
            argv,
            {
                "installed": [
                    _plugin(
                        DEV_PLUGIN_ID,
                        source=str(dev_marketplace),
                        source_type="local",
                    ),
                    _plugin(
                        RELEASE_PLUGIN_ID,
                        source="bllshttng/fno",
                        source_type="git",
                    ),
                ],
                "available": [],
            },
        )

    report = inspect_freshness(runner=runner, codex_home=home, source_root=source)
    assert report["status"] == "conflict"
    assert report["issue"] == "ambiguous-duplicate-state"
    assert "source_digest" not in report


def test_freshness_prioritizes_persisted_rollback_failure(tmp_path: Path) -> None:
    home = tmp_path / "codex-home"
    receipt = home / "footnote" / "rollback-failure.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text(
        json.dumps({"stage": "plugin-add", "detail": "rollback add failed"}),
        encoding="utf-8",
    )

    report = inspect_freshness(
        runner=lambda *_args, **_kwargs: pytest.fail("state collection must not run"),
        codex_home=home,
    )

    assert report["status"] == "error"
    assert report["issue"] == "rollback-failure"
    assert report["detail"] == "rollback add failed"


def test_payload_digest_is_stable_and_ignores_non_plugin_files(tmp_path: Path) -> None:
    source = _source(tmp_path)
    payload = source / "hooks" / "session-start.sh"
    payload.parent.mkdir(parents=True)
    payload.write_text("plugin payload\n", encoding="utf-8")
    first = plugin_payload_digest(source)
    (source / "tests").mkdir()
    (source / "tests" / "noise.txt").write_text("ignored\n", encoding="utf-8")
    pycache = source / "skills" / "target" / "__pycache__"
    pycache.mkdir(parents=True)
    (pycache / "generated.pyc").write_bytes(b"transient")
    (source / "hooks").mkdir(exist_ok=True)
    (source / "hooks" / ".DS_Store").write_bytes(b"transient")
    assert plugin_payload_digest(source) == first
    payload.write_text("changed payload\n", encoding="utf-8")
    assert plugin_payload_digest(source) != first


def test_owned_config_rollback_restores_exact_bytes_and_comments(tmp_path: Path) -> None:
    home = tmp_path / "codex-home"
    config = home / "config.toml"
    config.parent.mkdir(parents=True)
    expected = (
        b"# user heading\n"
        b"[unrelated]\n"
        b"value = 1 # keep this comment\n\n"
        b"[marketplaces.footnote-dev]\n"
        b"source_type = \"local\"\n"
        b"source = \"/legacy\"\n\n"
        b"[plugins.\"fno@footnote-dev\"]\n"
        b"enabled = true\n"
    )
    config.write_bytes(expected)
    snapshot = codex_plugin._read_owned_config(home)
    config.write_text(
        "[unrelated]\nvalue = 1\n\n"
        "[marketplaces.footnote]\nsource_type = \"local\"\nsource = \"/new\"\n\n"
        "[plugins.\"fno@footnote\"]\nenabled = true\n",
        encoding="utf-8",
    )

    codex_plugin._restore_owned_config(home, snapshot)

    assert config.read_bytes() == expected


def test_owned_config_rollback_refuses_nonfootnote_change(tmp_path: Path) -> None:
    home = tmp_path / "codex-home"
    config = home / "config.toml"
    config.parent.mkdir(parents=True)
    config.write_text(
        "[unrelated]\nvalue = 1\n\n"
        "[marketplaces.footnote-dev]\nsource_type = \"local\"\nsource = \"/legacy\"\n",
        encoding="utf-8",
    )
    snapshot = codex_plugin._read_owned_config(home)
    current = (
        "[unrelated]\nvalue = 2\n\n"
        "[marketplaces.footnote]\nsource_type = \"local\"\nsource = \"/new\"\n"
    )
    config.write_text(current, encoding="utf-8")

    with pytest.raises(CodexPluginError) as caught:
        codex_plugin._restore_owned_config(home, snapshot)

    assert caught.value.stage == "rollback-config"
    assert config.read_text(encoding="utf-8") == current


def test_failed_switch_restores_working_channel_and_marker_bytes(tmp_path: Path) -> None:
    source = _source(tmp_path)
    home = tmp_path / "codex-home"
    fake = _StatefulCodex(source)
    fake.add_marketplace(name=MARKETPLACE, source=str(source), source_type="local")
    fake.plugins.append(_plugin(PLUGIN_ID, source=str(source), source_type="local"))
    marker = home / "footnote" / "plugin-channel.json"
    marker.parent.mkdir(parents=True)
    marker_bytes = b'{"channel":"dev","marketplace":"footnote","source":"original"}\n'
    marker.write_bytes(marker_bytes)
    fake.fail_git_plugin_once = True

    with pytest.raises(CodexPluginError) as caught:
        converge(
            channel="release",
            validate_candidate=False,
            runner=fake,
            codex_home=home,
            source_root=source,
        )

    assert caught.value.stage == "plugin-add"
    assert marker.read_bytes() == marker_bytes
    assert [row["name"] for row in fake.marketplaces] == [MARKETPLACE]
    marketplace_source = fake.marketplaces[0]["marketplaceSource"]
    assert isinstance(marketplace_source, dict)
    assert marketplace_source == {"sourceType": "local", "source": str(source)}
    assert [row["pluginId"] for row in fake.plugins] == [PLUGIN_ID]


def test_rollback_receipt_clear_failure_prevents_switch_before_mutation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = _source(tmp_path)
    home = tmp_path / "codex-home"
    fake = _StatefulCodex(source)
    fake.add_marketplace(name=MARKETPLACE, source=str(source), source_type="local")
    fake.plugins.append(_plugin(PLUGIN_ID, source=str(source), source_type="local"))
    receipt = home / "footnote/rollback-failure.json"
    receipt.parent.mkdir(parents=True)
    receipt_bytes = b'{"detail":"prior rollback failed","stage":"plugin-add"}\n'
    receipt.write_bytes(receipt_bytes)

    def fail_clear(_home: Path) -> None:
        raise CodexPluginError("rollback-receipt-clear", "injected clear failure")

    monkeypatch.setattr(codex_plugin, "_clear_rollback_receipt", fail_clear)
    with pytest.raises(CodexPluginError) as caught:
        converge(
            channel="release",
            validate_candidate=False,
            runner=fake,
            codex_home=home,
            source_root=source,
        )

    assert caught.value.stage == "rollback-receipt-clear"
    marketplace_source = fake.marketplaces[0]["marketplaceSource"]
    assert isinstance(marketplace_source, dict)
    assert marketplace_source["sourceType"] == "local"
    assert receipt.read_bytes() == receipt_bytes


def test_legacy_cache_symlink_is_refused_without_mutation(tmp_path: Path) -> None:
    source = _source(tmp_path)
    home = tmp_path / "codex-home"
    fake = _StatefulCodex(source)
    fake.add_marketplace(name=MARKETPLACE, source=str(source), source_type="local")
    fake.plugins.append(_plugin(PLUGIN_ID, source=str(source), source_type="local"))
    marker = home / "footnote/plugin-channel.json"
    marker.parent.mkdir(parents=True)
    marker.write_text(
        json.dumps(
            {"channel": "dev", "marketplace": MARKETPLACE, "source": str(source)}
        ),
        encoding="utf-8",
    )
    target = tmp_path / "outside-cache"
    target.mkdir()
    legacy_cache = home / "plugins/cache/footnote-dev"
    legacy_cache.parent.mkdir(parents=True)
    legacy_cache.symlink_to(target, target_is_directory=True)

    with pytest.raises(CodexPluginError) as caught:
        converge(
            channel="dev",
            validate_candidate=False,
            runner=fake,
            codex_home=home,
            source_root=source,
        )

    assert caught.value.stage == "cache-quarantine"
    assert legacy_cache.is_symlink()
    assert [row["pluginId"] for row in fake.plugins] == [PLUGIN_ID]


def test_rollback_failure_is_persisted_and_named(tmp_path: Path) -> None:
    source = _source(tmp_path)
    home = tmp_path / "codex-home"
    fake = _StatefulCodex(source)
    fake.add_marketplace(name=MARKETPLACE, source=str(source), source_type="local")
    fake.plugins.append(_plugin(PLUGIN_ID, source=str(source), source_type="local"))
    fake.fail_git_plugin_once = True
    fake.fail_local_marketplace = True

    with pytest.raises(CodexPluginError) as caught:
        converge(
            channel="release",
            validate_candidate=False,
            runner=fake,
            codex_home=home,
            source_root=source,
        )

    assert caught.value.stage == "rollback-failure"
    receipt = json.loads(
        (home / "footnote" / "rollback-failure.json").read_text(encoding="utf-8")
    )
    assert receipt["channel"] == "release"
    assert receipt["stage"] == "plugin-add"
    assert "rollback failed" in receipt["detail"]


def test_dev_migrates_legacy_identity_and_removes_legacy_cache(tmp_path: Path) -> None:
    source = _source(tmp_path)
    home = tmp_path / "codex-home"
    legacy = tmp_path / LEGACY_DEV_MARKETPLACE
    legacy.mkdir()
    fake = _StatefulCodex(source)
    fake.add_marketplace(
        name=LEGACY_DEV_MARKETPLACE,
        source=str(legacy),
        source_type="local",
    )
    fake.plugins.append(
        _plugin(LEGACY_DEV_PLUGIN_ID, source=str(legacy), source_type="local")
    )
    legacy_cache = home / "plugins" / "cache" / LEGACY_DEV_MARKETPLACE
    legacy_cache.mkdir(parents=True)
    (legacy_cache / "stale").write_text("old", encoding="utf-8")

    result = converge(
        channel="dev",
        validate_candidate=False,
        runner=fake,
        codex_home=home,
        source_root=source,
    )

    assert result.plugin_id == PLUGIN_ID
    assert [row["name"] for row in fake.marketplaces] == [MARKETPLACE]
    assert [row["pluginId"] for row in fake.plugins] == [PLUGIN_ID]
    assert not legacy_cache.exists()
    marker = json.loads(
        (home / "footnote" / "plugin-channel.json").read_text(encoding="utf-8")
    )
    assert marker == {
        "channel": "dev",
        "marketplace": MARKETPLACE,
        "source": str(source),
        "ref": None,
    }


class _FakeStageRun:
    """Stands in for the fno-agents plugin-install child: ``--stage-only``
    prints a stage path, anything else is a no-op success."""

    def __init__(self, argv):
        self.argv = argv
        self.returncode = 0
        self.stdout = (
            "/tmp/plugin-stage-fno\n" if argv[1:2] == ["--stage-only"] else "fake\n"
        )
        self.stderr = ""


def test_public_cli_reports_verified_release_and_restart_posture(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # The `setup codex-plugin` door retired; the public door is
    # `fno config plugin install codex`.
    from fno.plugin_install_cli import plugin_app

    monkeypatch.setattr(
        "fno.plugin_install_cli.subprocess.run",
        lambda argv, **_kw: _FakeStageRun(argv),
    )
    monkeypatch.setattr(
        "fno.setup.codex_plugin.converge",
        lambda **_kwargs: ConvergenceResult("dev", "refreshed", "fno@footnote", "0.3.2"),
    )
    result = CliRunner().invoke(plugin_app, ["codex", "--force"])
    assert result.exit_code == 0, result.output
    assert "converged fno@footnote 0.3.2 (action=refreshed)" in result.output


def test_public_cli_exits_nonzero_without_success_on_stage_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from fno.plugin_install_cli import plugin_app

    def fail(**_kwargs: object) -> ConvergenceResult:
        raise CodexPluginError("plugin-add", "network unavailable")

    monkeypatch.setattr(
        "fno.plugin_install_cli.subprocess.run",
        lambda argv, **_kw: _FakeStageRun(argv),
    )
    monkeypatch.setattr("fno.setup.codex_plugin.converge", fail)
    result = CliRunner().invoke(plugin_app, ["codex"])
    assert result.exit_code == 1
    assert "plugin-add: network unavailable" in result.output
    assert "converged" not in result.output


def test_stable_release_noop_skips_offline_preflight_and_only_collects_live_state(
    tmp_path: Path,
) -> None:
    source = _source(tmp_path)
    home = tmp_path / "codex-home"
    marker = home / "footnote" / "plugin-channel.json"
    marker.parent.mkdir(parents=True)
    marker.write_text(
        json.dumps(
            {
                "channel": "release",
                "marketplace": MARKETPLACE,
                "source": "bllshttng/fno",
                "ref": "stable",
            }
        ),
        encoding="utf-8",
    )
    calls: list[list[str]] = []

    def runner(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(argv)
        if argv == [str(source / "scripts/release/sync-version.sh"), "--check"]:
            return _cp(argv, {}, rc=1, err="simulated offline preflight failure")
        if argv == ["codex", "plugin", "marketplace", "list", "--json"]:
            return _cp(
                argv,
                {
                    "marketplaces": [
                        {
                            "name": "footnote",
                            "marketplaceSource": {
                                "sourceType": "git",
                                "source": "https://github.com/bllshttng/fno.git",
                            },
                        }
                    ]
                },
            )
        if argv == ["codex", "plugin", "list", "--json"]:
            return _cp(
                argv,
                {
                    "installed": [
                        _plugin(
                            RELEASE_PLUGIN_ID,
                            source="https://github.com/bllshttng/fno.git",
                            source_type="git",
                        )
                    ],
                    "available": [],
                },
            )
        return _cp(argv, {}, rc=1, err=f"unexpected {argv}")

    result = converge(
        channel="release",
        runner=runner,
        codex_home=home,
        source_root=source,
    )

    assert result.action == "no-op"
    assert result.plugin_id == RELEASE_PLUGIN_ID
    assert result.version == "0.3.0"
    assert calls == [
        ["codex", "plugin", "marketplace", "list", "--json"],
        ["codex", "plugin", "list", "--json"],
    ]


@pytest.mark.parametrize("exc_type", [KeyboardInterrupt, SystemExit])
def test_interrupt_after_mutation_restores_exact_state_before_reraise(
    tmp_path: Path, exc_type: type[BaseException]
) -> None:
    source = _source(tmp_path)
    home = tmp_path / "codex-home"
    fake = _StatefulCodex(source)
    fake.add_marketplace(name=MARKETPLACE, source=str(source), source_type="local")
    fake.plugins.append(_plugin(PLUGIN_ID, source=str(source), source_type="local"))
    config = home / "config.toml"
    config.parent.mkdir(parents=True)
    config_bytes = (
        f'[marketplaces.footnote]\nsource_type = "local"\nsource = "{source}"\n\n'
        '[plugins."fno@footnote"]\nenabled = true\n'
    ).encode()
    config.write_bytes(config_bytes)
    marker = home / "footnote" / "plugin-channel.json"
    marker.parent.mkdir(parents=True)
    marker_bytes = b'{"channel":"dev","marketplace":"footnote","source":"prior"}\n'
    marker.write_bytes(marker_bytes)
    cache = home / "plugins" / "cache" / "footnote" / "fno" / "0.3.0"
    cache.mkdir(parents=True)
    cache_bytes = b"working cache\n"
    (cache / "payload").write_bytes(cache_bytes)

    def runner(argv: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        result = fake(argv, **kwargs)
        if argv[1:3] == ["plugin", "remove"] and argv[3] == PLUGIN_ID:
            raise exc_type("simulated termination after mutation")
        return result

    with pytest.raises(exc_type):
        converge(
            channel="release",
            validate_candidate=False,
            runner=runner,
            codex_home=home,
            source_root=source,
        )

    assert marker.read_bytes() == marker_bytes
    assert config.read_bytes() == config_bytes
    assert (cache / "payload").read_bytes() == cache_bytes
    assert [row["name"] for row in fake.marketplaces] == [MARKETPLACE]
    marketplace_source = fake.marketplaces[0]["marketplaceSource"]
    assert isinstance(marketplace_source, dict)
    assert marketplace_source["sourceType"] == "local"
    assert [row["pluginId"] for row in fake.plugins] == [PLUGIN_ID]


def test_door_forwards_extra_args_to_fno_agents(monkeypatch) -> None:
    """The plugin-install door forwards unknown flags to the Rust command
    byte-exact: the opencode arm's --yes/--dry-run live in Rust, and the
    Typer door must accept them without declaring them."""
    from fno.plugin_install_cli import plugin_app

    class _Proc:
        returncode = 0

    captured: dict = {}

    def fake_run(argv, check=False):
        captured["argv"] = argv
        return _Proc()

    monkeypatch.setattr(subprocess, "run", fake_run)
    monkeypatch.setattr(
        "fno.plugin_install_cli._binary", lambda: Path("/bin/true")
    )
    result = CliRunner().invoke(
        plugin_app, ["install", "opencode", "--dry-run", "--yes"]
    )
    assert result.exit_code == 0, result.output
    argv = captured["argv"]
    assert argv[1] == "plugin-install"
    assert argv[2:] == ["opencode", "--dry-run", "--yes"]
