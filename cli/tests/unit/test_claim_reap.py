"""Unit tests for claim GC (reap) and the list reader's honesty.

Two things had no test before this: nothing pruned a claim whose holder died
without releasing (so a leaked lockfile stayed forever), and `fno agents claim list`
rendered a store that was 99 percent stale as an empty store. Every test here
proves a real defect would have been caught. The leak-measurement case (a
subprocess killed without release) moved with the engine to the Rust parity
suite; what stays here proves the Python-side readers, verdict plumbing and
dispatch reservations.
"""
from __future__ import annotations

import socket
from pathlib import Path

import psutil

from fno.claims.cli import _abandonment_probe
from fno.claims.core import (
    _claim_verdict,
    acquire_claim,
    sweep_verdict,
)
from fno.claims.io import claim_path, serialize_claim
from fno.claims.types import Claim, now_ms
from fno.claims.verdict import claim_verdicts


def _dead_pid() -> int:
    dead = 999_999
    while psutil.pid_exists(dead):
        dead += 1
    return dead


def _native_sweep_verdict(claim: Claim) -> tuple[bool, str]:
    """Exercise the Rust decision door for a synthetic claim fixture."""
    from tempfile import TemporaryDirectory

    with TemporaryDirectory() as raw_root:
        root = Path(raw_root)
        path = claim_path(claim.key, root=root)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(serialize_claim(claim), encoding="utf-8")
        row = claim_verdicts([claim.key], root=root).get(claim.key)
        assert row is not None, f"native door omitted {claim.key}"
        return bool(row["provably_dead"]), str(row["bucket"] or "")


def classify_for_sweep(claim: Claim, _now: int | None = None) -> tuple[bool, str]:
    return _native_sweep_verdict(claim)


def _probe_sweep(claim: Claim, root):
    """Judge one claim the way the pre-forward `reap` sweep did.

    The verb now forwards to Rust, whose own roster arms answer fail-closed,
    so the probe wiring is exercised directly: the same `sweep_verdict` call
    the old CLI loop made, with a root-resolved native verdict row and a
    fresh lazily-reading abandonment probe. Node settlement stays unwired -
    these fixtures pin abandonment-probe arms, and a settlement probe would
    consume a flaky roster fake's calls. Returns ((dead, bucket), probe);
    the probe's `.reasons` dict carries one token per unanswered arm.
    """
    probe = _abandonment_probe()
    dead, bucket = sweep_verdict(
        claim, abandonment_probe=probe,
        native_verdict=_claim_verdict(claim, root=root),
    )
    return (dead, bucket), probe


# ---------------------------------------------------------------------------
# a dead one-shot holder blocks nothing (x-05be change 3)
# ---------------------------------------------------------------------------


class TestTheSweepNeverReapsAnUnexpiredDispatchReservation:
    """A dead-pid `dispatch:` claim inside its TTL LOOKS like a pure wedge, and
    the sweep must still keep it.

    That TTL is the boot window. It outlives its spawner on purpose, so a second
    dispatcher does not launch onto a node whose worker has not yet reached `fno
    target init`. A background sweep reaping it collapses the dedup window, and
    it also voids the `dispatch:think:<node>:<reason>` tokens, which have no
    node claim behind them at all. The wedge is cleared at the spawn guard
    instead, where the caller is the next dispatcher rather than a sweep.
    """

    def _suspect(self, key):
        return Claim(
            key=key, holder="spawn-cli:1", acquired_at=now_ms(),
            expires_at=now_ms() + 180_000, pid=_dead_pid(),
            host=socket.gethostname(),
        )

    def test_a_dead_dispatch_reservation_is_kept_inside_its_ttl(self):
        provably_dead, bucket = classify_for_sweep(self._suspect("dispatch:x-05be"))
        assert (provably_dead, bucket) == (False, "suspect")


# ---------------------------------------------------------------------------
# the abandonment probe: reap only on a positive finding
# ---------------------------------------------------------------------------


class TestCliProbeWiring:
    """The roster join is injected by sweep_verdict's callers: the spawn
    guard in fno.agents.cli passes abandonment_probe for every suspect
    claim, and the pre-forward `reap` sweep was the other caller. The verb
    itself now forwards to Rust, whose own roster arms answer fail-closed,
    so these tests drive the Python wiring directly - a probe that exists
    but is never passed is a decorative guard."""

    def _fake_roster(self, monkeypatch, rows, warnings=()):
        def _fake(*_a, **_kw):
            return list(rows), list(warnings)

        monkeypatch.setattr("fno.agents.watchdog.fleet_rows", _fake)


    def _transcript_says(self, monkeypatch, finished):
        monkeypatch.setattr(
            "fno.claims.cli._transcript_says_finished", lambda *_a, **_kw: finished
        )

    def test_the_holder_found_and_finished_reaps(self, tmp_path, monkeypatch):
        """Abandonment is proven by FINDING the holder and seeing it stopped."""
        from fno.agents.watchdog import Row

        claim = acquire_claim(
            key="node:x-abandoned", holder="target-session:sid-gone", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        self._fake_roster(
            monkeypatch,
            rows=[Row(row_id="sid-gone", name="t-gone", state="done", node="x-abandoned", cwd="")],
        )
        self._transcript_says(monkeypatch, True)
        (dead, bucket), _probe = _probe_sweep(claim, tmp_path)
        assert (dead, bucket) == (True, "")

    def test_a_terminal_row_whose_transcript_is_alive_is_never_reaped(
        self, tmp_path, monkeypatch
    ):
        """The row state alone cannot authorize a reap. `_TERMINAL_STATES`
        carries its own warning that the roster called a WORKING session done
        on 2026-08-15, and archiving on it hands a live worker's node to the
        next dispatcher - the `reaped_a_live_worker` kill criterion."""
        from fno.agents.watchdog import Row

        claim = acquire_claim(
            key="node:x-lying", holder="target-session:sid-busy", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        self._fake_roster(
            monkeypatch,
            rows=[Row(row_id="sid-busy", name="t-busy", state="done", node="x-lying", cwd="")],
        )
        self._transcript_says(monkeypatch, False)
        (dead, bucket), probe = _probe_sweep(claim, tmp_path)
        assert (dead, bucket) == (False, "suspect_alive")
        assert probe.reasons[claim.key] == "row-transcript-unfinished"


class TestExternalDeathEvidence:
    """The lead's two 2026-08-23 specimens, replayed. Both held a claim whose
    holder was dead to every outside instrument - process table, registry row,
    mux pane - yet the reaper answered "I cannot tell" (suspect) and a manual
    --force release was the only way through. The fix feeds the reaper those
    instruments as positive findings; a claim that still cannot be proven
    dead keeps, exactly as before."""

    def _fake_roster(self, monkeypatch, rows, warnings=()):
        def _fake(*_a, **_kw):
            return list(rows), list(warnings)

        monkeypatch.setattr("fno.agents.watchdog.fleet_rows", _fake)

    def _handover(self, tmp_path, *, pid=None, key="node:x-c272"):
        return acquire_claim(
            key=key,
            holder="spawn-handover:bp-xc272-daemondrift",
            ttl_ms=900_000,  # inside the launch window: the old probe's blind spot
            pid=pid if pid is not None else _dead_pid(),
            root=tmp_path,
        )

    def _pane(self, monkeypatch, absent):
        monkeypatch.setattr(
            "fno.claims.cli._mux_pane_absent_for",
            lambda worker, node_id="", runner=None: absent,
        )

    def test_specimen_2_absent_pane_and_dead_pid_reaps(self, tmp_path, monkeypatch):
        """Specimen two: the lead killed the holder's pane and removed its
        registry row; the spawn-handover claim survived both and the next
        dispatch refused. Pane positively absent from the mux listing AND the
        recorded spawner pid dead is the launch window OVER - a positive
        finding."""
        claim = self._handover(tmp_path)
        self._pane(monkeypatch, absent=True)
        (dead, bucket), _probe = _probe_sweep(claim, tmp_path)
        assert (dead, bucket) == (True, "")

    def test_handover_whose_pane_is_still_live_stays(self, tmp_path, monkeypatch):
        """The negative that keeps the fix honest: a pane still hosting the
        worker is the launch window OPEN, and the claim keeps."""
        claim = self._handover(tmp_path)
        self._pane(monkeypatch, absent=False)
        (dead, bucket), probe = _probe_sweep(claim, tmp_path)
        assert (dead, bucket) == (False, "suspect_unprobed")
        assert probe.reasons[claim.key] == "handover-pane-not-absent"


    def test_specimen_1_degraded_then_recovered_roster_reaps(
        self, tmp_path, monkeypatch
    ):
        """Specimen one: the lead killed the holder's whole tree and verified
        five pids gone; `claim reap` still reported the roster unconsulted
        because one degraded read answered for the whole pass. One retry on a
        degraded reading resolves it here."""
        from fno.agents.watchdog import Row

        calls = {"n": 0}

        def _flaky(*_a, **_kw):
            calls["n"] += 1
            if calls["n"] == 1:
                return [], ["claude not on PATH"]
            return (
                [
                    Row(
                        row_id="sid-3f84", name="t-3f84", state="done",
                        node="x-3f84", cwd="",
                    )
                ],
                [],
            )

        monkeypatch.setattr("fno.agents.watchdog.fleet_rows", _flaky)
        monkeypatch.setattr(
            "fno.claims.cli._transcript_says_finished", lambda *_a, **_kw: True
        )
        claim = acquire_claim(
            key="node:x-3f84", holder="target-session:sid-3f84",
            ttl_ms=3_600_000, pid=_dead_pid(), root=tmp_path,
        )
        (dead, bucket), _probe = _probe_sweep(claim, tmp_path)
        assert (dead, bucket) == (True, "")
        assert calls["n"] == 2  # degraded once, retried once, settled

    def _claimed_no_row(self, tmp_path, *, metadata=None):
        return acquire_claim(
            key="node:x-no-row", holder="target-session:sid-no-row",
            ttl_ms=3_600_000, pid=_dead_pid(), metadata=metadata, root=tmp_path,
        )

    def _roster_without_holder(self, monkeypatch):
        from fno.agents.watchdog import Row

        self._fake_roster(
            monkeypatch,
            rows=[
                Row(row_id=f"other-{i}", name=f"t-{i}", state="working",
                    node=f"x-{i}", cwd="")
                for i in range(3)
            ],
        )

    def test_dead_pid_with_worktree_metadata_and_finished_transcript_reaps(
        self, tmp_path, monkeypatch
    ):
        """The transcript fallback: the roster ran and has no row for the
        holder (the codex/hand-started coverage gap), the recorded pid is
        dead, and the claim itself carries the worktree the session's tree
        lives under. A finished tree is abandonment PROVEN, never inferred
        from the absent row."""
        self._roster_without_holder(monkeypatch)
        claim = self._claimed_no_row(tmp_path, metadata={"worktree": "/tmp/wt-x"})
        monkeypatch.setattr(
            "fno.claims.cli._transcript_says_finished", lambda *_a, **_kw: True
        )
        (dead, bucket), _probe = _probe_sweep(claim, tmp_path)
        assert (dead, bucket) == (True, "")


class TestMuxPaneAbsenceHelper:
    """_mux_pane_absent_for's own parsing rules, with a fake runner."""

    class _Proc:
        def __init__(self, rc, out):
            self.returncode = rc
            self.stdout = out

    def _runner(self, replies):
        calls = {"n": 0}

        def _run(argv, **_kw):
            idx = calls["n"]
            calls["n"] += 1
            return replies[idx]

        return _run


    def test_match_by_worktree_cwd_basename_means_present(self):
        """The NORMAL live-launch marker: the pane's fno_id is the session
        UUID (not the worker name) and the title is whatever the shell set,
        but dispatch names the worker's worktree after the worker, so the
        pane's cwd basename is the reliable join. A miss here is the
        reaped-a-live-worker disaster."""
        from fno.claims.cli import _mux_pane_absent_for

        runner = self._runner(
            [
                self._Proc(0, '[{"session":"main","state":"live","panes":1}]'),
                self._Proc(
                    0,
                    '[{"pane_id":4,"fno_id":"01a0-fresh-uuid","title":null,'
                    '"cwd":"/Users/x/.fno/worktrees/footnote/bp-x-worker"}]',
                ),
            ]
        )
        assert _mux_pane_absent_for("bp-x-worker", runner=runner) is False


    def test_nonempty_listing_without_the_worker_is_positive_absence(self):
        from fno.claims.cli import _mux_pane_absent_for

        runner = self._runner(
            [
                self._Proc(0, '[{"session":"main","state":"live","panes":1}]'),
                self._Proc(
                    0,
                    '[{"pane_id":2,"fno_id":"someone-else","title":null,'
                    '"cwd":"/Users/x/code/other"}]',
                ),
            ]
        )
        assert _mux_pane_absent_for("bp-x-worker", runner=runner) is True

    def test_empty_listing_is_unknown_not_absent(self):
        """`pane ls` prints [] both for no panes and for an unreachable
        session socket, so an empty listing proves nothing about absence."""
        from fno.claims.cli import _mux_pane_absent_for

        runner = self._runner(
            [
                self._Proc(0, '[{"session":"main","state":"live","panes":2}]'),
                self._Proc(0, "[]"),
            ]
        )
        assert _mux_pane_absent_for("bp-x-worker", runner=runner) is None


class TestRosterWiringOverTheTable:
    _fake_roster = TestCliProbeWiring._fake_roster


    def test_a_holder_absent_from_the_roster_is_never_reaped(self, tmp_path, monkeypatch):
        """fleet_rows cannot represent a codex or hand-started holder, so its
        absence there is never read as abandonment."""
        from fno.agents.watchdog import Row

        claim = acquire_claim(
            key="node:x-codex", holder="target-session:sid-codex", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        self._fake_roster(
            monkeypatch,
            rows=[
                Row(row_id=f"other-{i}", name=f"t-{i}", state="working", node=f"x-{i}", cwd="")
                for i in range(40)
            ],
        )
        (dead, bucket), probe = _probe_sweep(claim, tmp_path)
        assert (dead, bucket) == (False, "suspect_unprobed")
        assert probe.reasons[claim.key] == "row-absent-no-cwd"

    def test_a_still_degraded_roster_after_retry_keeps(self, tmp_path, monkeypatch):
        calls = {"n": 0}

        def _always_degraded(*_a, **_kw):
            calls["n"] += 1
            return [], ["claude not on PATH"]

        monkeypatch.setattr("fno.agents.watchdog.fleet_rows", _always_degraded)
        claim = acquire_claim(
            key="node:x-dead-roster", holder="target-session:sid-1",
            ttl_ms=3_600_000, pid=_dead_pid(), root=tmp_path,
        )
        (dead, bucket), probe = _probe_sweep(claim, tmp_path)
        assert (dead, bucket) == (False, "suspect_unprobed")
        assert probe.reasons[claim.key] == "roster-read-degraded"
        assert calls["n"] == 2
