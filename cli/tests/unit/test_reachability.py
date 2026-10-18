"""One reachability derivation with a declared basis, behind every agents surface.

Written RED, before the implementation, per the node's verify-by-making-it-fail.

Three assertion families, because the node's single proposed assertion is not
enough:

1. CONGRUENCE -- every surface that reports reachability returns the same
   verdict for the same agent over one snapshot.
2. CORRECTNESS -- congruence is not correctness. Two surfaces can agree and both
   be wrong, which is exactly what happens today: a session dead 43 minutes
   reads ``live`` from every transcript-keyed surface because the whole liveness
   axis has two-hour resolution. So a dead fixture must read ``unreachable`` at
   an age BELOW the stalled threshold.
3. MONOTONICITY -- a falsifier may only ever lower a verdict toward
   ``unreachable``. This is the invariant that keeps the destructive reaping
   rule un-rederivable; it is asserted over the falsifier set directly, not just
   end-to-end, because an end-to-end test passes right up until someone adds a
   "the pane is alive so it must be live" rule.

The node's own proposed test ("assert list and status return identical
per-agent statuses") CANNOT be written as specified: ``fno agents status`` emits
only a histogram (``agents: {total, by_status}``, daemon.rs), with no per-agent
rows to compare. Congruence is therefore asserted over the derivation the
surfaces consult, not over ``status`` output.
"""

from __future__ import annotations

import pytest


# The module under test does not exist yet. Importing it is the red.
from fno.agents.reachability import (
    MODEL_REFUSED,
    NO_EVIDENCE,
    NO_INFERENCE,
    PROVIDER_REFUSED,
    REFUSED,
    REACHABLE,
    TRANSCRIPT,
    TRANSCRIPT_TURN,
    UNKNOWN,
    UNREACHABLE,
    Reachability,
    classify_progress,
    classify_reachability,
    rendered_activity,
)


# --------------------------------------------------------------------------
# 2. CORRECTNESS -- the bug this node exists for.
# --------------------------------------------------------------------------


def test_dead_session_below_the_stalled_threshold_is_unreachable() -> None:
    """The lead's 2026-08-07 specimen: dead 43 minutes, reported live.

    43 minutes is well under STALLED_AFTER_S, so the transcript classifies
    ``working`` and every transcript-keyed surface renders ``live``. Only an
    affirmative falsifier can catch it. This is the assertion the node omits and
    the one that fails today at ANY age under two hours.
    """
    got = classify_reachability(
        truth_state="working",
        age_s=43 * 60,
        falsifier="process-gone",
    )
    assert got.verdict == UNREACHABLE
    assert got.basis == "process-gone"
    assert got.age_s == 43 * 60


# --------------------------------------------------------------------------
# Framing rule (a): a registry of REACHABLE agents, not a process table.
# "orphaned means unreachable, not dead, so never reap a row for being quiet."
# --------------------------------------------------------------------------


def test_silence_alone_never_produces_unreachable() -> None:
    """Quiet is absence of evidence, not evidence of absence.

    A row silent for 34.7 hours with no falsifier available (89 percent of rows
    carry no pid) is genuinely unknown-reachability. Asserting ``unreachable``
    there claims knowledge we do not have, which is the exact class of lie this
    node is about. The age rides along in the value so the reader can draw their
    own conclusion.
    """
    got = classify_reachability(
        truth_state="stalled",
        age_s=int(34.7 * 3600),
        falsifier=None,
    )
    assert got.verdict == UNKNOWN, "silence must never be reported as unreachable"
    assert got.age_s == int(34.7 * 3600)


# --------------------------------------------------------------------------
# 3. MONOTONICITY -- the invariant that keeps the reaping rule un-rederivable.
# --------------------------------------------------------------------------

_ALL_TRUTH_STATES = ("working", "watching", "your-move", "done", "stalled", "unknown")


@pytest.mark.parametrize("truth_state", ("working", "done", "unknown"))
def test_a_falsifier_never_raises_a_verdict(truth_state: str) -> None:
    """Adding a falsifier may lower a verdict and may never raise one.

    Asserted over the whole state space rather than a sampled case, so a future
    "the pane is alive, therefore live" rule fails here instead of shipping.
    """
    without = classify_reachability(truth_state=truth_state, age_s=10, falsifier=None)
    with_ = classify_reachability(truth_state=truth_state, age_s=10, falsifier="process-gone")
    assert _rank(with_.verdict) <= _rank(without.verdict), (
        f"falsifier RAISED the verdict for {truth_state}: {without.verdict} -> {with_.verdict}"
    )
    assert with_.verdict == UNREACHABLE


def _rank(verdict: str) -> int:
    """reachable > unknown > unreachable, for the monotonicity comparison."""
    return {UNREACHABLE: 0, UNKNOWN: 1, REACHABLE: 2}[verdict]


# --------------------------------------------------------------------------
# 4. Basis and age are part of the value: a bare "live" must be unprintable.
# --------------------------------------------------------------------------


def test_every_verdict_carries_a_basis_and_renders_with_it() -> None:
    for state in _ALL_TRUTH_STATES:
        got = classify_reachability(truth_state=state, age_s=61, falsifier=None)
        assert isinstance(got, Reachability)
        assert got.basis, f"{state} produced a verdict with no basis"
        rendered = got.render()
        assert got.verdict in rendered
        assert got.basis in rendered, "the basis must be visible to the reader"


# --------------------------------------------------------------------------
# 1. CONGRUENCE -- one snapshot, one verdict per agent, across every surface.
# --------------------------------------------------------------------------


def test_truth_json_carries_the_reachability_verdict_for_rust_callers() -> None:
    """The Python/Rust wire, which is where a congruence fix silently leaks.

    ``fno agents truth --json`` is what Rust's ``family1_truth_probe`` reads, and
    ``resume`` decides "is live - attaching" from it. If the verdict is not on
    that wire, Rust keeps re-deriving liveness from the raw transcript ``state``
    and the falsifier never reaches it -- the same trap, one language over.

    Asserted against the payload builder rather than a subprocess so it stays
    hermetic.
    """
    from fno.agents.cli import _truth_payload

    payload = _truth_payload(
        {
            "handle": "h",
            "state": "working",
            "reason": None,
            "last_activity_age_s": 43 * 60,
            "session_id": "s",
            "observed_model": {"kind": "no-model-yet"},
        },
        falsifier="process-gone",
    )
    assert payload["reachability"] == UNREACHABLE, (
        "a dead-process row must reach Rust as unreachable, not as working"
    )
    assert payload["basis"] == "process-gone"
    # The raw transcript state stays on the wire unchanged: existing Rust
    # consumers parse it, and overloading it would be a silent contract break.
    assert payload["state"] == "working"


def _pane_row():
    """A mux row whose recorded pid is confidently dead (pid 1 is never a worker).

    ``pane_id`` 7, not 0: ids are allocated from a floor of 1, so 0 is an
    unset field rather than a pane, and a row carrying it is not a pane row.
    """
    from types import SimpleNamespace

    return SimpleNamespace(pid=1, pid_start_time=None, mux={"session": "s", "pane_id": 7})


# --------------------------------------------------------------------------
# x-d914: a mux ref that names no pane must read like no ref at all.
# --------------------------------------------------------------------------


def _zero_ref_row(pid, status=None):
    """The damaged shape measured live: ``mux: {"session": "main", "pane_id": 0}``."""
    from types import SimpleNamespace

    return SimpleNamespace(
        pid=pid, pid_start_time=None, mux={"session": "main", "pane_id": 0}, status=status
    )


@pytest.mark.parametrize(
    "bad_mux",
    [
        {"session": "main", "pane_id": 0},
        {"session": "main", "pane_id": True},
        {"session": "", "pane_id": 7},
        None,
    ],
)
def test_a_ref_that_names_no_pane_is_invalid(bad_mux) -> None:
    """AC4-EDGE: every impossible shape fails validity and falls through.

    ``pane_id`` 0 is the live specimen (server.rs allocates from a floor of
    1), and the rest are the boundaries around it. ``True`` is excluded
    explicitly: ``isinstance(True, int)`` is True, and a ``pane_id`` of
    ``True`` is a leaked flag, not a pane.
    """
    from fno.agents.reachability import mux_ref_names_a_pane

    assert mux_ref_names_a_pane(bad_mux) is False


def test_a_zero_pane_ref_row_is_judged_by_its_pid(monkeypatch) -> None:
    """AC1-HP: a live worker behind a broken ref stays reachable.

    And AC9-INV at the row level: the mux is never asked, because validity is
    structural. The booms prove it -- either one firing fails this test.
    """
    from fno.agents import mux_spawn, spawn_gate
    from fno.agents.reachability import registry_falsifier

    def _boom(*_a):
        raise AssertionError("an invalid ref must never reach a probe")

    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", _boom)
    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda pid, start: True)
    assert registry_falsifier(_zero_ref_row(pid=4242, status="live")) is None


def test_a_reconciled_exit_survives_the_pid_being_cleared(monkeypatch) -> None:
    """Reconcile PROVES a worker gone and then destroys the proof.

    ``apply_reconcile_change`` nulls ``pid`` and ``pid_start_time`` on the
    terminal ``Exited`` transition (deliberately -- a stale pid is itself a
    misleading liveness signal). The row is then a no-pid row, and no-pid is
    absence of evidence here, so a worker whose transcript is still warm
    classifies ``reachable`` and ``resume`` picks the dead attach path: the
    original 43-minute false-live, re-entered through a different door.

    A recorded ``exited`` is not the stored-enum guess this derivation refuses
    to trust. It is an affirmative probe RESULT, written only after reconcile
    confirmed the child was gone, and re-stamped ``live`` by any later
    successful interaction -- so it falsifies, and nothing else stored does.
    """
    from types import SimpleNamespace

    from fno.agents.reachability import registry_falsifier

    reconciled_dead = SimpleNamespace(pid=None, pid_start_time=None, mux=None, status="exited")
    assert registry_falsifier(reconciled_dead) == "exit-recorded"


def test_no_other_stored_status_falsifies(monkeypatch) -> None:
    """Only ``exited`` is a probe result. The rest are guesses or the opposite.

    ``orphaned`` is the sharp case and it must NEVER falsify: reconcile keeps
    the pid on an orphaned row precisely because that process is still LIVE but
    unowned (daemon.rs), so condemning it from the stored word would reap a
    running worker. Falsifying on every terminal-sounding value is how the
    stored enum becomes the answer again.
    """
    from types import SimpleNamespace

    from fno.agents.reachability import registry_falsifier

    for stored in ("orphaned", "live", "spawning", "failed", "unknown", None):
        row = SimpleNamespace(pid=None, pid_start_time=None, mux=None, status=stored)
        assert registry_falsifier(row) is None, f"stored {stored!r} must not condemn a row"


def test_a_mux_pane_row_carries_no_pid_falsifier(monkeypatch) -> None:
    """A pane row's recorded pid is not the authority on that pane.

    ``reconcile`` re-derives the pane's current child on every pass, because a
    mux restart can hand ``(session, pane_id)`` to a new child while the recorded
    pid dies -- and it treats a live pane with no usable pid as INCONCLUSIVE,
    never dead. Falsifying off the stale pid would render a healthy pane worker
    ``orphaned`` in `list` and make `resume` refuse to attach it: the reaping
    hazard, rebuilt one field over.
    """
    from types import SimpleNamespace

    from fno.agents import mux_spawn
    from fno.agents.reachability import registry_falsifier

    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda mux: True)
    assert registry_falsifier(_pane_row()) is None

    bare_row = SimpleNamespace(pid=1, pid_start_time=None, mux=None)
    assert registry_falsifier(bare_row) == "process-gone"


def test_an_exited_pane_falsifies_the_row(monkeypatch) -> None:
    """Suppressing the stale pid must not discard the AUTHORITATIVE pane signal.

    ``_mux_pane_alive`` is exactly the authority the recorded pid is not: it
    answers about the pane itself, and a ``False`` from it is the mux stating
    the pane exited. Dropping every falsifier for a mux row -- rather than
    swapping the wrong one for the right one -- leaves a dead pane rendering
    ``unknown`` (or ``reachable``, while its transcript is still under the
    staleness window) for as long as that transcript stays warm.
    """
    from fno.agents import mux_spawn
    from fno.agents.reachability import registry_falsifier

    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda mux: False)
    assert registry_falsifier(_pane_row()) == "pane-gone"


def test_unreadable_pane_liveness_never_condemns(monkeypatch) -> None:
    """``None`` from the mux is "cannot answer", which is not evidence of death.

    Same rule as an unreadable pid: only a confident gone falsifies. ``reconcile``
    treats this case as ``mux-pane-liveness-unavailable`` and declines to act,
    and a row must not be condemned here for what reconcile refuses to condemn.
    """
    from fno.agents import mux_spawn
    from fno.agents.reachability import registry_falsifier

    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda mux: None)
    assert registry_falsifier(_pane_row()) is None


# --------------------------------------------------------------------------
# x-c1a3: transcript evidence is positive only while it is FRESH.
# --------------------------------------------------------------------------


def test_an_active_state_past_the_freshness_bound_demotes_to_unknown() -> None:
    """The x-52d2 reading: truth_state working, transcript 83 minutes silent.

    83 minutes sits inside the two-hour reap-safety window, so the verdict was
    REACHABLE and the node stayed wedged behind a session dead by four
    instruments. Past the bound the honest answer is UNKNOWN with basis
    stale-transcript -- never UNREACHABLE, because an old transcript is absence
    of evidence exactly as silence is.
    """
    from fno.agents.reachability import STALE_TRANSCRIPT, TRANSCRIPT_EVIDENCE_S

    assert TRANSCRIPT_EVIDENCE_S == 20 * 60
    got = classify_reachability(truth_state="working", age_s=4980, falsifier=None)
    assert got.verdict == UNKNOWN
    assert got.basis == STALE_TRANSCRIPT
    assert got.age_s == 4980


def test_an_unknowable_age_is_never_evidence_of_staleness() -> None:
    """Only POSITIVE evidence of staleness demotes; a missing age is not
    evidence, and the monotone rule in this module's header forbids lowering
    on absence."""
    got = classify_reachability(truth_state="working", age_s=None, falsifier=None)
    assert got.verdict == REACHABLE
    assert got.basis == "transcript"


# ---------------------------------------------------------------------------
# provider_refusal: the shared AC3/AC4 case table (x-e594)
#
# One table, two lanes: every row here is copied verbatim into the Rust test
# in crates/fno-agents/src/daemon/list_rows.rs. The lanes do not link, so the
# copy is the only thing keeping them from drifting apart on a corpse that
# used to read `writing` at 469 s.
# ---------------------------------------------------------------------------

_QUOTA = "provider_4xx_quota"

# (truth_state, age_s, reachability, provider_refusal)
#   -> (status, progress verdict, progress basis)
_PROVIDER_REFUSAL_CASES = [
    # AC3-HP: the measured corpse - dead 469 s, still reading writing before.
    ("working", 469, REACHABLE, _QUOTA, ("refused", "refused", PROVIDER_REFUSED)),
    # and over parked: a done tail plus a refusal is still a refusal
    ("done", 10, REACHABLE, _QUOTA, ("refused", "refused", PROVIDER_REFUSED)),
    # AC4-ERR: a positively falsified row stays orphaned - reachability first
    ("working", 469, UNREACHABLE, _QUOTA, ("orphaned", "unknown", NO_EVIDENCE)),
    # AC4-ERR: no refusal renders exactly as today
    ("working", 469, REACHABLE, None, ("writing", "advancing", TRANSCRIPT_TURN)),
]


@pytest.mark.parametrize("truth_state,age_s,reach,refusal,expected", _PROVIDER_REFUSAL_CASES)
def test_provider_refusal_case_table(truth_state, age_s, reach, refusal, expected):
    status = rendered_activity(
        truth_state=truth_state,
        age_s=age_s,
        reachability=reach,
        provider_refusal=refusal,
    )
    prog = classify_progress(
        truth_state=truth_state,
        reachability=reach,
        observed_model=None,
        harness="claude",
        route_settings_path=None,
        last_activity_age_s=age_s,
        provider_refusal=refusal,
    )
    assert (status, prog.verdict, prog.basis) == expected


def test_provider_refusal_sits_beside_the_model_refusal_arm() -> None:
    """The structural model-refused arm keeps precedence (Locked Decision 3):
    provider-refused is the transcript-text arm BELOW it, never a replacement."""
    prog = classify_progress(
        truth_state="working",
        reachability=REACHABLE,
        observed_model={"kind": "observed", "model": "glm-5.2[1m]"},
        harness="claude",
        route_settings_path=None,
        last_activity_age_s=469,
        provider_refusal=_QUOTA,
    )
    assert (prog.verdict, prog.basis) == (REFUSED, MODEL_REFUSED)


# ---------------------------------------------------------------------------
# The inference-sample marker (x-e594, measured 2026-09-12)
#
# A worker killed by a provider 429 dies WRITING that error, so its transcript
# is freshest at the instant it died. Age alone therefore reports the corpse as
# reachable: `fno backlog requeue` refused one at 13 minutes quiet and accepted
# the same dead worker at 23. `observed_model` is the marker that cannot be
# faked, because only a vendor answering a turn writes the record it counts.
# ---------------------------------------------------------------------------

_CORPSE = {"kind": "no-model-yet"}


def test_a_fresh_transcript_with_no_inference_stops_certifying_liveness() -> None:
    """The measured corpse: state working, 13 minutes, zero vendor turns."""
    got = classify_reachability(
        truth_state="working", age_s=13 * 60, falsifier=None, observed_model=_CORPSE
    )
    assert (got.verdict, got.basis) == (UNKNOWN, NO_INFERENCE)
    assert got.age_s == 13 * 60


@pytest.mark.parametrize(
    "observed",
    [
        None,
        {"kind": "not-file-backed"},
    ],
)
def test_an_absent_sample_reading_lowers_nothing(observed) -> None:
    """Every variant that is an ABSENCE leaves the verdict exactly as it was.
    `not-file-backed` is the load-bearing one: opencode keeps no per-session
    file, so counting it as zero would read every opencode worker as unknown."""
    got = classify_reachability(
        truth_state="working", age_s=13 * 60, falsifier=None, observed_model=observed
    )
    assert (got.verdict, got.basis) == (REACHABLE, TRANSCRIPT)


# ──: a claude row's falsifier yields to claude's session records ──

_SPECIMEN_SID = "bb2731c9-ad46-4303-a80d-152c68e91a4e"


def _specimen_row(**overrides):
    """A stopped claude thread row: pid cleared, exit recorded, session id
    carried."""
    from types import SimpleNamespace

    row = SimpleNamespace(
        pid=None,
        pid_start_time=None,
        mux=None,
        status="exited",
        harness="claude",
        harness_session_id=_SPECIMEN_SID,
    )
    for key, value in overrides.items():
        setattr(row, key, value)
    return row


def test_a_proven_holder_cancels_a_claude_row_falsifier(monkeypatch) -> None:
    """AC3-HP: the exit stamp yields to the resumed process's own record."""
    from fno.agents import reachability
    from fno.agents.reachability import classify_reachability, registry_falsifier

    monkeypatch.setattr(reachability, "_claude_holder_proven", lambda sid: True)
    assert registry_falsifier(_specimen_row()) is None

    # With the falsifier cancelled, a fresh working transcript reads
    # reachable on the transcript basis - the resumed worker is no longer
    # condemned mid-turn.
    verdict = classify_reachability(
        truth_state="working",
        age_s=16.0,
        falsifier=None,
        last_activity_basis="last-entry",
        observed_model={"kind": "observed", "model": "claude-opus-5-5"},
    )
    assert verdict.verdict == "reachable"
    assert verdict.basis == "transcript"


def test_every_other_holder_answer_keeps_the_falsifier(monkeypatch) -> None:
    """AC3-ERR: held false, held null, and a broken seam all keep the exit
    stamp. The seam can only cancel on positive proof."""
    from fno.agents import reachability
    from fno.agents.reachability import registry_falsifier

    for probe in (
        lambda sid: False,  # held false, and held null: the seam maps both to False
        lambda sid: (_ for _ in ()).throw(RuntimeError("binary gone")),
    ):
        monkeypatch.setattr(reachability, "_claude_holder_proven", probe)
        assert registry_falsifier(_specimen_row()) == "exit-recorded"
