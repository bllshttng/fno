"""Unit tests for fno.recovery — Layer-2 session auto-recovery watchdog (x-f47c).

The load-bearing part is ``classify`` (the idle-but-incomplete predicate); the
rest of the suite covers the registry∩live-bg-session join, the per-session
nudge cap, and the one-event-per-decision contract. Every I/O dependency in
``recovery_sweep`` is injectable so these run without a live claude / filesystem.
"""
from __future__ import annotations

from datetime import datetime, timedelta, timezone

import pytest

def _naming_real(cmd, **kw):
    """The name commands execute in the fno-agents binary: a blanket stub would
    answer the mint with a spawn-shaped SimpleNamespace lacking .stdout. Ride
    the real binary for them; _REAL_RUN is captured at module import, before
    any test patches the subprocess module."""
    parts = [str(part) for part in cmd]
    if "name-mint" in parts or "name-codes" in parts or "name-parse" in parts:
        return _REAL_RUN(cmd, **kw)
    return None

_REAL_RUN = __import__("subprocess").run


from fno import recovery  # noqa: E402 - after capturing subprocess.run


def _now() -> datetime:
    return datetime(2026, 6, 29, 20, 0, 0, tzinfo=timezone.utc)


def _iso(dt: datetime) -> str:
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


# ---------------------------------------------------------------------------
# classify — the predicate (AC1 nudge, AC2 needs-input, AC3 done)
# ---------------------------------------------------------------------------

class TestClassify:
    def test_needs_input_never_nudged_even_when_stale(self):
        # AC2-EDGE: a needs-input session is waiting on a human, not stalled.
        old = _iso(_now() - timedelta(hours=1))
        assert recovery.classify(
            "needs-input", old, _now(), 300,
            truth_state="your-move", truth_age_s=3600,
        ) == recovery.SKIP_NEEDS_INPUT

    @pytest.mark.parametrize("state", ["completed"])
    def test_terminal_states_skipped(self, state):
        # AC3-EDGE: a clean terminal is never re-nudged.
        old = _iso(_now() - timedelta(hours=1))
        assert recovery.classify(
            state, old, _now(), 300, truth_state="done", truth_age_s=3600,
        ) == recovery.SKIP_TERMINAL

    def test_running_and_stale_nudges(self):
        # AC1-HP: idle past the threshold, work incomplete -> nudge.
        stale = _iso(_now() - timedelta(seconds=600))
        assert recovery.classify(
            "running", stale, _now(), 300, truth_state="working", truth_age_s=600,
        ) == recovery.NUDGE

    def test_missing_updated_at_is_conservative(self):
        # Can't prove idleness -> do not nudge.
        assert recovery.classify(
            "running", None, _now(), 300, truth_state="unknown", truth_age_s=None,
        ) == recovery.NOT_STALE

    def test_family1_stalled_drives_recovery_even_when_state_json_is_fresh(self):
        assert (
            recovery.classify(
                "running", _iso(_now()), _now(), 300,
                truth_state="stalled", truth_age_s=600,
            )
            == recovery.NUDGE
        )


# ---------------------------------------------------------------------------
# candidate join — registry provenance ∩ live bg sessions (the AC invariant:
# only ever touch sessions footnote launched)
# ---------------------------------------------------------------------------

class _Entry:
    def __init__(self, harness, short_id, cwd=None):
        self.harness = harness
        self.short_id = short_id
        self.cwd = cwd


class _Locator:
    def __init__(self, short_id, sock, jobs_dir):
        self.short_id = short_id
        self.messaging_socket_path = sock
        self.jobs_dir = jobs_dir


class TestCandidateJoin:
    def test_only_footnote_claude_entries_with_live_sessions(self, tmp_path):
        entries = [
            _Entry("claude", "aaaa1111"),   # live bg session -> candidate
            _Entry("claude", "bbbb2222"),   # no live session -> dropped
            _Entry("codex", "cccc3333"),    # not claude -> dropped
            _Entry("claude", None),         # no short_id -> dropped
        ]
        live = {"aaaa1111": _Locator("aaaa1111", "/tmp/a.sock", tmp_path)}
        cands = recovery.iter_candidates(entries, locate_fn=lambda sid: live.get(sid))
        assert [c.short_id for c in cands] == ["aaaa1111"]

    def test_launch_account_is_carried_from_the_registry_row(self, tmp_path):
        # x-bbc0: the quota lock is written against the account the worker
        # was LAUNCHED on, so the join carries it off the registry row.
        entry = _Entry("claude", "dddd4444")
        entry.launch_account = "readyrule"
        live = {"dddd4444": _Locator("dddd4444", "/tmp/d.sock", tmp_path)}
        (cand,) = recovery.iter_candidates([entry], locate_fn=lambda sid: live.get(sid))
        assert cand.launch_account == "readyrule"


# ---------------------------------------------------------------------------
# recovery_sweep — nudge, cap (AC4), one-event-per-decision (invariant)
# ---------------------------------------------------------------------------

class _Cfg:
    enabled = True
    idle_threshold_seconds = 300
    max_nudges = 3


def _node_bound_cwd(tmp_path):
    """A worktree-like cwd with a ``.fno/target-state.md`` so the candidate reads
    as a node-bound /target worker (not node-less)."""
    cwd = tmp_path / "wt"
    (cwd / ".fno").mkdir(parents=True, exist_ok=True)
    (cwd / ".fno" / "target-state.md").write_text(
        "graph_node_id: x-test\n", encoding="utf-8"
    )
    return str(cwd)


def _stale_candidate(tmp_path, short_id="aaaa1111", sock="/tmp/a.sock", *, node_less=False):
    return recovery.Candidate(
        short_id=short_id,
        sock_path=sock,
        jobs_dir=tmp_path,
        cwd=None if node_less else _node_bound_cwd(tmp_path),
    )


class _Harness:
    """Collects emitted events and socket sends for assertions."""

    def __init__(self, state="running", updated_age_s=600, sock_live=True):
        self.events: list[tuple[str, dict]] = []
        self.sends: list[tuple[str, str]] = []
        self._state = state
        self._updated = _iso(_now() - timedelta(seconds=updated_age_s))
        self._sock_live = sock_live

    def emit(self, etype, data):
        self.events.append((etype, data))

    def read_state(self, jobs_dir):
        return recovery._SnapshotView(self._state, self._updated)

    def liveness(self, sock):
        return self._sock_live

    def truth(self, _candidate):
        if self._state == "needs-input":
            return {"state": "your-move", "last_activity_age_s": 600}
        if self._state in {"done", "completed", "failed"}:
            return {"state": "done", "last_activity_age_s": 600}
        age = int((_now() - datetime.fromisoformat(self._updated.replace("Z", "+00:00"))).total_seconds())
        return {"state": "stalled" if age >= 300 else "working", "last_activity_age_s": age}

    def send(self, sock, content, from_name):
        self.sends.append((sock, content))

    def event_types(self):
        return [e[0] for e in self.events]


class TestSweep:
    def test_stale_node_bound_worker_surfaced_as_held_no_send(self, tmp_path):
        # x-d93d: a stuck node-bound /target worker is bypass, so the socket
        # nudge is held by Claude Code's crossSessionInbound guard. recovery does
        # NOT send (that would stack an operator dialog); it surfaces the stuck
        # worker once as held-by-design.
        h = _Harness()
        counts: dict = {}
        recovery.recovery_sweep(
            _now(), _Cfg(),
            candidates=[_stale_candidate(tmp_path)],
            counts=counts,
            emit=h.emit, read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
        )
        assert h.sends == []
        assert h.event_types() == ["recovery_skipped"]
        assert h.events[0][1]["reason"] == "held-by-design"
        assert counts["aaaa1111"] == 1

    def test_node_less_bg_thread_is_silent_not_surfaced(self, tmp_path):
        # x-a76d scope fix: a node-less bg thread (an ask/relay worker, or a
        # ``--bg`` session a human attached to and drives) is none of recovery's
        # mission. A stuck one stays silent, never a candidate for held-by-design
        # surfacing or a close nudge.
        h = _Harness()
        recovery.recovery_sweep(
            _now(), _Cfg(),
            candidates=[_stale_candidate(tmp_path, node_less=True)],
            counts={},
            emit=h.emit, read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
        )
        assert h.sends == []
        assert h.events == []

    def test_cap_reached_emits_capped_once_no_send(self, tmp_path):
        # AC4-INV: at the cap, no further nudges; recovery_capped emitted.
        h = _Harness()
        counts = {"aaaa1111": 3}
        recovery.recovery_sweep(
            _now(), _Cfg(),
            candidates=[_stale_candidate(tmp_path)],
            counts=counts,
            emit=h.emit, read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
        )
        assert h.sends == []
        assert h.event_types() == ["recovery_capped"]
        assert counts["aaaa1111"] == 3  # not incremented past cap


# ---------------------------------------------------------------------------
# run_recovery_sweep — the high-level entry (registry join -> sweep -> persist)
# ---------------------------------------------------------------------------


class TestRunRecoverySweep:
    def test_end_to_end_held_surface_and_counts_persisted(self, tmp_path):
        h = _Harness()  # stale running session, live socket, no promise
        entries = [_Entry("claude", "aaaa1111", cwd=_node_bound_cwd(tmp_path)), _Entry("codex", "z")]
        live = {"aaaa1111": _Locator("aaaa1111", "/tmp/a.sock", tmp_path)}
        saved: dict = {}

        n = recovery.run_recovery_sweep(
            _Cfg(),
            emit=h.emit,
            now=_now(),
            registry_load=lambda: entries,
            locate_fn=lambda sid: live.get(sid),
            read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
            load_counts_fn=lambda: {},
            save_counts_fn=lambda c: saved.update(c),
        )

        assert n == 1
        assert h.event_types() == ["recovery_skipped"]
        assert h.events[0][1]["reason"] == "held-by-design"
        assert saved["aaaa1111"] == 1

    def test_prunes_counts_for_vanished_sessions(self, tmp_path):
        h = _Harness()
        entries = [_Entry("claude", "aaaa1111", cwd=_node_bound_cwd(tmp_path))]
        live = {"aaaa1111": _Locator("aaaa1111", "/tmp/a.sock", tmp_path)}
        saved: dict = {}
        # "gone9999" is a leftover count for a session no longer live.
        prior = {"aaaa1111": 0, "gone9999": 2, "capped:gone9999": True}

        recovery.run_recovery_sweep(
            _Cfg(),
            emit=h.emit,
            now=_now(),
            registry_load=lambda: entries,
            locate_fn=lambda sid: live.get(sid),
            read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
            load_counts_fn=lambda: dict(prior),
            save_counts_fn=lambda c: saved.update(c),
        )

        assert "gone9999" not in saved
        assert "capped:gone9999" not in saved
        assert saved["aaaa1111"] == 1


class TestClassifyWorkerRefusal:
    """AC1: a LIVE worker's refusal is readable from the transcript turn."""

    def test_transcript_refusal_is_found_and_sourced(self):
        # AC1-HP: the capped worker never died, so output_result is absent and
        # only its last turn carries the refusal.
        got = recovery.classify_worker_refusal(
            None, "Claude usage limit reached. Resets 2026-08-18 07:19:38"
        )
        assert got is not None
        err, source = got
        assert err.triggers_swap is True
        assert source == "transcript"

    def test_non_swap_class_output_falls_through_to_transcript(self):
        # A connection drop is not swap-class, so it must not shadow a real
        # refusal sitting in the transcript.
        got = recovery.classify_worker_refusal(
            "API Error: Connection closed mid-response", "usage limit reached"
        )
        assert got is not None
        assert got[1] == "transcript"

    def test_only_the_leading_clause_of_a_live_turn_is_read(self):
        # A worker whose turn buries a marker past the lead is not refused.
        # This narrows the false positives; the sweep's corroboration gate
        # closes the rest.
        buried = "x" * 200 + " usage limit reached"
        assert recovery.classify_worker_refusal(None, buried) is None


class _RefusalHarness(_Harness):
    """A sweep harness whose worker is LIVE, fresh, and carrying a refusal.

    This is the population the whole plan exists for: state=running, transcript
    mtime seconds old, and the last turn is the provider saying no. Every gate
    in the sweep reads it as healthy.
    """

    def __init__(self, last_message, **kw):
        super().__init__(updated_age_s=5, **kw)
        self._last_message = last_message

    def truth(self, _candidate):
        return {
            "state": "working",
            "last_activity_age_s": 5,
            "last_message": self._last_message,
            "observed_model": {"kind": "observed", "model": "glm-5.2", "samples": 3},
        }


class TestRefusalHoistedAboveTheStalenessGate:
    """AC1: a refusal is affirmative evidence and does not wait for a timer."""

    _QUOTA = (
        "Claude usage limit reached. Your limit will reset at "
        "2026-08-18T07:19:38+08:00"
    )

    def _sweep(self, h, tmp_path, counts):
        recovery.recovery_sweep(
            _now(), _Cfg(),
            candidates=[_stale_candidate(tmp_path)],
            counts=counts,
            emit=h.emit, read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
        )

    def test_ac1_hp_fires_on_the_first_tick_while_the_worker_reads_fresh(
        self, tmp_path
    ):
        # The worker is 5s old against a 300s idle threshold, so classify()
        # returns NOT_STALE and the old code skipped it entirely. The event must
        # fire anyway, on this tick.
        h = _RefusalHarness(self._QUOTA)
        self._sweep(h, tmp_path, {})
        assert "worker_refused" in h.event_types()
        payload = dict(h.events[h.event_types().index("worker_refused")][1])
        assert payload["short_id"] == "aaaa1111"
        assert payload["node"] == "x-test"
        assert payload["error_class"] == "provider_4xx_quota"
        assert payload["source"] == "transcript"
        assert payload["model"] == "glm-5.2"
        assert payload["resets_at"] is not None

    def test_ac1_edge_a_second_tick_with_no_new_turn_stays_silent(self, tmp_path):
        counts: dict = {}
        h = _RefusalHarness(self._QUOTA)
        self._sweep(h, tmp_path, counts)
        h2 = _RefusalHarness(self._QUOTA)
        self._sweep(h2, tmp_path, counts)
        assert "worker_refused" not in h2.event_types()


class _StaleRefusalHarness(_Harness):
    """A worker that is BOTH stale and carrying a refusal in its last turn.

    The sweep records the quota refusal and surfaces the strand; the move
    itself belongs to the provider-cap actor (x-7e05).
    """

    def __init__(self, last_message, **kw):
        super().__init__(**kw)
        self._last_message = last_message

    def truth(self, _candidate):
        return {
            "state": "stalled",
            "last_activity_age_s": 900,
            "last_message": self._last_message,
            "observed_model": {"kind": "observed", "model": "glm-5.2"},
        }


class TestTranscriptRefusalNeedsCorroboration:
    """A live worker's prose about a cap reads the same as a cap.

    The event costs nothing when it is wrong. The action costs a stopped worker
    and a re-dispatched node, so it waits for one more tick with the same turn.
    """

    _QUOTA = "Claude usage limit reached. Resets 2026-08-18T07:19:38+08:00"

    def _sweep(self, h, tmp_path, counts):
        recovery.recovery_sweep(
            _now(), _Cfg(),
            candidates=[_stale_candidate(tmp_path)],
            counts=counts,
            emit=h.emit, read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
        )

    def test_the_first_tick_reports_but_does_not_act(self, tmp_path):
        counts: dict = {}
        h = _StaleRefusalHarness(self._QUOTA)
        self._sweep(h, tmp_path, counts)
        assert "worker_refused" in h.event_types()
        assert "provider_quota_locked" not in h.event_types(), (
            "a first sighting must not record the lock"
        )

    def test_the_same_turn_a_tick_later_acts(self, tmp_path):
        counts: dict = {}
        self._sweep(_StaleRefusalHarness(self._QUOTA), tmp_path, counts)
        h2 = _StaleRefusalHarness(self._QUOTA)
        self._sweep(h2, tmp_path, counts)
        assert "provider_quota_locked" in h2.event_types()

    def test_a_worker_that_moved_on_never_acts(self, tmp_path):
        # The independent signal: prose about a cap is followed by a different
        # turn, and the refusal simply stops classifying.
        counts: dict = {}
        self._sweep(
            _StaleRefusalHarness("Added a test for the rate limit path."),
            tmp_path, counts,
        )
        h2 = _StaleRefusalHarness("Pushed the branch and opened the PR.")
        self._sweep(h2, tmp_path, counts)
        assert "provider_quota_locked" not in h2.event_types()

    def test_a_dead_sessions_own_error_still_acts_at_once(self, tmp_path):
        # The session's own error text (not prose about someone else's cap)
        # records the refusal at once instead of waiting out a tick.
        h = _StaleRefusalHarness("rate limit exceeded")
        recovery.recovery_sweep(
            _now(), _Cfg(),
            candidates=[_stale_candidate(tmp_path)],
            counts={},
            emit=h.emit, read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
        )
        assert "worker_refused" in h.event_types()



class TestMaterializeManagedSwitch:
    """US3: the managed-account materialize gate (config.providers.auto_switch)."""

    def _fake_config(self, auto_switch, record):
        class _Cfg:
            pass
        c = _Cfg()
        c.auto_switch = auto_switch
        c.by_id = {record.id: record} if record is not None else {}
        return c

    def _managed_record(self):
        from fno.adapters.providers.model import ProviderRecord
        return ProviderRecord(id="claude-secondary", name="B", harness="claude", auth="managed")

    def test_auto_switch_off_returns_false_without_touching_slot(self, monkeypatch):
        from fno import rust_binary
        from fno.adapters.providers import loader as loader_mod

        rec = self._managed_record()
        monkeypatch.setattr(loader_mod, "load_providers",
                            lambda *a, **k: self._fake_config(False, rec))
        switched = {"called": False}
        monkeypatch.setattr(rust_binary, "command_call",
                            lambda *a, **k: switched.__setitem__("called", True))
        assert recovery._materialize_managed_switch("claude-secondary") is False
        assert switched["called"] is False   # disarmed: slot never mutated

    def test_auto_switch_on_materializes(self, monkeypatch):
        from fno import rust_binary
        from fno.adapters.providers import loader as loader_mod

        rec = self._managed_record()
        monkeypatch.setattr(loader_mod, "load_providers",
                            lambda *a, **k: self._fake_config(True, rec))
        seen: dict = {}

        def _switch(command, payload, *a, **k):
            seen.update(command=command, payload=payload)
            return {"active": payload["id"], "slot_changed": True}
        monkeypatch.setattr(rust_binary, "command_call", _switch)
        assert recovery._materialize_managed_switch("claude-secondary", "/repo") is True
        # The switch op defers under live pins and writes no routing pointer.
        assert seen["command"] == ["spawn-overlay", "accounts", "switch"]
        assert seen["payload"] == {"id": "claude-secondary", "repo_root": "/repo"}

    def test_switch_deferred_returns_false(self, monkeypatch):
        from fno import rust_binary
        from fno.adapters.providers import loader as loader_mod

        rec = self._managed_record()
        monkeypatch.setattr(loader_mod, "load_providers",
                            lambda *a, **k: self._fake_config(True, rec))

        def _defer(*a, **k):
            raise rust_binary.CommandUnavailable(
                "fno-agents spawn-overlay accounts switch exited 2: switch deferred: slot pinned by pid 42"
            )
        monkeypatch.setattr(rust_binary, "command_call", _defer)
        assert recovery._materialize_managed_switch("claude-secondary") is False


class TestNodeIdFromWorktree:

    @pytest.mark.parametrize("raw", ["null", '""'])
    def test_sentinel_values_are_unresolved(self, tmp_path, raw):
        fno_dir = tmp_path / ".fno"
        fno_dir.mkdir()
        (fno_dir / "target-state.md").write_text(
            f"graph_node_id: {raw}\n", encoding="utf-8"
        )
        assert recovery._node_id_from_worktree(str(tmp_path)) is None


class TestNodeIsDone:
    """x-370f AC1-EDGE: the already-done guard reads node status, fail-open."""

    def _patch_graph(self, monkeypatch, entries):
        from fno.graph import store as gl
        monkeypatch.setattr(gl, "read_nodes_by_ids", lambda *a, **k: {"entries": entries, "missing": []})
        monkeypatch.setattr(gl, "read_graph_strict", lambda *a, **k: entries)

    def test_true_when_done(self, monkeypatch):
        self._patch_graph(monkeypatch, [{"id": "x-370f", "status": "done"}])
        assert recovery._node_is_done("x-370f") is True

    def test_load_error_degrades_to_false(self, monkeypatch):
        from fno.graph import store as gl

        def boom(*a, **k):
            raise RuntimeError("corrupt graph")

        monkeypatch.setattr(gl, "read_nodes_by_ids", lambda *a, **k: None)
        monkeypatch.setattr(gl, "read_graph_strict", boom)
        assert recovery._node_is_done("x-370f") is False


class TestMissionAwareTerminalGate:
    """x-5583: a hollow ``<promise>`` no longer suppresses recovery forever."""

    def _classify(self, mission_complete, age_s=3600):
        return recovery.classify(
            "running", None, _now(), 300,
            truth_state="done", truth_age_s=age_s,
            mission_complete=mission_complete,
        )

    def test_incomplete_and_stale_nudges(self):
        # AC1: positive evidence of an unfinished mission relaxes the skip.
        assert self._classify(False) == recovery.NUDGE

    @pytest.mark.parametrize("mc", [None])
    def test_unverifiable_stays_terminal(self, mc):
        # AC7: None (probe failed, node-less thread) keeps the terminal skip
        # (fail closed).
        assert self._classify(mc) == recovery.SKIP_TERMINAL

    def test_complete_and_stale_nudges_close(self):
        # x-a76d: a session whose mission actually shipped but still lingers is
        # surfaced to CLOSE once idle, not left silent or resumed.
        assert self._classify(True) == recovery.NUDGE_CLOSE

    def test_close_notify_calls_seam_once_across_ticks(self, tmp_path):
        # The close surface fires ONCE per session (a finished session the
        # operator has not gotten to must not ping every tick).
        notified: list = []
        counts: dict = {}
        for _ in range(3):
            h = _Harness(state="done")
            recovery.recovery_sweep(
                _now(), _Cfg(),
                candidates=[_stale_candidate(tmp_path)], counts=counts,
                emit=h.emit, read_state_fn=h.read_state,
                truth_fn=h.truth, liveness_fn=h.liveness,
                mission_complete_fn=lambda c: True,
                notify_close_fn=lambda c: notified.append(c) or True,
            )
        assert len(notified) == 1
        assert counts[recovery._close_key("aaaa1111")] is True

    def test_close_notify_undelivered_does_not_claim_surface(self, tmp_path):
        # A channel that reports not-delivered (no osascript/notify-send, or a
        # raising seam) must not record a recovery_close_notify it never sent.
        def boom(_c):
            raise RuntimeError("notify down")

        h = _Harness(state="done")
        recovery.recovery_sweep(
            _now(), _Cfg(),
            candidates=[_stale_candidate(tmp_path)], counts={},
            emit=h.emit, read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
            mission_complete_fn=lambda c: True,
            notify_close_fn=boom,
        )
        assert h.event_types() == ["recovery_skipped"]
        assert h.events[0][1]["reason"] == "no-notify-channel"


class TestMissionComplete:
    """x-5583: the family-2 artifact probe behind the terminal-suppression gate."""

    def _patch_graph(self, monkeypatch, entries):
        from fno.graph import store as gl
        monkeypatch.setattr(gl, "read_nodes_by_ids", lambda *a, **k: {"entries": entries, "missing": []})
        monkeypatch.setattr(gl, "read_graph_strict", lambda *a, **k: entries)

    def _cand(self, name=None, cwd=None):
        return recovery.Candidate(short_id="s1", sock_path="/s", jobs_dir=None,
                                  cwd=cwd, name=name)

    def _worktree(self, tmp_path, node):
        fno_dir = tmp_path / ".fno"
        fno_dir.mkdir()
        (fno_dir / "target-state.md").write_text(
            f"graph_node_id: {node}\n", encoding="utf-8")
        return str(tmp_path)

    # -- resolution order -------------------------------------------------
    def test_manifest_wins_over_name(self, monkeypatch, tmp_path):
        # A non-standard name that WOULD parse to nothing still resolves via the
        # manifest, and a manifest hit is always a target mission.
        self._patch_graph(monkeypatch, [{"id": "x-1111", "plan_path": "/p.md"}])
        cand = self._cand(name="tgt-x-9999-liveness",
                          cwd=self._worktree(tmp_path, "x-1111"))
        # plan_path alone never completes a target mission (AC5).
        assert recovery.mission_complete(cand) is False

    def test_think_name_ignores_a_foreign_manifest(self, monkeypatch, tmp_path):
        # The think dispatch spawns with --cwd on the node's CANONICAL root, where
        # an unrelated /target session's manifest can sit; a think worker writes
        # none of its own. Reading that manifest would answer about x-1111 and
        # nudge a design pass that actually finished.
        self._patch_graph(monkeypatch, [
            {"id": "x-1111", "status": "ready"},           # foreign, incomplete
            {"id": "x-2222", "plan_path": "/plans/d.md"},  # ours, complete
        ])
        cand = self._cand(name="think-x-2222-bar",
                          cwd=self._worktree(tmp_path, "x-1111"))
        assert recovery.mission_complete(cand) is True

    def test_unresolvable_mission_is_none(self, monkeypatch, tmp_path):
        # AC7: no node id in the name and no manifest -> unverifiable.
        self._patch_graph(monkeypatch, [{"id": "x-1111", "status": "ready"}])
        assert recovery.mission_complete(
            self._cand(name="relay-worker", cwd=str(tmp_path))) is None
        assert recovery.mission_complete(self._cand()) is None

    # -- target missions --------------------------------------------------
    @pytest.mark.parametrize("entry,expected", [
        ({"id": "x-1111", "status": "done"}, True),
        ({"id": "x-1111", "status": "ready", "pr_number": 42}, True),
        # AC5: a blueprinted-but-unshipped target node is NOT complete.
        ({"id": "x-1111", "status": "ready", "plan_path": "/p.md"}, False),
        ({"id": "x-1111", "status": "done", "archived_at": "2026-09-01"}, None),
    ])
    def test_target_artifacts(self, monkeypatch, entry, expected):
        self._patch_graph(monkeypatch, [entry])
        assert recovery.mission_complete(
            self._cand(name="target-x-1111-foo")) is expected

    # -- think missions ---------------------------------------------------
    @pytest.mark.parametrize("entry,expected", [
        ({"id": "x-2222", "plan_path": "/plans/d.md"}, True),
        ({"id": "x-2222", "plan_path": "   "}, False),  # whitespace is no artifact
    ])
    def test_think_artifacts(self, monkeypatch, entry, expected):
        self._patch_graph(monkeypatch, [entry])
        assert recovery.mission_complete(
            self._cand(name="think-x-2222-bar")) is expected

    # -- non-birth think passes -------------------------------------------
    @pytest.mark.parametrize("name", [
        "think-x-2222-retro-bar",          # dispatched only AFTER status done
    ])
    @pytest.mark.parametrize("entry", [
        {"id": "x-2222", "status": "done"},
        {"id": "x-2222", "plan_path": "/plans/d.md"},
    ])
    def test_non_birth_think_cannot_inherit_the_nodes_artifacts(
        self, monkeypatch, name, entry,
    ):
        # These artifacts predate the worker, so reading them as completion
        # would re-open the exact suppression this change closes. Unverifiable.
        self._patch_graph(monkeypatch, [entry])
        assert recovery.mission_complete(self._cand(name=name)) is None

    def test_birth_pass_whose_slug_shadows_a_reason_fails_closed(self, monkeypatch):
        # `think-x-2222-retrospective-ui` is a BIRTH pass whose slug merely starts
        # with a reason word. Reading it as lifecycle costs a suppression we would
        # have had anyway; the other direction would be a false completion.
        self._patch_graph(monkeypatch, [{"id": "x-2222", "plan_path": "/p.md"}])
        assert recovery.mission_complete(
            self._cand(name="think-x-2222-retro-spective-ui")) is None


class TestRedispatch:
    """Failover frees the dead session's claim before spawn; the child init
    acquires its own lockfile claim."""

    def _cand(self):
        return recovery.Candidate(
            short_id="aaaa1111", sock_path="/tmp/a.sock", jobs_dir=None,
            cwd="/wt/x-370f", name="dead-worker",
        )

    def _patch_resolve(self, monkeypatch, node="x-370f", done=False):
        monkeypatch.setattr(recovery, "_node_id_from_worktree", lambda cwd: node)
        monkeypatch.setattr(recovery, "_node_is_done", lambda n: done)

    def _patch_run(self, monkeypatch, *, stop_rc=0, stop_out=b"", force_release_rc=0,
                   spawn_rc=0):
        """Stub subprocess.run; record the (markered) calls for assertions."""
        from types import SimpleNamespace
        import subprocess as sp

        calls = []

        def fake_run(cmd, **kw):
            passthrough = _naming_real(cmd, **kw)
            if passthrough is not None:
                return passthrough
            calls.append(cmd)
            if cmd[:3] == ["fno-py", "agents", "stop"]:
                return SimpleNamespace(returncode=stop_rc, stdout=stop_out, stderr=b"")
            if cmd[:3] == ["fno-py", "mux", "pane", "kill"]:
                return SimpleNamespace(returncode=0)
            # Token containment, not a 3-slice: `claim force-release` became
            # `claim release --force`, and a prefix compare against a 4-token
            # marker is never true - it made this stub return the default 0 and
            # the force_release_rc=1 case silently pass a spawn it should block.
            if all(tok in cmd for tok in ("claim", "release", "--force")):
                return SimpleNamespace(returncode=force_release_rc)
            if cmd[:3] == ["fno-py", "agents", "spawn"]:
                return SimpleNamespace(returncode=spawn_rc)
            return SimpleNamespace(returncode=0)

        monkeypatch.setattr(sp, "run", fake_run)
        return calls

    @staticmethod
    def _index_of(calls, marker):
        """Index of the first call matching every token in ``marker``.

        Was a `c[:3]` prefix compare. `claim force-release` and `claim
        lane-release` collapsed into `claim release --force` / `--lane`, so a
        three-token prefix now matches BOTH and the two assertions below would
        silently pass on the wrong call. Matching on the full token set keeps
        each assertion pinned to the call it names.
        """
        return next(
            (i for i, c in enumerate(calls) if all(tok in c for tok in marker)),
            None,
        )

    @staticmethod
    def _assert_no_graph_claim_writes(calls):
        assert not any(
            c[:3] == ["fno-py", "backlog", "update"] for c in calls
        ), calls

    def test_force_release_before_spawn_happy_path(self, monkeypatch):
        # AC1-HP: stop -> release --force node:<id> -> canonical claude bg spawn.
        self._patch_resolve(monkeypatch)
        calls = self._patch_run(monkeypatch)
        assert recovery._redispatch(self._cand()) is True

        fr = self._index_of(calls, ["fno-py", "agents", "claim", "release", "--force"])
        spawn = self._index_of(calls, ["fno-py", "agents", "spawn"])
        assert fr is not None and spawn is not None
        assert fr < spawn                      # claim freed strictly before spawn
        assert "node:x-370f" in calls[fr]      # exact claim key
        assert "-R" in calls[fr]               # required audit reason supplied
        spawn_cmd = calls[spawn]
        assert "--harness" in spawn_cmd and "claude" in spawn_cmd
        assert "--substrate" in spawn_cmd and "bg" in spawn_cmd
        assert "--cwd" in spawn_cmd and "/wt/x-370f" in spawn_cmd
        self._assert_no_graph_claim_writes(calls)

    def test_stop_failure_skips_force_release_and_spawn(self, monkeypatch):
        # codex P2: a non-zero `fno agents stop` means the worker may still be
        # live; force-releasing its claim + spawning would create two workers on
        # one node. Bail to the nudge (no force-release, no spawn) → False.
        self._patch_resolve(monkeypatch)
        calls = self._patch_run(monkeypatch, stop_rc=1)
        assert not recovery._redispatch(self._cand())
        assert self._index_of(calls, ["fno-py", "agents", "claim", "release", "--force"]) is None
        assert self._index_of(calls, ["fno-py", "agents", "spawn"]) is None
        self._assert_no_graph_claim_writes(calls)

    def test_pane_refusal_runs_the_named_pane_kill_then_rotates(self, monkeypatch):
        # The daemon refuses `agents stop` on a pane row and NAMES the pane-kill
        # command with the row's own ref. The printed `main:10` selector is the
        # mux parser's own grammar, so a human can paste it as-is; the failover
        # still translates it to the flag form, which predates the selector and
        # parses identically. Treating the refusal as "may still be live" would
        # degrade every pane-worker failover to a nudge. The daemon-side
        # round-trip test in daemon.rs feeds this wording's real producer to
        # the real parser, so the two sides cannot drift apart.
        self._patch_resolve(monkeypatch)
        refusal = (
            b'agent w is a pane worker; `stop` reaches no pane and would report '
            b'a stop it did not perform. Kill the pane: `fno mux pane kill '
            b'main:10`. The registry row survives that.'
        )
        calls = self._patch_run(monkeypatch, stop_rc=1, stop_out=refusal)
        assert recovery._redispatch(self._cand()) is True
        kill = self._index_of(
            calls, ["fno-py", "mux", "pane", "kill", "--server", "main", "10"]
        )
        assert kill is not None, "the named pane-kill command ran"
        fr = self._index_of(calls, ["fno-py", "agents", "claim", "release", "--force"])
        spawn = self._index_of(calls, ["fno-py", "agents", "spawn"])
        assert fr is not None and spawn is not None
        assert kill < fr < spawn

    def test_claim_release_failure_skips_spawn(self, monkeypatch):
        # AC1-ERR: force-release non-zero → no spawn, False so the caller nudges.
        self._patch_resolve(monkeypatch)
        calls = self._patch_run(monkeypatch, force_release_rc=1)
        assert not recovery._redispatch(self._cand())
        assert self._index_of(calls, ["fno-py", "agents", "spawn"]) is None
        self._assert_no_graph_claim_writes(calls)

    def test_done_node_not_redispatched(self, monkeypatch):
        # AC1-EDGE: already-done node → no stop/force-release/spawn at all.
        self._patch_resolve(monkeypatch, done=True)
        calls = self._patch_run(monkeypatch)
        assert not recovery._redispatch(self._cand())
        assert calls == []

    def test_spawn_failure_releases_lane_slot(self, monkeypatch):
        # Parallel G4: no replacement worker → the dead lane's dispatch-time
        # slot is freed so lane-fill can re-select the node before the TTL.
        self._patch_resolve(monkeypatch)
        calls = self._patch_run(monkeypatch, spawn_rc=1)
        assert not recovery._redispatch(self._cand())
        # The lane slot frees through the native door now: <binary> claim
        # lane-release --lane <node> (the Python `claim release --lane` leg is
        # deleted), so the pin names the door the code actually walks.
        lr = self._index_of(calls, ["claim", "lane-release", "--lane"])
        assert lr is not None
        assert "x-370f" in calls[lr]

    def test_pre_spawn_runs_after_stop_release_before_spawn(self, monkeypatch):
        # codex P1 (US3): the managed materialize (pre_spawn) must run AFTER the
        # worker is stopped (so it no longer pins the slot) and its claim freed,
        # and BEFORE the replacement spawns (it must read the new account's creds).
        self._patch_resolve(monkeypatch)
        calls = self._patch_run(monkeypatch)
        assert recovery._redispatch(
            self._cand(), pre_spawn=lambda: calls.append(["MATERIALIZE"]) or True) is True
        stop = self._index_of(calls, ["fno-py", "agents", "stop"])
        fr = self._index_of(calls, ["fno-py", "agents", "claim", "release", "--force"])
        mat = next((i for i, c in enumerate(calls) if c == ["MATERIALIZE"]), None)
        spawn = self._index_of(calls, ["fno-py", "agents", "spawn"])
        assert None not in (stop, fr, mat, spawn)
        assert stop < mat < spawn      # materialize between the stop and the spawn
        assert fr < mat                # and after the claim is freed

    def test_pre_spawn_false_aborts_spawn_and_frees_lane(self, monkeypatch):
        # A False pre_spawn (materialize deferred/failed) → no spawn, lane slot
        # freed so the node re-dispatches fresh, and False so the caller nudges.
        self._patch_resolve(monkeypatch)
        calls = self._patch_run(monkeypatch)
        assert recovery._redispatch(self._cand(), pre_spawn=lambda: False) is False
        assert self._index_of(calls, ["fno-py", "agents", "spawn"]) is None
        assert self._index_of(calls, ["claim", "lane-release", "--lane"]) is not None
        self._assert_no_graph_claim_writes(calls)


class TestReviveBgThread:
    """US4/US5: node-less bg-thread revival - resume under the new account, or
    degrade to the manual-resume notify path (never a resume against a missing
    transcript)."""

    def _cand(self, tmp_path):
        return recovery.Candidate(short_id="eeee5555", sock_path="/tmp/e.sock",
                                  jobs_dir=tmp_path, cwd=str(tmp_path), name="thread-w")

    def _snap(self, auth="oauth_dir"):
        from types import SimpleNamespace
        return SimpleNamespace(id="claude-secondary", harness="claude", auth=auth)

    def test_visible_transcript_resumes_returns_swapped(self, monkeypatch, tmp_path):
        # AC4-FR: transcript visible -> respawn resuming the uuid; not notified.
        monkeypatch.setattr(recovery, "_resolve_session_uuid", lambda s: "U-1")
        monkeypatch.setattr(recovery, "_transcript_visible", lambda u, d: True)
        seen: dict = {}
        monkeypatch.setattr(recovery, "_respawn_bg_resume",
                            lambda cand, uuid, *, pre_spawn=None: seen.update(uuid=uuid) or True)
        notified = {"n": False}
        monkeypatch.setattr(recovery, "_notify_manual_resume",
                            lambda *a: notified.__setitem__("n", True))
        assert recovery._revive_bg_thread(
            self._cand(tmp_path), self._snap(), str(tmp_path), managed=False
        ) == "swapped"
        assert seen["uuid"] == "U-1"
        assert notified["n"] is False

    def test_unshared_transcript_notifies(self, monkeypatch, tmp_path):
        # AC3-FR: transcript not visible to the new account -> notify, never resume.
        monkeypatch.setattr(recovery, "_resolve_session_uuid", lambda s: "U-1")
        monkeypatch.setattr(recovery, "_transcript_visible", lambda u, d: False)
        monkeypatch.setattr(recovery, "_respawn_bg_resume",
                            lambda *a, **k: pytest.fail("must not resume a missing transcript"))
        seen: dict = {}
        monkeypatch.setattr(recovery, "_notify_manual_resume",
                            lambda cand, snap, uuid: seen.update(uuid=uuid))
        assert recovery._revive_bg_thread(
            self._cand(tmp_path), self._snap(), str(tmp_path), managed=False
        ) == "notified"
        assert seen["uuid"] == "U-1"

    def test_managed_disarmed_falls_through_to_nudge(self, monkeypatch, tmp_path):
        # A disarmed managed swap never materializes the slot, so a resume would
        # land on the exhausted account: fall to the nudge, never reach visibility.
        monkeypatch.setattr(recovery, "_resolve_session_uuid", lambda s: "U-1")
        monkeypatch.setattr(recovery, "_auto_switch_enabled", lambda repo_root=None: False)
        monkeypatch.setattr(recovery, "_transcript_visible",
                            lambda u, d: pytest.fail("disarmed managed must not reach visibility"))
        assert recovery._revive_bg_thread(
            self._cand(tmp_path), self._snap(auth="managed"), str(tmp_path), managed=True
        ) == "rotated-no-worker"

    def test_managed_visible_materializes_via_pre_spawn(self, monkeypatch, tmp_path):
        # A managed revival threads the materialize into _respawn_bg_resume's
        # pre_spawn (stop -> materialize -> spawn), mirroring _redispatch.
        monkeypatch.setattr(recovery, "_resolve_session_uuid", lambda s: "U-1")
        monkeypatch.setattr(recovery, "_auto_switch_enabled", lambda repo_root=None: True)
        monkeypatch.setattr(recovery, "_transcript_visible", lambda u, d: True)
        mat: list = []
        monkeypatch.setattr(recovery, "_materialize_managed_switch",
                            lambda rid, repo_root=None: mat.append(rid) or True)
        captured: dict = {}

        def _fake_respawn(cand, uuid, *, pre_spawn=None):
            captured["pre_spawn_result"] = pre_spawn() if pre_spawn else None
            return True
        monkeypatch.setattr(recovery, "_respawn_bg_resume", _fake_respawn)
        assert recovery._revive_bg_thread(
            self._cand(tmp_path), self._snap(auth="managed"), str(tmp_path), managed=True
        ) == "swapped"
        assert captured["pre_spawn_result"] is True   # materialize ran as pre_spawn
        assert mat == ["claude-secondary"]


class TestRespawnBgResume:
    """The node-less resume respawn: stop -> pre_spawn -> ``claude --bg --resume``."""

    def _cand(self):
        return recovery.Candidate(short_id="ffff6666", sock_path="/tmp/f.sock",
                                  jobs_dir=None, cwd="/wt/thread", name="thread-w")

    def _patch_run(self, monkeypatch, *, stop_rc=0, spawn_rc=0):
        from types import SimpleNamespace
        import subprocess as sp
        calls = []

        def fake_run(cmd, **kw):
            passthrough = _naming_real(cmd, **kw)
            if passthrough is not None:
                return passthrough
            calls.append(cmd)
            if cmd[:3] == ["fno-py", "agents", "stop"]:
                return SimpleNamespace(returncode=stop_rc)
            if cmd[:3] == ["fno-py", "agents", "spawn"]:
                return SimpleNamespace(returncode=spawn_rc)
            return SimpleNamespace(returncode=0)
        monkeypatch.setattr(sp, "run", fake_run)
        return calls

    def test_happy_path_builds_resume_spawn(self, monkeypatch):
        calls = self._patch_run(monkeypatch)
        assert recovery._respawn_bg_resume(self._cand(), "U-abc") is True
        spawn = next(c for c in calls if c[:3] == ["fno-py", "agents", "spawn"])
        assert "--substrate" in spawn and "bg" in spawn
        assert "--resume" in spawn and "U-abc" in spawn
        assert "--cwd" in spawn and "/wt/thread" in spawn
        assert spawn[-1] == recovery.CONTINUE_MESSAGE   # seeds the continue turn

    def test_stop_failure_skips_spawn(self, monkeypatch):
        # A non-zero stop means the thread may be live; a second --resume would
        # double it. Bail (the caller notifies).
        calls = self._patch_run(monkeypatch, stop_rc=1)
        assert recovery._respawn_bg_resume(self._cand(), "U-abc") is False
        assert not any(c[:3] == ["fno-py", "agents", "spawn"] for c in calls)

    def test_no_name_bails_without_spawn(self, monkeypatch):
        # Finding 2: with no name we cannot stop the dead thread, and the node-less
        # path has no claim+init backstop against a double, so a blind --resume
        # spawn could put two supervisors on one transcript. Bail (False), spawn
        # nothing (the caller notifies).
        calls = self._patch_run(monkeypatch)
        cand = recovery.Candidate(short_id="ffff6666", sock_path="/tmp/f.sock",
                                  jobs_dir=None, cwd="/wt/thread", name=None)
        assert recovery._respawn_bg_resume(cand, "U-abc") is False
        assert calls == []   # neither stop nor spawn ran


class TestWorktreeIsNodeLess:
    """Finding 1: revival gates on confirmed manifest ABSENCE, so an unreadable /
    transiently-unreachable manifest never misroutes a node-bound worker."""

    def test_absent_manifest_is_node_less(self, tmp_path):
        (tmp_path / ".fno").mkdir()
        assert recovery._worktree_is_node_less(str(tmp_path)) is True

    def test_existing_manifest_is_not_node_less_regardless_of_content(self, tmp_path):
        # A present-but-garbage manifest still means a /target worker (a read miss
        # must not read as node-less): it exists, so treat it as node-bound.
        fno = tmp_path / ".fno"
        fno.mkdir()
        (fno / "target-state.md").write_text("\x00 not utf parseable", encoding="latin-1")
        assert recovery._worktree_is_node_less(str(tmp_path)) is False


class TestNotifyManualResume:
    """US5: the manual-resume OS notification carries the exact resume command."""

    def _cand(self):
        return recovery.Candidate(short_id="9999aaaa", sock_path="/tmp/g.sock",
                                  jobs_dir=None, cwd="/wt/thread", name="thread-w")

    def test_spacey_config_dir_is_shell_escaped(self, monkeypatch):
        # A config dir with spaces must stay one shell token when pasted (gemini
        # review): shlex.quote wraps it so the CLAUDE_CONFIG_DIR assignment holds.
        from fno.adapters.providers import dispatch as dispatch_mod
        monkeypatch.setattr(dispatch_mod, "dispatch_env",
                            lambda pid, **k: {"CLAUDE_CONFIG_DIR": "/Users/u/Application Support/.claude"})
        assert recovery._resume_command("claude-b", "/wt/thread", "U-1") == \
            "CLAUDE_CONFIG_DIR='/Users/u/Application Support/.claude' claude --resume U-1"

    def test_notify_sends_os_notification_with_command(self, monkeypatch):
        from types import SimpleNamespace
        from fno.notify import _impl as notify_impl
        sent: dict = {}
        monkeypatch.setattr(notify_impl, "send_notification",
                            lambda title, body: sent.update(title=title, body=body) or (0, ""))
        monkeypatch.setattr(recovery, "_resume_command", lambda *a: "claude --resume U-1")
        recovery._notify_manual_resume(self._cand(),
                                       SimpleNamespace(id="claude-secondary"), "U-1")
        assert "claude --resume U-1" in sent["body"]
        assert "claude-secondary" in sent["title"]


# ---------------------------------------------------------------------------
# Re-dispatch across the harness axis (AC6-*)
# ---------------------------------------------------------------------------


requires_rust = pytest.mark.dev_build


@requires_rust
class TestRedispatchAxisBundle:
    """The spawn call actually changes vendor, not just its label."""

    def test_flags_replace_the_hardcoded_claude_bg_pair(self, tmp_path, monkeypatch):
        calls = []
        monkeypatch.setattr(
            recovery, "_node_id_from_worktree", lambda cwd: "x-test", raising=True,
        )
        monkeypatch.setattr(recovery, "_node_is_done", lambda n: False, raising=True)

        class _Ok:
            returncode = 0

        import subprocess as _sp
        monkeypatch.setattr(
            _sp, "run",
            lambda cmd, **kw: calls.append(cmd)
            or (_naming_real(cmd, **kw) or _Ok()),
            raising=True,
        )

        c = _stale_candidate(tmp_path)
        assert recovery._redispatch(
            c, flags=["-H", "codex", "-m", "gpt-5.6-sol", "--substrate", "pane"],
        ) is True
        spawn_cmd = [x for x in calls if "spawn" in x][0]
        assert "-H" in spawn_cmd and "codex" in spawn_cmd
        assert spawn_cmd[spawn_cmd.index("--substrate") + 1] == "pane"
        assert "--harness" not in spawn_cmd



class TestSweepWritesTheQuotaLock:
    """x-bbc0 change 2: a corroborated quota refusal cools the account the
    worker was LAUNCHED on. The event fires even when nothing was written -
    an unattributable refusal is the finding, not silence."""

    _QUOTA = (
        "Claude usage limit reached. Your limit will reset at "
        "2026-08-18T07:19:38+08:00"
    )

    def _cand(self, tmp_path, account):
        c = _stale_candidate(tmp_path)
        c.launch_account = account
        return c

    def _sweep(self, h, tmp_path, counts, account):
        recovery.recovery_sweep(
            _now(), _Cfg(),
            candidates=[self._cand(tmp_path, account)],
            counts=counts,
            emit=h.emit, read_state_fn=h.read_state,
            truth_fn=h.truth, liveness_fn=h.liveness,
        )

    def _lock_calls(self, monkeypatch):
        import fno.agents.quota_lock as ql

        calls: list[tuple] = []

        def _rec(acct, text, resets_at=None):
            calls.append((acct, resets_at))
            return acct

        monkeypatch.setattr(ql, "record_quota_lock", _rec)
        return calls

    def test_second_corroborated_tick_locks_the_launch_account_once(
        self, tmp_path, monkeypatch
    ):
        monkeypatch.setenv(
            "FNO_RUNTIME_STATE_PATH", str(tmp_path / "provider-runtime-state.json")
        )
        calls = self._lock_calls(monkeypatch)
        counts: dict = {}

        h1 = self._harness()
        self._sweep(h1, tmp_path, counts, "readyrule")  # tick 1
        assert calls == []  # corroboration not yet met
        assert "provider_quota_locked" not in h1.event_types()

        h2 = self._harness()
        self._sweep(h2, tmp_path, counts, "readyrule")  # tick 2: same refusal
        assert len(calls) == 1
        assert calls[0][0] == "readyrule"
        payload = dict(
            h2.events[h2.event_types().index("provider_quota_locked")][1]
        )
        assert payload["attributed"] is True
        assert payload["account"] == "readyrule"
        assert payload["source"] == "transcript"
        assert payload["resets_at"] is not None

        h3 = self._harness()
        self._sweep(h3, tmp_path, counts, "readyrule")  # tick 3: once per worker
        assert len(calls) == 1
        assert "provider_quota_locked" not in h3.event_types()

    def _harness(self):
        return _RefusalHarness(self._QUOTA)

    def test_an_unattributed_refusal_emits_and_writes_nothing(
        self, tmp_path, monkeypatch
    ):
        # The REAL record_quota_lock runs: launch_account None must write
        # nothing, and the event must still say so.
        monkeypatch.setenv(
            "FNO_RUNTIME_STATE_PATH", str(tmp_path / "provider-runtime-state.json")
        )
        counts: dict = {}
        h1 = self._harness()
        self._sweep(h1, tmp_path, counts, None)
        h2 = self._harness()
        self._sweep(h2, tmp_path, counts, None)  # corroboration met

        payload = dict(
            h2.events[h2.event_types().index("provider_quota_locked")][1]
        )
        assert payload["attributed"] is False
        assert payload["account"] is None
        from fno.adapters.providers.runtime_state import read_state

        assert read_state().provider_health == {}
