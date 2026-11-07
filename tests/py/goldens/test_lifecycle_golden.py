"""Golden receipts: done, reopen, defer, contain, supersede."""
from __future__ import annotations

from goldens._door import door, graph_rows, make_sandbox, seed_node, warm


# -- done --

def test_done_with_note_closes_and_prints_the_close_receipt(tmp_path):
    root = make_sandbox(tmp_path, [seed_node("x-aaaa1111", "in_progress")])
    warm(root, "x-aaaa1111")
    code, out, err = door(root, ["done", "x-aaaa1111", "--note", "wrapped by hand"])
    assert code == 0, err
    assert out == (
        "fno backlog done: x-aaaa1111 -> done  domain=code"
        "  note='wrapped by hand'  session=test-ses\n"
    ), out


def test_done_without_evidence_refuses_and_writes_nothing(tmp_path):
    root = make_sandbox(tmp_path, [seed_node("x-aaaa1111", "in_progress")])
    warm(root, "x-aaaa1111")
    code, out, err = door(root, ["done", "x-aaaa1111"])
    assert code == 1, err
    assert out == "", out
    assert err.startswith("Error: refused:"), err
    assert "would close done with no record of why" in err
    assert "Nothing was written." in err
    assert "--pr-number <n>" in err
    assert '--note "<why it is done>"' in err
    assert "--link <artifact url>" in err


# -- supersede --


def test_a_narrower_successor_refuses_until_the_leftover_is_filed(tmp_path):
    narrow = seed_node("x-bbb22000", details="Pass 1: docs. Pass 2 (not this node): code identifiers.")
    root = make_sandbox(tmp_path, [seed_node("x-aaa11010"), narrow, seed_node("x-ccc33000", "idea")])
    warm(root, "x-aaa11010")
    base = ["supersede", "x-bbb22000", "--replaces", "x-aaa11010", "--cause", "old approach", "--surface", "a.py"]
    code, out, err = door(root, [*base, "--covers", "renames everything"])
    assert code == 1, err
    assert out == "", out
    assert "--reason is required and cannot be blank." in err
    code, out, err = door(root, [*base, "--reason", "folded in", "--covers", "renames everything"])
    assert code == 1, err
    assert 'its own text says "not this node"' in err
    assert "fno backlog idea" in err
    assert {r["id"]: r for r in graph_rows(root)}["x-aaa11010"].get("superseded_by") is None

    code, out, err = door(root, [*base, "--reason", "folded in", "--leftover", "x-ccc33000"])
    assert code == 0, err
    assert out == "superseded x-aaa11010 with x-bbb22000\n", out
    rows = {r["id"]: r for r in graph_rows(root)}
    assert rows["x-aaa11010"]["status"] == "superseded"
    assert rows["x-aaa11010"]["superseded_by"] == "x-bbb22000"
    assert rows["x-aaa11010"]["supersession"]["reason"] == "folded in; leftover: x-ccc33000"
    assert rows["x-bbb22000"]["supersedes"] == ["x-aaa11010"]
