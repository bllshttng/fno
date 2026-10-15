"""The Python coverage reader's tiled-chain exemption (AC4, Python half).

The Rust producer walks the attestation ranges with git and writes the
`range_tiling` answer onto the `review_coverage` row; Python READS it (the
Ownership rule) so both surfaces apply the same chain exemption. Without the
exemption a chain member's single-sha freshness reads stale and the shaper
invalidates the row - the exact one-artifact-voids-per-fix loop the tiling
exists to end.
"""
from __future__ import annotations

import json
from pathlib import Path

from fno.pr import _reviews


def _row(*, tiled=None, chain=None, freshness="stale", reviewed_sha="c3", rounds_exhausted=None):
    """One review_coverage row: a local_attestation verdict plus tiling data."""
    data = {
        "pr": 42,
        "coverage": "covered",
        "reviewed_count": 1,
        "head_sha": "head",
        "verdicts": [
            {
                "producer": "local_attestation",
                "name": "code-review",
                "verdict": "reviewed",
                "reviewed_sha": reviewed_sha,
                "freshness": freshness,
                "scope": "attested_branch",
            }
        ],
    }
    if rounds_exhausted is not None:
        data["rounds_exhausted"] = rounds_exhausted
    if tiled is not None or chain is not None:
        data["range_tiling"] = {
            "tiled": bool(tiled),
            "gaps": [] if tiled else ["c1..c3"],
            "dropped": [],
            "chain_heads": list(chain or []),
        }
    return data


class TestTilingChainExemption:
    def test_tiled_chain_member_keeps_the_row_covered(self):
        # The stored freshness label is stale and the shaper recomputes
        # nothing, so only the chain exemption can carry this verdict: the
        # test proves the exemption rather than a lucky freshness carry.
        shaped = _reviews._shape_review_coverage(
            _row(tiled=True, chain=["c3"], reviewed_sha="c3", freshness="stale"),
            head="head",
            cwd=None,
        )
        assert shaped["coverage"] == "covered"
        assert shaped["review_state"] == "reviewed"
        assert shaped["reviewed_count"] == 1

    def test_no_tiling_data_keeps_today_single_sha_rule(self):
        shaped = _reviews._shape_review_coverage(
            _row(reviewed_sha="c3", freshness="stale"),
            head="head",
            cwd=None,
        )
        assert shaped["coverage"] == "uncovered"
        assert shaped["reviewed_count"] == 1

    def test_not_tiled_chain_does_not_rescue(self):
        shaped = _reviews._shape_review_coverage(
            _row(tiled=False, chain=["c3"], reviewed_sha="c3", freshness="stale"),
            head="head",
            cwd=None,
        )
        assert shaped["coverage"] == "uncovered"

    def test_chain_member_outside_the_chain_is_not_rescued(self):
        shaped = _reviews._shape_review_coverage(
            _row(tiled=True, chain=["c9"], reviewed_sha="c3", freshness="stale"),
            head="head",
            cwd=None,
        )
        assert shaped["coverage"] == "uncovered"

    def test_fresh_verdict_still_counts_without_any_chain(self):
        shaped = _reviews._shape_review_coverage(
            _row(reviewed_sha="head", freshness="fresh"),
            head="head",
            cwd=None,
        )
        assert shaped["coverage"] == "covered"


class TestTilingChainReader:
    def test_tiled_row_yields_its_chain_heads(self):
        assert _reviews._tiling_chain(_row(tiled=True, chain=["c1", "c3"])) == {"c1", "c3"}

    def test_not_tiled_row_yields_empty(self):
        assert _reviews._tiling_chain(_row(tiled=False, chain=["c1"])) == set()

    def test_row_without_tiling_yields_empty(self):
        assert _reviews._tiling_chain(_row()) == set()

    def test_malformed_tiling_yields_empty(self):
        assert _reviews._tiling_chain({"range_tiling": {"tiled": "yes"}}) == set()
        assert _reviews._tiling_chain({"range_tiling": None}) == set()
        assert _reviews._tiling_chain({"range_tiling": {"tiled": True, "chain_heads": "c1"}}) == set()

    def test_non_string_heads_are_dropped(self):
        assert _reviews._tiling_chain(
            {"range_tiling": {"tiled": True, "chain_heads": ["c1", 7, None, ""]}}
        ) == {"c1"}


# ---- the cached negative expires, and names what it was pinned to ----
#
# An UNCOVERED row pinned to the CURRENT head passes every freshness check the
# reader applied, because the head matches. So it was never refreshed, and a
# valid head-pinned pass attestation landing after it was ignored forever.
# Measured on one PR: the row was written at 07:15:38Z, the attestation for the
# same head landed at 07:48:55Z, and the gate still read uncovered.

_H = "74106361b0000000000000000000000000000000"


def _write_log(tmp_path, *events):
    (tmp_path / ".fno").mkdir(parents=True, exist_ok=True)
    (tmp_path / ".fno" / "events.jsonl").write_text(
        "\n".join(json.dumps(e) for e in events) + "\n", encoding="utf-8"
    )


def _uncovered_row(ts, head=_H, pr=1242):
    return {
        "ts": ts,
        "type": "review_coverage",
        "source": "hook",
        "data": {
            "pr": pr,
            "coverage": "uncovered",
            "review_state": "unreviewed",
            "reviewed_count": 0,
            "head_sha": head,
        },
    }


def _covered_row(ts, head=_H, pr=1242):
    return {
        "ts": ts,
        "type": "review_coverage",
        "source": "hook",
        "data": {
            "pr": pr,
            "coverage": "covered",
            "review_state": "reviewed",
            "reviewed_count": 1,
            "head_sha": head,
            # A modern producer resolves a rung on every covered row; a row
            # without this key reads as written by a pre-posture producer and
            # the gate demands a recompute, which is its own test below.
            "review_posture": {
                "posture": "self_review",
                "rank": 3,
                "source": "legacy",
                "posture_satisfied": True,
                "posture_gaps": [],
            },
            # The shaper refuses a covered row carrying no verdict proof, so a
            # fixture without this shapes back to uncovered and the test would
            # read as "the recompute did not help" when it did.
            "verdicts": [
                {
                    "producer": "local_attestation",
                    "name": "code-review",
                    "verdict": "reviewed",
                    "reviewed_sha": head,
                    "freshness": "fresh",
                    "attestation_origin": "other_session",
                }
            ],
        },
    }


def _pass_attestation(ts, head=_H):
    return {
        "ts": ts,
        "type": "review_attestation",
        "source": "hook",
        "data": {
            "reviewer": "code-review",
            "head_sha": head,
            "verdict": "pass",
            "session_id": "s-other",
            "branch": "feature/x-e555",
        },
    }


class TestPosturelessRowRecomputeScope:
    """A covered row with no review_posture is unusable to a GATE (the
    producer predates the field), but only a caller that can act on the
    producer-upgrade refusal pays the recompute; a receipt surface reports
    the row as-is instead of rewriting the same postureless row every read."""

    def _postureless_world(self, monkeypatch, tmp_path):
        _isolate_logs(monkeypatch, tmp_path)
        row = _covered_row("2026-08-28T08:00:00Z")
        del row["data"]["review_posture"]
        _write_log(tmp_path, row)
        fired = []
        return fired



def _isolate_logs(monkeypatch, tmp_path):
    """Read only the fixture log: no global mirror, no repo slug."""
    monkeypatch.setattr(
        _reviews,
        "_coverage_logs",
        lambda cwd=None, project_events=None: (
            Path(tmp_path) / ".fno" / "events.jsonl",
            None,
            None,
        ),
    )


class TestCachedNegativeNamesItsInput:

    def test_split_pin_note_separates_two_different_claims(self):
        """One says what an instrument did, the other what a stored answer
        rests on. A consumer keyed on the first must not receive the second."""
        joined = "recomputed; coverage row pinned to abc123def at 2026-08-28T07:15:38Z"
        recompute, pin = _reviews.split_pin_note(joined)
        assert recompute == "recomputed"
        assert pin == "coverage row pinned to abc123def at 2026-08-28T07:15:38Z"
        assert _reviews.split_pin_note("") == ("", "")
        assert _reviews.split_pin_note("recomputed") == ("recomputed", "")


    def test_an_unparseable_timestamp_falls_back_without_raising(self):
        """A malformed line fails quiet, the same as the chain read does."""
        assert _reviews._instant("not-a-timestamp") is None
        assert _reviews._is_after("zzz", "aaa", None) is True
        assert _reviews._is_after("aaa", "zzz", None) is False


def test_review_state_matches_the_shared_table():
    """One oracle, two readers: the Rust review_state test walks the SAME
    file, so the two derivations cannot drift on which refusals name the
    state (the optional_apps_default.json pattern)."""
    golden = json.loads(
        (Path(__file__).resolve().parents[1] / "config" / "review_state_table.json").read_text()
    )
    assert golden["rows"], "a golden nothing reads is a green control aimed at nothing"
    for row in golden["rows"]:
        state = _reviews._derive_review_state(
            "covered",
            row["verdicts"],
            set(),
            False,
            rounds_exhausted=row.get("rounds_exhausted", False),
        )
        assert state == row["expected_state"], f"row {row['name']} diverged: {state}"


class TestSpentBudgetShaping:
    def test_spent_row_with_a_stale_verdict_stays_covered_and_reviewed(self):
        # d-0fa92eb9: at the cap the row IS reviewed. The shaper keeps the
        # producer's covered word and the producer's count - it changes the
        # state word only.
        shaped = _reviews._shape_review_coverage(
            _row(reviewed_sha="c3", freshness="stale", rounds_exhausted=True),
            head="head",
            cwd=None,
        )
        assert shaped["coverage"] == "covered"
        assert shaped["review_state"] == "reviewed"
        assert shaped["reviewed_count"] == 1

    def test_the_same_row_unspent_still_reads_uncovered(self):
        shaped = _reviews._shape_review_coverage(
            _row(reviewed_sha="c3", freshness="stale", rounds_exhausted=False),
            head="head",
            cwd=None,
        )
        assert shaped["coverage"] == "uncovered"
        assert shaped["review_state"] == "unreviewed"
        assert shaped["reviewed_count"] == 1

    def test_spent_row_with_a_malformed_verdict_names_no_state(self):
        # A malformed list returns None before the spent arm, so a broken
        # row fails closed even past the budget.
        row = _row(rounds_exhausted=True)
        row["verdicts"] = [{"producer": "local_attestation", "verdict": "junk"}]
        shaped = _reviews._shape_review_coverage(row, head="head", cwd=None)
        assert "review_state" not in shaped
