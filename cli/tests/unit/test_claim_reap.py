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

import json
import os
import socket
from pathlib import Path
from types import SimpleNamespace

import psutil
import pytest
from typer.testing import CliRunner

from fno.claims.cli import cli
from fno.claims.core import (
    acquire_claim,
    claim_status,
    release_claim,
)
from fno.claims.io import claim_path, serialize_claim
from fno.claims.types import Claim, now_ms
from fno.claims.verdict import claim_verdicts


HOLDER_A = "target-session:sid-a"
runner = CliRunner()


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


def is_provably_dead(claim: Claim, now: int | None = None) -> bool:
    return _native_sweep_verdict(claim)[0]


class TestReapDeadClaims:
    def test_node_release_does_not_open_or_write_the_graph(self, tmp_path, monkeypatch):
        node_id = "x-release-lockfile-only"
        holder = "target-session:release-lockfile-only"
        graph_path = tmp_path / "configured-graph.json"
        graph_path.write_text(json.dumps({"entries": [{"id": node_id, "locked_by": "old-copy"}]}) + "\n")
        before = graph_path.read_text()
        monkeypatch.setattr(
            "fno.paths.graph_json",
            lambda: pytest.fail("claim release must not open the graph"),
        )

        claim = acquire_claim(f"node:{node_id}", holder, pid=os.getpid(), root=tmp_path)
        released = release_claim(claim.key, holder, root=tmp_path)

        assert isinstance(released, Claim)
        assert claim_status(claim.key, root=tmp_path)["state"] == "free"
        assert graph_path.read_text() == before

# ---------------------------------------------------------------------------
# acquire_claim's idempotent re-acquire vs reap's recovery mutex: a
# respawned worker refreshing the same holder string must not have its live
# write clobbered by a concurrent reap sweep that proved the OLD pid dead.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# refresh_claim vs reap's recovery mutex: a TTL claim reap has proven dead
# and is archiving must not be resurrected by a concurrent refresh.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# list_claims_with_counts / `fno agents claim list`: the reader must not lie
# ---------------------------------------------------------------------------


class TestReaderReportsFilteredCount:
    def test_cli_list_does_not_print_bare_no_claims_string(self, cwd_tmp):
        for i in range(3):
            acquire_claim(f"k{i}", HOLDER_A, pid=_dead_pid(), root=cwd_tmp)

        result = runner.invoke(cli, ["list"])

        assert result.exit_code == 0
        assert result.output.strip() != "no claims"
        assert "no live claims (" in result.output
        assert "3 stale" in result.output

    def test_cli_list_labels_rows_with_root(self, cwd_tmp):
        acquire_claim("k-live", HOLDER_A, pid=os.getpid(), root=cwd_tmp)

        result = runner.invoke(cli, ["list"])

        assert result.exit_code == 0
        assert "root=" in result.output


# ---------------------------------------------------------------------------
# `fno agents claim reap` CLI verb
# ---------------------------------------------------------------------------


class TestReapCliVerb:
    def test_json_output_is_parseable(self, cwd_tmp):
        acquire_claim("k", HOLDER_A, pid=_dead_pid(), root=cwd_tmp)
        import json

        result = runner.invoke(cli, ["reap", "--json"])

        assert result.exit_code == 0
        payload = json.loads(result.output)
        assert payload["would_reap"] == 1


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

    def test_a_nested_dispatch_token_is_kept_too(self):
        """`dispatch:think:<node>:<reason>` is a dedup token with no node claim
        behind it, so reaping it early re-runs the work it deduplicated."""
        provably_dead, bucket = classify_for_sweep(
            self._suspect("dispatch:think:x-18ac:birth")
        )
        assert (provably_dead, bucket) == (False, "suspect")

    def test_an_expired_dispatch_reservation_is_still_reapable(self):
        """Expiry frees them on schedule, which is the whole recovery path the
        sweep is allowed to take."""
        claim = Claim(
            key="dispatch:x-old", holder="spawn-cli:1",
            acquired_at=now_ms() - 240_000, expires_at=now_ms() - 60_000,
            pid=_dead_pid(), host=socket.gethostname(),
        )
        assert is_provably_dead(claim) is True

    def test_a_node_key_is_kept_too(self):
        provably_dead, bucket = classify_for_sweep(self._suspect("node:x-05be"))
        assert (provably_dead, bucket) == (False, "suspect")

    def test_an_off_host_dispatch_reservation_is_still_opaque(self):
        claim = Claim(
            key="dispatch:x-far", holder="spawn-cli:1", acquired_at=now_ms(),
            expires_at=now_ms() + 180_000, pid=_dead_pid(),
            host="some-other-host", machine_id="not-this-machine",
        )
        provably_dead, bucket = classify_for_sweep(claim)
        assert (provably_dead, bucket) == (False, "offhost")


# ---------------------------------------------------------------------------
# the abandonment probe: reap only on a positive finding (x-05be change 2)
# ---------------------------------------------------------------------------


class TestCliProbeWiring:
    """The CLI is what injects the roster join, so the wiring needs its own
    coverage: a probe that exists but is never passed is a decorative guard."""

    def _fake_roster(self, monkeypatch, rows, warnings=()):
        def _fake(*_a, **_kw):
            return list(rows), list(warnings)

        monkeypatch.setattr("fno.agents.watchdog.fleet_rows", _fake)

    def test_an_empty_scan_never_reaps(self, tmp_path, monkeypatch):
        """Zero rows scanned is not a finding, even with no read error: there is
        nothing to have found the worker in."""
        acquire_claim(
            key="node:x-empty", holder="target-session:s", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        self._fake_roster(monkeypatch, rows=[])
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output
        assert "probe unanswered: row-absent-no-cwd 1" in r.output

    def _transcript_says(self, monkeypatch, finished):
        monkeypatch.setattr(
            "fno.claims.cli._transcript_says_finished", lambda *_a, **_kw: finished
        )

    def test_the_holder_found_and_finished_reaps(self, tmp_path, monkeypatch):
        """Abandonment is proven by FINDING the holder and seeing it stopped."""
        from fno.agents.watchdog import Row

        acquire_claim(
            key="node:x-abandoned", holder="target-session:sid-gone", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        self._fake_roster(
            monkeypatch,
            rows=[Row(row_id="sid-gone", name="t-gone", state="done", node="x-abandoned", cwd="")],
        )
        self._transcript_says(monkeypatch, True)
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 1" in r.output

    def test_a_terminal_row_whose_transcript_is_alive_is_never_reaped(
        self, tmp_path, monkeypatch
    ):
        """The row state alone cannot authorize a reap. `_TERMINAL_STATES`
        carries its own warning that the roster called a WORKING session done
        on 2026-08-15, and archiving on it hands a live worker's node to the
        next dispatcher - the `reaped_a_live_worker` kill criterion."""
        from fno.agents.watchdog import Row

        acquire_claim(
            key="node:x-lying", holder="target-session:sid-busy", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        self._fake_roster(
            monkeypatch,
            rows=[Row(row_id="sid-busy", name="t-busy", state="done", node="x-lying", cwd="")],
        )
        self._transcript_says(monkeypatch, False)
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output

    def test_the_holder_found_and_working_keeps(self, tmp_path, monkeypatch):
        from fno.agents.watchdog import Row

        acquire_claim(
            key="node:x-busy2", holder="target-session:sid-busy", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        self._fake_roster(
            monkeypatch,
            rows=[Row(row_id="sid-busy", name="t-busy2", state="working", node="x-busy2", cwd="")],
        )
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output
        assert "suspect (worker alive)" in r.output

    def test_an_unparseable_holder_is_never_reaped(self, tmp_path, monkeypatch):
        from fno.agents.watchdog import Row

        acquire_claim(
            key="node:x-odd", holder="some-foreign-holder-shape", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        self._fake_roster(
            monkeypatch,
            rows=[Row(row_id="a", name="t-a", state="done", node="x-odd", cwd="")],
        )
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output


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
        acquire_claim(
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
        """x-c272: the lead killed the holder's pane and removed its registry
        row; the spawn-handover claim survived both and the next dispatch
        refused. Pane positively absent from the mux listing AND the recorded
        spawner pid dead is the launch window OVER - a positive finding."""
        self._pane(monkeypatch, absent=True)
        self._handover(tmp_path)
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 1" in r.output

    def test_handover_whose_pane_is_still_live_stays(self, tmp_path, monkeypatch):
        """The negative that keeps the fix honest: a pane still hosting the
        worker is the launch window OPEN, and the claim keeps."""
        self._pane(monkeypatch, absent=False)
        self._handover(tmp_path)
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output

    def test_handover_with_absent_pane_but_live_spawner_pid_stays(
        self, tmp_path, monkeypatch
    ):
        """Pane gone but the spawner process itself still runs: it may be
        mid-relaunch onto a new pane, so neither absence alone may reap."""
        self._pane(monkeypatch, absent=True)
        self._handover(tmp_path, pid=os.getpid())
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output

    def test_handover_when_the_mux_cannot_answer_stays(self, tmp_path, monkeypatch):
        """An unverifiable pane listing (None) is not absence: unknown keeps."""
        monkeypatch.setattr(
            "fno.claims.cli._mux_pane_absent_for",
            lambda worker, node_id="", runner=None: None,
        )
        self._handover(tmp_path)
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output

    def test_handover_in_place_policy_stays_unknown(self, tmp_path, monkeypatch):
        """An in-place worker's pane has no worker/worktree identity, so a
        missing match must not prove that its live pane is gone."""
        self._pane(monkeypatch, absent=True)
        monkeypatch.setattr(
            "fno.worktree_paths.resolve_worktree_policy",
            lambda *_a, **_kw: SimpleNamespace(policy="never"),
        )
        self._handover(tmp_path)
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output

    def test_specimen_1_degraded_then_recovered_roster_reaps(
        self, tmp_path, monkeypatch
    ):
        """x-3f84: the lead killed the holder's whole tree and verified five
        pids gone; `claim reap` still reported `kept: 1 suspect (roster not
        consulted)` because one degraded roster read answered None for the
        whole pass. One retry on a degraded reading resolves it here."""
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
        acquire_claim(
            key="node:x-3f84", holder="target-session:sid-3f84",
            ttl_ms=3_600_000, pid=_dead_pid(), root=tmp_path,
        )
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 1" in r.output
        assert calls["n"] == 2  # degraded once, retried once, settled

    def _claimed_no_row(self, tmp_path, *, metadata=None):
        acquire_claim(
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
        self._claimed_no_row(tmp_path, metadata={"worktree": "/tmp/wt-x"})
        monkeypatch.setattr(
            "fno.claims.cli._transcript_says_finished", lambda *_a, **_kw: True
        )
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 1" in r.output

    def test_dead_pid_without_worktree_metadata_stays(self, tmp_path, monkeypatch):
        """No cwd to find the tree with: the probe still answers None. The
        fallback is only as live as the worktree stamp init now writes."""
        self._roster_without_holder(monkeypatch)
        self._claimed_no_row(tmp_path)
        monkeypatch.setattr(
            "fno.claims.cli._transcript_says_finished",
            lambda *_a, **_kw: (_ for _ in ()).throw(
                AssertionError("no transcript lookup without a worktree")
            ),
        )
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output

    def test_dead_pid_with_live_transcript_stays(self, tmp_path, monkeypatch):
        """An unfinished transcript overrules the fallback: still working."""
        self._roster_without_holder(monkeypatch)
        self._claimed_no_row(tmp_path, metadata={"worktree": "/tmp/wt-x"})
        monkeypatch.setattr(
            "fno.claims.cli._transcript_says_finished", lambda *_a, **_kw: False
        )
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output


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

    def test_match_by_fno_id_or_title_means_present(self):
        from fno.claims.cli import _mux_pane_absent_for

        runner = self._runner(
            [
                self._Proc(0, '[{"session":"main","state":"live","panes":2}]'),
                self._Proc(
                    0,
                    '[{"pane_id":2,"fno_id":"other"},'
                    '{"pane_id":3,"fno_id":null,"title":"bp-x-worker"}]',
                ),
            ]
        )
        assert _mux_pane_absent_for("bp-x-worker", runner=runner) is False

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

    def test_match_by_node_id_worktree_name_means_present(self):
        """The `target start` naming: a worktree named after the node id."""
        from fno.claims.cli import _mux_pane_absent_for

        runner = self._runner(
            [
                self._Proc(0, '[{"session":"main","state":"live","panes":1}]'),
                self._Proc(
                    0,
                    '[{"pane_id":5,"fno_id":null,"title":null,'
                    '"cwd":"/Users/x/.fno/worktrees/footnote/x-c272"}]',
                ),
            ]
        )
        assert _mux_pane_absent_for("bp-x-worker", node_id="x-c272", runner=runner) is False

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

    def test_uninspectable_live_session_keeps_mixed_listing_unknown(self):
        """One unreadable live session invalidates absence from another
        session's unrelated pane listing."""
        from fno.claims.cli import _mux_pane_absent_for

        runner = self._runner(
            [
                self._Proc(
                    0,
                    '[{"session":"main","state":"live","panes":1},'
                    '{"session":"other","state":"live","panes":1}]',
                ),
                self._Proc(
                    0,
                    '[{"pane_id":2,"fno_id":"someone-else",'
                    '"title":null,"cwd":"/Users/x/code/other"}]',
                ),
                self._Proc(1, "pane socket unavailable"),
            ]
        )
        assert _mux_pane_absent_for("bp-x-worker", runner=runner) is None

    def test_no_live_sessions_is_unknown(self):
        from fno.claims.cli import _mux_pane_absent_for

        runner = self._runner([self._Proc(0, '[{"session":"main","state":"stale"}]')])
        assert _mux_pane_absent_for("bp-x-worker", runner=runner) is None


class TestSweepReadsWalkedDir:
    """AC1/AC2 (x-9c91): the sweep asks the native door about the directory it
    walks, and a claim with no verdict there is unclassified, never unprobed.

    The old root=cdir.parent.parent round-trip re-resolved the space claims
    dir one level down, so all 19 space-root claims got no verdict and fell
    to unknown-keeps labelled "roster not consulted" - a cause nobody
    measured."""

    def test_AC2_ERR_sweep_event_carries_the_new_buckets(self, monkeypatch):
        from fno.claims import events as claim_events

        captured: dict = {}
        monkeypatch.setattr(claim_events, "_emit", lambda event: captured.update(event))
        summary = {
            "scanned": 1, "reaped": 0, "would_reap": 0, "kept_live": 0,
            "kept_suspect": 0, "kept_suspect_alive": 0, "kept_suspect_unprobed": 1,
            "kept_unclassified": 2, "unclassified_dirs": {"/claims": 2},
            "kept_suspect_unprobed_by": {"roster-read-degraded": 1},
            "kept_offhost": 0, "corrupted": 0, "vanished": 0, "contended": 0,
            "reap_failed": [], "apply": True,
            "roots": ["/claims"],
        }
        claim_events.emit_claim_reap_swept(summary)
        data = captured["data"]
        assert data["kept_unclassified"] == 2
        assert data["unclassified_dirs"] == {"/claims": 2}
        assert data["kept_suspect_unprobed_by"] == {"roster-read-degraded": 1}


# ---------------------------------------------------------------------------
# Reap contracts ported from the lockfile era: the claims table is read
# through claim_status, never through files on disk.
# ---------------------------------------------------------------------------


def _held(key, root):
    return claim_status(key, root=root)["state"] != "free"


class TestReapOverTheClaimsTable:
    def test_AC7_counts_reconcile_with_the_table(self, tmp_path, monkeypatch):
        from fno.claims.core import list_claims_with_counts

        monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "global"))
        for i in range(5):
            acquire_claim(f"k{i}", HOLDER_A, pid=_dead_pid(), root=tmp_path)
        acquire_claim("k-live", HOLDER_A, pid=os.getpid(), root=tmp_path)

        rows, counts, states_by_key = list_claims_with_counts(root=tmp_path)

        assert counts["total"] == 6
        assert counts["stale"] == 5
        assert counts["live"] == 1
        assert len(rows) == 1, "default include_stale=False shows only the live row"
        assert len(states_by_key) == 6
        assert states_by_key["k-live"] == "live"
        assert states_by_key["k0"] == "stale"


class TestReapCliOverTheTable:
    def test_dry_run_is_the_default(self, cwd_tmp):
        acquire_claim("k", HOLDER_A, pid=_dead_pid(), root=cwd_tmp)
        result = runner.invoke(cli, ["reap"])
        assert result.exit_code == 0, result.output
        assert "would reap 1" in result.output
        assert _held("k", cwd_tmp)

    def test_apply_archives_and_exits_zero(self, cwd_tmp):
        acquire_claim("k", HOLDER_A, pid=_dead_pid(), root=cwd_tmp)
        result = runner.invoke(cli, ["reap", "--apply"])
        assert result.exit_code == 0, result.output
        assert "reaped 1" in result.output
        assert not _held("k", cwd_tmp)


class TestRosterWiringOverTheTable:
    _fake_roster = TestCliProbeWiring._fake_roster

    def test_a_blind_roster_never_reaps_and_says_so(self, tmp_path, monkeypatch):
        acquire_claim(
            key="node:x-blind", holder="target-session:s", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        self._fake_roster(monkeypatch, rows=[], warnings=["claude not on PATH"])
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output
        assert "roster-read-degraded 1 (roster not consulted)" in r.output

    def test_a_holder_absent_from_the_roster_is_never_reaped(self, tmp_path, monkeypatch):
        """fleet_rows cannot represent a codex or hand-started holder, so its
        absence there is never read as abandonment."""
        from fno.agents.watchdog import Row

        acquire_claim(
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
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output
        assert "probe unanswered: row-absent-no-cwd 1" in r.output

    def test_a_still_degraded_roster_after_retry_keeps(self, tmp_path, monkeypatch):
        calls = {"n": 0}

        def _always_degraded(*_a, **_kw):
            calls["n"] += 1
            return [], ["claude not on PATH"]

        monkeypatch.setattr("fno.agents.watchdog.fleet_rows", _always_degraded)
        acquire_claim(
            key="node:x-dead-roster", holder="target-session:sid-1",
            ttl_ms=3_600_000, pid=_dead_pid(), root=tmp_path,
        )
        r = runner.invoke(cli, ["reap", "--root", str(tmp_path)])
        assert "would reap 0" in r.output
        assert "roster not consulted" in r.output
        assert calls["n"] == 2
