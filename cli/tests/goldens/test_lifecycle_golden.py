"""Golden receipts: done, reopen, defer, contain, supersede."""
from __future__ import annotations

from tests.goldens._door import door, graph_rows, make_sandbox, seed_node, warm


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


# -- reopen --


def test_reopen_reads_the_completion_not_the_status_field(tmp_path):
    # A row stamped done without a completion timestamp reads as live to
    # reopen: the completion stamp is the evidence of done, not the status.
    root = make_sandbox(tmp_path, [seed_node("x-aaaa1111", "done")])
    warm(root, "x-aaaa1111")
    code, out, err = door(root, ["reopen", "x-aaaa1111", "-R", "not actually done"])
    assert code == 0, err
    assert out == "", out
    assert err == "warning: x-aaaa1111 is not done; nothing to reopen\n", err

    # The exception is the rollup's own shape: a status-done row whose
    # children are all closed is a children-rollup derived done, and reopen
    # stamps the reopened_at hold the rollup respects.
    derived_root = tmp_path / "derived"
    derived_root.mkdir()
    derived = make_sandbox(
        derived_root,
        [
            seed_node("x-aaaa1111", "done"),
            seed_node("x-bbbb2222", "done", parent="x-aaaa1111", completed_at="2026-09-27T10:00:00+00:00"),
        ],
    )
    warm(derived, "x-aaaa1111")
    code, out, err = door(derived, ["reopen", "x-aaaa1111", "-R", "epic work continues"])
    assert code == 0, err
    assert out == "Reopened x-aaaa1111\n", out


# -- defer --


def test_defer_prints_the_deferred_receipt_and_stamps_the_kind(tmp_path):
    root = make_sandbox(tmp_path, [seed_node("x-cccc3333")])
    warm(root, "x-cccc3333")
    code, out, err = door(root, ["defer", "x-cccc3333", "-R", "parked until api lands", "-K", "later"])
    assert code == 0, err
    assert out == 'Deferred x-cccc3333: "parked until api lands"\n', out
    rows = {r["id"]: r for r in graph_rows(root)}
    assert rows["x-cccc3333"]["status"] == "deferred"
    assert rows["x-cccc3333"]["deferred_kind"] == "later"


# -- contain --

def test_contain_stamps_parent_and_contained_in_and_prints_per_child(tmp_path):
    root = make_sandbox(
        tmp_path,
        [seed_node("x-ccc99000", "in_progress"), seed_node("x-ddd10000"), seed_node("x-eee20000")],
    )
    warm(root, "x-ccc99000")
    code, out, err = door(root, ["contain", "x-ccc99000", "x-ddd10000", "x-eee20000"])
    assert code == 0, err
    assert out == (
        "contained x-ddd10000 into x-ccc99000; it ships inside x-ccc99000's PR\n"
        "contained x-eee20000 into x-ccc99000; it ships inside x-ccc99000's PR\n"
    ), out
    rows = {r["id"]: r for r in graph_rows(root)}
    assert rows["x-ddd10000"]["parent"] == "x-ccc99000"
    assert rows["x-ddd10000"]["contained_in"] == "x-ccc99000"


# -- supersede --

def test_supersede_without_a_cause_prints_the_evidence_trail_refusal(tmp_path):
    root = make_sandbox(tmp_path, [seed_node("x-aaa11010"), seed_node("x-bbb22000")])
    warm(root, "x-aaa11010")
    code, out, err = door(root, ["supersede", "x-bbb22000", "--replaces", "x-aaa11010"])
    assert code == 1, err
    assert out == "", out
    assert "--cause is required and cannot be blank." in err
    assert "A supersede carries the evidence trail" in err
    assert "fno backlog supersede <new> --replaces <old>" in err


def test_supersede_a_shipped_target_refuses(tmp_path):
    root = make_sandbox(tmp_path, [seed_node("x-ccc33000", "done"), seed_node("x-bbb22000")])
    warm(root, "x-bbb22000")
    code, out, err = door(
        root,
        ["supersede", "x-bbb22000", "--replaces", "x-ccc33000", "--cause", "old approach", "--surface", "a.py"],
    )
    assert code == 1, err
    assert err == (
        "Error: cannot supersede x-ccc33000: it is already shipped (status=done)."
        " Open a follow-up node instead.\n"
    ), err


def test_supersede_stamps_the_edge_and_superseded_status(tmp_path):
    root = make_sandbox(tmp_path, [seed_node("x-aaa11010"), seed_node("x-bbb22000")])
    warm(root, "x-aaa11010")
    code, out, err = door(
        root,
        ["supersede", "x-bbb22000", "--replaces", "x-aaa11010", "--cause", "old approach", "--surface", "a.py"],
    )
    assert code == 0, err
    assert out == "superseded x-aaa11010 with x-bbb22000\n", out
    rows = {r["id"]: r for r in graph_rows(root)}
    assert rows["x-aaa11010"]["status"] == "superseded"
    assert rows["x-aaa11010"]["superseded_by"] == "x-bbb22000"
    assert rows["x-bbb22000"]["supersedes"] == ["x-aaa11010"]
