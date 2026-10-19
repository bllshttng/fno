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
    def test_a_later_pass_at_the_same_head_forces_a_recompute(
        self, monkeypatch, tmp_path
    ):
        """The measured failure. Asserting that a fresh attestation on a FRESH
        head works proves nothing: that path already worked, which is exactly
        why this went unnoticed for two readers on one PR."""
        _isolate_logs(monkeypatch, tmp_path)
        _write_log(
            tmp_path,
            _uncovered_row("2026-08-28T07:15:38Z"),
            _pass_attestation("2026-08-28T07:48:55Z"),
        )
        fired = []

        def fake_fire(pr_number, cwd, head):
            fired.append((pr_number, head))
            # The producer's job: it rewrites the row for this head.
            _write_log(
                tmp_path,
                _uncovered_row("2026-08-28T07:15:38Z"),
                _pass_attestation("2026-08-28T07:48:55Z"),
                _covered_row("2026-08-28T08:00:00Z"),
            )
            return True, ""

        monkeypatch.setattr(_reviews, "_fire_review_coverage_verb", fake_fire)
        data, note = _reviews.review_coverage_for_gate(1242, str(tmp_path), _H)

        # Positive markers only: the recompute RAN, and the answer flipped.
        assert fired == [(1242, _H)], fired
        assert data["coverage"] == "covered"
        assert "recomputed" in note

    def test_an_earlier_attestation_does_not_force_a_recompute(
        self, monkeypatch, tmp_path
    ):
        """The arm is narrow on purpose. An attestation OLDER than the row is
        evidence the row already accounted for, and recomputing on it would
        fire on every uncovered read forever - a refusal turned into a spin."""
        _isolate_logs(monkeypatch, tmp_path)
        _write_log(
            tmp_path,
            _pass_attestation("2026-08-28T06:00:00Z"),
            _uncovered_row("2026-08-28T07:15:38Z"),
        )
        fired = []
        monkeypatch.setattr(
            _reviews,
            "_fire_review_coverage_verb",
            lambda *a, **k: (fired.append(a) or (True, "")),
        )
        data, note = _reviews.review_coverage_for_gate(1242, str(tmp_path), _H)

        assert fired == []
        assert data["coverage"] == "uncovered"
        # The row still names what it was pinned to: the corollary-5 half holds
        # whether or not the recompute arm fires.
        assert note == "coverage row pinned to 74106361b at 2026-08-28T07:15:38Z"

    def test_the_recomputed_row_is_not_overtaken_again(
        self, monkeypatch, tmp_path
    ):
        """The recompute writes a row stamped after the attestation, so the
        next read finds nothing newer than the row - the arm cannot spin."""
        _isolate_logs(monkeypatch, tmp_path)
        failed = _pass_attestation("2026-08-28T07:48:55Z")
        failed["data"]["verdict"] = "fail"
        _write_log(
            tmp_path,
            _uncovered_row("2026-08-28T07:15:38Z"),
            failed,
            _uncovered_row("2026-08-28T08:00:00Z"),
        )
        fired = []
        monkeypatch.setattr(
            _reviews,
            "_fire_review_coverage_verb",
            lambda *a, **k: (fired.append(a) or (True, "")),
        )
        data, note = _reviews.review_coverage_for_gate(1242, str(tmp_path), _H)

        assert fired == []
        assert "recomputed" not in note

    def test_the_pin_does_not_ride_the_recompute_key(self, monkeypatch, tmp_path):
        """`recompute` promises a recompute RAN, and readers act on that.

        `_status.coverage_recompute_note` prints every value but the literal
        "recomputed", so folding the pin into that key narrates a recompute
        that never happened on every status poll of an uncovered PR.
        """
        _isolate_logs(monkeypatch, tmp_path)
        _write_log(tmp_path, _uncovered_row("2026-08-28T07:15:38Z"))
        monkeypatch.setattr(
            _reviews, "_fire_review_coverage_verb", lambda *a, **k: (False, "no binary")
        )
        # A head-matching uncovered row with no later attestation runs no
        # recompute, so the ONLY note is the pin. It must not arrive wearing
        # the recompute key.
        shaped = _reviews.read_review_coverage(
            1242, str(tmp_path), _H, recompute=True
        )
        assert shaped["coverage_pin"].startswith("coverage row pinned to 74106361b")
        assert "recompute" not in shaped, shaped.get("recompute")

        # And where a recompute DOES run, both keys carry their own claim.
        # Asserting only the case above would pass on a build that dropped the
        # recompute note entirely.
        other = "ffffffff00000000000000000000000000000000"
        shaped = _reviews.read_review_coverage(
            1242, str(tmp_path), other, recompute=True
        )
        assert shaped["recompute"] == "recompute unavailable: no binary"
        assert "pinned to" not in shaped["recompute"]

    def test_a_same_second_pass_in_the_other_spelling_still_overtakes(
        self, monkeypatch, tmp_path
    ):
        """The two producers spell one moment differently.

        The Rust gate writes second precision with a `Z`. Python's event log
        writes `datetime.isoformat()`, so microseconds and `+00:00`. Both `.`
        and `+` sort BEFORE `Z`, so a lexical compare reads the Python
        attestation as OLDER than the Rust row it actually followed, and the
        cached uncovered row stays stuck exactly when the two land in one
        second. Asserted on the real spellings, not on invented ones.
        """
        _isolate_logs(monkeypatch, tmp_path)
        row = _uncovered_row("2026-08-28T07:15:38Z")
        later = _pass_attestation("2026-08-28T07:15:38.123456+00:00")
        # The trap, stated as the measurement: lexically the pass reads older.
        assert not (later["ts"] > row["ts"])
        _write_log(tmp_path, row, later)
        fired = []
        monkeypatch.setattr(
            _reviews,
            "_fire_review_coverage_verb",
            lambda *a, **k: (fired.append(a) or (True, "")),
        )
        _reviews.review_coverage_for_gate(1242, str(tmp_path), _H)
        assert fired, "a same-second pass in the other spelling did not overtake"

    def test_split_pin_note_separates_two_different_claims(self):
        """One says what an instrument did, the other what a stored answer
        rests on. A consumer keyed on the first must not receive the second."""
        joined = "recomputed; coverage row pinned to abc123def at 2026-08-28T07:15:38Z"
        recompute, pin = _reviews.split_pin_note(joined)
        assert recompute == "recomputed"
        assert pin == "coverage row pinned to abc123def at 2026-08-28T07:15:38Z"
        assert _reviews.split_pin_note("") == ("", "")
        assert _reviews.split_pin_note("recomputed") == ("recomputed", "")
