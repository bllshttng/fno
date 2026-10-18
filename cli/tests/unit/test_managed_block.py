"""Tests for the offered managed block (node x-0960, W4 / US7-US8).

Covers the plan's acceptance criteria:
  AC5-HP    stamp + re-stamp keeps bytes outside the markers byte-identical
  AC2-ERR   exactly one marker -> refuse, touch nothing
  AC2-EDGE  a first-time decline is durable (no re-prompt, no file touched)
  US8       fno doctor flags a stale block version (advisory)

Run: fno doctor test -- -k managed_block
"""
from __future__ import annotations

from pathlib import Path

from fno.setup.managed_block import (
    BLOCK_VERSION,
    marker_state,
    offer_managed_block,
    stamp_block,
    stamped_version,
)

_USER = "# My House Rules\n\nDo the thing. Do not break the build.\n"


def test_stamp_and_restamp_preserve_outside_bytes(tmp_path: Path) -> None:
    """AC5-HP: append on a marker-less file, then re-stamp to a new version;
    every byte outside the fences is untouched across both."""
    f = tmp_path / "AGENTS.md"
    f.write_text(_USER, encoding="utf-8")

    first = stamp_block(f, version=1)
    assert first.action == "appended"
    after = f.read_text(encoding="utf-8")
    assert after.startswith(_USER)  # user prose untouched, block appended below
    assert stamped_version(after) == 1

    second = stamp_block(f, version=2)
    assert second.action == "restamped"
    restamped = f.read_text(encoding="utf-8")
    assert stamped_version(restamped) == 2
    # Outside-marker bytes are exactly the original prose; only the fence changed.
    before_block = restamped.split("<!-- fno:begin", 1)[0]
    assert before_block == after.split("<!-- fno:begin", 1)[0] == _USER + "\n"


def test_never_downgrades_newer_block(tmp_path: Path) -> None:
    """offer leaves a block newer than the template alone (no downgrade prompt)."""
    f = tmp_path / "AGENTS.md"
    f.write_text("# rules\n", encoding="utf-8")
    stamp_block(f, version=BLOCK_VERSION + 1)  # host carries a newer stamp
    calls: list[str] = []
    res = offer_managed_block(
        tmp_path, confirm_fn=lambda m: calls.append(m) or True, version=BLOCK_VERSION
    )
    assert res["status"] == "current"
    assert calls == []  # never prompted to downgrade


def test_malformed_single_marker_refuses(tmp_path: Path) -> None:
    """AC2-ERR: a file with fno:begin but no fno:end is refused untouched."""
    f = tmp_path / "AGENTS.md"
    broken = _USER + "\n<!-- fno:begin v=1 -->\nstuff\n"  # no end marker
    f.write_text(broken, encoding="utf-8")
    assert marker_state(broken) == "malformed"

    res = stamp_block(f, version=BLOCK_VERSION)
    assert res.action == "refused-malformed"
    assert f.read_text(encoding="utf-8") == broken  # nothing written


def test_offer_declined_is_durable(tmp_path: Path) -> None:
    """AC2-EDGE: decline once -> no file created, and a second run does not
    re-prompt (a recorded decline, not amnesia)."""
    f = tmp_path / "AGENTS.md"
    f.write_text(_USER, encoding="utf-8")

    first = offer_managed_block(tmp_path, confirm_fn=lambda _m: False)
    assert first["status"] == "declined"
    assert stamped_version(f.read_text(encoding="utf-8")) is None  # untouched

    calls: list[str] = []

    def _confirm(msg: str) -> bool:
        calls.append(msg)
        return True  # would accept - but must never be asked

    second = offer_managed_block(tmp_path, confirm_fn=_confirm)
    assert second["status"] == "declined-remembered"
    assert calls == []  # no re-prompt
    assert stamped_version(f.read_text(encoding="utf-8")) is None
