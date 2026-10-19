"""Canonical node-ID module.

Covers the single source of truth in ``fno.graph._constants``: accessors,
``mint_node_id`` (configured scheme + collision retry + exhaustion), the
config-free ``is_wellformed_node_id`` matcher, and ``extract_node_ids``.

Filter: `uv run pytest cli/tests -k node_id -q`
"""
from __future__ import annotations

import pytest

from fno.config import SettingsModel
from fno.graph import _constants as c


# --- is_wellformed_node_id (config-free, liberal) -------------------------


@pytest.mark.parametrize(
    "good",
    [
        "ab-55ba9adb",
        "xb299",           # dash-less: the pre-2026-09-27 minter shape
        "a3f9c1d2",        # bare hex now reads as prefix 'a' + 7 hex
    ],
)
def test_wellformed_accepts(good):
    assert c.is_wellformed_node_id(good)


@pytest.mark.parametrize(
    "bad",
    [
        "ab-123",          # 3 hex (< 4)
        "ab-123456789",    # 9 hex (> 8)
        "AB-12345678",     # uppercase prefix
    ],
)
def test_wellformed_rejects(bad):
    assert not c.is_wellformed_node_id(bad)


# --- extract_node_ids ------------------------------------------------------


def test_extract_finds_configured_and_legacy():
    text = "promoted -> xy-a3f9 and the old -> ab-55ba9adb landed"
    assert c.extract_node_ids(text) == ["xy-a3f9", "ab-55ba9adb"]


# --- has_node_id_prefix (resolution-gate pre-check) -----------------------


def test_has_prefix_accepts_any_wellformed_past_prefix(monkeypatch):
    # codex P2: a node minted under a PAST configured prefix must still pass the
    # gate even when the CURRENT config uses a different prefix - the graph
    # lookup decides existence, not the gate.
    _patch_settings(monkeypatch, id_prefix="xy-", id_hex_width=4)
    assert c.has_node_id_prefix("fno-a3f9")   # different, but well-formed
    assert c.has_node_id_prefix("xy-a3f9")    # current configured
    assert c.has_node_id_prefix("ab-55ba9adb")  # legacy


def test_has_prefix_rejects_obvious_non_id(monkeypatch):
    _patch_settings(monkeypatch, id_prefix="xy-", id_hex_width=4)
    assert not c.has_node_id_prefix("just some words")
    assert not c.has_node_id_prefix("")
    assert not c.has_node_id_prefix(None)


# --- accessors: fallback on a failed settings load ------------------------------


def _patch_settings(monkeypatch, *, state_dir=None, **backlog):
    """Patch ``load_settings`` directly to a fixed model.

    ``state_dir`` is a distinct kwarg, not folded into ``backlog``: it must
    land in ``config.state_dir``, a sibling of ``config.backlog``, not inside
    it. Passing it lets a caller combine backlog config (id_prefix etc.) with
    filesystem isolation in ONE patched settings object - composing this with
    ``paths_testing.use_tmpdir`` instead does not work, because whichever of
    the two monkeypatches ``fno.config.load_settings`` LAST wins outright and
    silently drops the other's config.
    """
    config: dict = {"backlog": backlog}
    if state_dir is not None:
        config["state_dir"] = state_dir
    model = SettingsModel(config=config)
    monkeypatch.setattr("fno.config.load_settings", lambda: model)
    # paths._settings() is an uncached delegation to load_settings(), so the
    # patch above reaches every paths reader directly - no cache swap needed.


def test_accessors_fall_back_when_load_raises(monkeypatch):
    # AC2-ERR: a malformed settings load must not crash the accessor.
    def boom():
        raise RuntimeError("malformed settings.yaml")

    monkeypatch.setattr("fno.config.load_settings", boom)
    assert c.node_id_prefix() == "ab-"
    assert c.node_id_hex_width() == 8


# --- mint_node_id ----------------------------------------------------------


def test_mint_uses_configured_scheme(monkeypatch):
    _patch_settings(monkeypatch, id_prefix="xy-", id_hex_width=4)
    mid = c.mint_node_id(set())
    assert c.is_wellformed_node_id(mid)
    assert mid.startswith("xy-")
    suffix = c.node_id_suffix(mid)
    assert len(suffix) == 4 and all(ch in "0123456789abcdef" for ch in suffix)


def test_mint_normalizes_prefix_without_trailing_dash(monkeypatch):
    # A config prefix with no separator must not mint bare "<prefix><hex>"
    # literals: they fail every canonical shape gate downstream (the target
    # init guard, extract_node_ids, is_wellformed_node_id). The mint owns the
    # separator so any configured prefix yields a wellformed id.
    _patch_settings(monkeypatch, id_prefix="x", id_hex_width=4)
    mid = c.mint_node_id(set())
    assert c.is_wellformed_node_id(mid)
    assert mid.startswith("x-")
    assert len(c.node_id_suffix(mid)) == 4


def test_mint_raises_on_exhaustion(monkeypatch):
    # AC2-EDGE: every candidate "collides" -> bounded retries then a clear error.
    _patch_settings(monkeypatch, id_prefix="xy-", id_hex_width=4)
    monkeypatch.setattr(c, "_MINT_MAX_ATTEMPTS", 5)

    class AllContains:
        def __contains__(self, _x):
            return True

        def __len__(self):
            return 65536

    with pytest.raises(RuntimeError, match="exhaustion"):
        c.mint_node_id(AllContains())


# --- mint_node_id vs the archive -----------------------------------


def test_mint_avoids_archived_id(tmp_path, monkeypatch):
    """A freed working-graph id must not collide with a live archive entry."""
    import json

    _patch_settings(monkeypatch, id_prefix="xy-", id_hex_width=4, state_dir=str(tmp_path))

    from fno.graph._constants import _graph_archive_json as archive_path_fn

    archive_path = archive_path_fn()
    archive_path.parent.mkdir(parents=True, exist_ok=True)
    archive_path.write_text(
        json.dumps({"entries": [{"id": "xy-dead", "completed_at": "2026-01-01T00:00:00Z"}]}),
        encoding="utf-8",
    )

    # Minting against an empty working pool must still avoid the id the
    # archive holds: force the first candidate to collide, second to be free.
    import uuid as uuid_mod

    calls = iter(["dead" + "0" * 28, "cafe" + "0" * 28])

    class _FakeUUID:
        def __init__(self, hexval):
            self.hex = hexval

    monkeypatch.setattr(uuid_mod, "uuid4", lambda: _FakeUUID(next(calls)))
    mid = c.mint_node_id(set())
    assert mid != "xy-dead"
    assert mid == "xy-cafe"


def test_mint_archive_read_failure_degrades_to_working_pool(tmp_path, monkeypatch):
    """A corrupt archive must not block minting (advisory read)."""
    _patch_settings(monkeypatch, id_prefix="xy-", id_hex_width=4, state_dir=str(tmp_path))

    from fno.graph._constants import _graph_archive_json as archive_path_fn

    archive_path = archive_path_fn()
    archive_path.parent.mkdir(parents=True, exist_ok=True)
    archive_path.write_text("{not json at all", encoding="utf-8")

    mid = c.mint_node_id(set())
    assert c.is_wellformed_node_id(mid)
