"""The operator lane parser: the single implementation two consumers read.

A missing file is an empty lane; an unreadable one sets `error`. Those two
must never collapse into each other, which is why each has its own test.
"""
from __future__ import annotations

from fno.lead.lane import open_items, parked_items, read_lane


def _write(tmp_path, text):
    path = tmp_path / "my-priorities.md"
    path.write_text(text, encoding="utf-8")
    return path


def test_a_filed_item_carries_its_node_and_is_not_open(tmp_path):
    path = _write(tmp_path, "- [ ] ship it -> x-aaae\n")
    read = read_lane(path)
    item = read.items[0]
    assert item.node == "x-aaae"
    assert item.text == "ship it"
    assert open_items(read) == []


def test_a_parked_item_carries_its_reason_and_is_not_open(tmp_path):
    path = _write(tmp_path, "- [ ] call the dentist -> parked: not node-shaped\n")
    read = read_lane(path)
    item = read.items[0]
    assert item.parked == "not node-shaped"
    assert item.text == "call the dentist"
    assert open_items(read) == []
    assert [i.text for i in parked_items(read)] == ["call the dentist"]


def test_a_bare_park_with_no_reason_is_not_a_park(tmp_path):
    """An unreasoned `-> parked:` stays open; \\S in the reason group enforces it."""
    path = _write(tmp_path, "- [ ] call the dentist -> parked:\n")
    read = read_lane(path)
    item = read.items[0]
    assert item.parked is None
    assert item.node is None
    assert item.text == "call the dentist -> parked:"
    assert [i.text for i in open_items(read)] == ["call the dentist -> parked:"]
    assert parked_items(read) == []


def test_missing_file_is_an_empty_lane_not_an_error(tmp_path):
    read = read_lane(tmp_path / "does-not-exist.md")
    assert read.error == ""
    assert read.items == []


def test_undecodable_file_sets_error(tmp_path):
    path = tmp_path / "my-priorities.md"
    path.write_bytes(b"\xff\xfe\x00bad")
    read = read_lane(path)
    assert read.error
    assert "cannot read operator lane" in read.error
