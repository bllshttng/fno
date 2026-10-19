"""The rolling 24h wake ledger: the window rolls, the 22nd wake lands, the 33rd refuses.

The ledger lives on the lead manifest as ``wake_times``, a comma-joined list of
RFC3339 UTC stamps pruned to the trailing 24 hours at every read and write. The
tests assert POSITIVE markers: an allow verdict with a count, a refusal naming
its word, a rewritten line with every other line byte-identical. An emptied
list and an unreadable store read differently here (empty list vs absent) only
because read_wakes returns a list and the tests pin which is which.
"""
from __future__ import annotations

from datetime import datetime, timedelta, timezone

from fno.lead.wake import admit_wake, bill_wake, read_wakes, should_wake

NOW = datetime(2026, 8, 29, 12, 0, 0, tzinfo=timezone.utc)

CEILING = 32
DEBOUNCE_S = 900


def _stamp(minutes_ago: float) -> datetime:
    return NOW - timedelta(minutes=minutes_ago)


def _manifest(path, stamps: list[datetime], *, extra: str = "") -> None:
    joined = ",".join(s.strftime("%Y-%m-%dT%H:%M:%SZ") for s in stamps)
    body = (
        "---\n"
        'fno_id: "20260829T120000Z-kg1-abc123"\n'
        "scope: epic-x\n"
        "respawn_count: 3\n"
        "respawn_ceiling: 4\n"
        f"wake_times: {joined}\n"
        "---\n"
        "A body line the rewrite must not touch.\n"
    )
    if extra:
        body += extra
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")


def _thirty_two_at_the_ceiling() -> list[datetime]:
    """32 stamps, oldest 23h30m ago, newest 45m ago: a full, fresh window."""
    return [_stamp(1410 - 45 * i) for i in range(32)]


def test_the_window_rolls_it_does_not_tumble(tmp_path):
    # The half that separates a rolling window from an anchored one: 32 stamps
    # fill the window now, and the oldest ages out WITHOUT the anchor moving.
    # A window keyed on the first-ever wake would keep refusing; a rolling one
    # drops the aged stamp and the next trigger is allowed.
    stamps = _thirty_two_at_the_ceiling()
    path = tmp_path / "leads" / "epic-x.md"
    _manifest(path, stamps)
    refused = should_wake(path, now=NOW, ceiling=CEILING, debounce_s=DEBOUNCE_S)
    assert refused.refusal == "ceiling", "fixture must start at the ceiling"

    # 45 minutes later the oldest stamp (23h30m old) is past 24h and aged
    # out; 31 remain, which is back under the ceiling of 32.
    later = NOW + timedelta(minutes=45)

    verdict = should_wake(path, now=later, ceiling=CEILING, debounce_s=DEBOUNCE_S)

    assert verdict.allowed, (
        f"the oldest stamp is past 24h; the window must roll: {verdict}"
    )
    assert verdict.count == 31


def test_bill_wake_appends_prunes_and_touches_no_other_line(tmp_path):
    path = tmp_path / "leads" / "epic-x.md"
    stale = [_stamp(25 * 60)]  # outside the trailing 24h
    fresh = [_stamp(60), _stamp(30)]
    _manifest(path, stale + fresh)
    before = path.read_text(encoding="utf-8")

    count = bill_wake(path, now=NOW)

    after = path.read_text(encoding="utf-8")
    assert count == 3, "the two fresh stamps plus this one"
    kept = read_wakes(path, now=NOW)
    assert len(kept) == 3, "the stale stamp is pruned on the write"
    lines_before = before.splitlines()
    lines_after = after.splitlines()
    assert len(lines_before) == len(lines_after)
    for i, (b, a) in enumerate(zip(lines_before, lines_after)):
        if b.startswith("wake_times:"):
            assert not a == b, "the wake_times line must move"
        else:
            assert a == b, f"line {i} changed: {b!r} -> {a!r}"


def test_admit_wake_bills_on_allow_so_the_second_tick_takes_the_refusal(tmp_path):
    # The two-reader race: two ticks both read an empty ledger. Deciding and
    # billing under one lock means the FIRST call lands its stamp before the
    # second ever reads, so the second answers the same question with the
    # winner's stamp in view.
    path = tmp_path / "leads" / "epic-x.md"
    _manifest(path, [])

    first = admit_wake(path, now=NOW, ceiling=CEILING, debounce_s=DEBOUNCE_S)
    second = admit_wake(path, now=NOW, ceiling=CEILING, debounce_s=DEBOUNCE_S)

    assert first.allowed and first.count == 1
    assert not second.allowed and second.refusal == "debounce"
    assert len(read_wakes(path, now=NOW)) == 1, "exactly one wake was billed"


def test_a_ceiling_of_zero_is_unbounded_even_past_the_default(tmp_path):
    path = tmp_path / "leads" / "epic-x.md"
    _manifest(path, [_stamp(20 + 60 * i) for i in range(40)])

    verdict = should_wake(path, now=NOW, ceiling=0, debounce_s=DEBOUNCE_S)

    assert verdict.allowed, "40 stamps pass when the operator disabled the cap"


def test_admit_wake_retains_enough_stamps_for_a_ceiling_above_the_default(tmp_path):
    # 63 stamps inside the window, spaced past debounce: the 64th wake under
    # a configured ceiling of 64 is allowed, and the prune must not then cap
    # the stored ledger at the default 32 - the gate reads len(stamps), so a
    # ledger that cannot hold 64 stamps can never refuse at 64 and the
    # operator's raised cap silently stops bounding anything.
    path = tmp_path / "leads" / "epic-x.md"
    _manifest(path, [_stamp(20 + 15 * i) for i in range(63)])

    verdict = admit_wake(path, now=NOW, ceiling=64, debounce_s=DEBOUNCE_S)

    assert verdict.allowed and verdict.count == 64
    assert len(read_wakes(path, now=NOW)) == 64, "the ledger must hold 64 stamps"
