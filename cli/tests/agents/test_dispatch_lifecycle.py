"""Tests for fno.agents.dispatch lifecycle verbs.

Wave-1 coverage (US4-lifecycle):

- ``reconcile_agents`` (AC3-*)

``attach`` has no Python leg: the Rust client verb owns it, and its refusals
are pinned in ``test_rust_verb_parity.py``.

Each test monkeypatches the corresponding helper in
``fno.agents.harnesses.{claude,codex}`` so we exercise the
dispatch surface in isolation; the provider adapters have their own
shellout-level tests.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


def _seed_registry(*entries):
    """Persist a list of AgentEntry dicts as the active registry."""
    from fno.agents.registry import AgentEntry, write_registry

    out: list[AgentEntry] = []
    for kwargs in entries:
        kwargs.setdefault("cwd", "/tmp")
        kwargs.setdefault("log_path", "/tmp/x.log")
        _canonize_identity_kwargs(kwargs)
        out.append(AgentEntry(**kwargs))
    write_registry(out)
    return out


def _canonize_identity_kwargs(kwargs: dict) -> None:
    """Map legacy identity kwargs to the v10 canonical fields (x-880e), mirroring
    what load_registry back-fills for on-disk rows, so terse test dicts still build."""
    if "provider" in kwargs:
        kwargs["harness"] = kwargs.pop("provider")
    for _k in ("codex_session_id", "gemini_session_id", "claude_session_uuid"):
        if _k in kwargs:
            kwargs.setdefault("harness_session_id", kwargs.pop(_k))


def _force_claude_on_path(monkeypatch, tmp_path: Path) -> None:
    """Make ``is_provider_available('claude')`` return True without a real binary.

    We monkeypatch ``shutil.which`` via the ``dispatch`` module so all
    PATH checks see a positive result; the per-call ``claude_*`` helpers
    are independently patched per-test.
    """
    from fno.agents import dispatch as dispatch_mod

    bin_dir = tmp_path / "fakebin"
    bin_dir.mkdir(exist_ok=True)
    fake = bin_dir / "claude"
    fake.write_text("#!/bin/sh\nexit 0\n")
    fake.chmod(0o755)
    monkeypatch.setenv("PATH", str(bin_dir))
    # Defense against test order: the dispatch module imports shutil at
    # import time; PATH env tweak above is sufficient because
    # shutil.which is called on every is_provider_available invocation.
    assert dispatch_mod.is_provider_available("claude") is True


def _read_events(tmp_path: Path) -> list[dict]:
    """Return the committed event rows for the default journal.

    Mirrors the helper in test_codex_fatal_error_dispatch.py; copied
    here so each lifecycle test can verify the forensic event-stream
    contract independently of provider failure tests.
    """
    from fno import paths

    from tests._event_rows import event_rows

    return event_rows(paths.state_dir() / "events.jsonl")


def test_reconcile_orphan_detection(tmp_path: Path, monkeypatch) -> None:
    """AC3-HP: live claude agent flips to orphaned when logs probe fails."""
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-claude",
            provider="claude",
            short_id="7c5dcf5d",
            status="live",
            last_message_at="2026-05-01T00:00:00Z",
        ),
    )
    # is_provider_available("claude") must return True so reconcile reaches
    # the claude_logs_reachable monkeypatch instead of routing to errors
    # (sigma-review C1 fix: missing claude -> errors, not orphaned).
    _force_claude_on_path(monkeypatch, tmp_path)

    from fno.agents import dispatch
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.registry import load_registry

    monkeypatch.setattr(
        claude_mod, "claude_logs_reachable",
        lambda short_id, *, timeout=10.0: False,
    )

    result = dispatch.reconcile_agents()

    assert result.scanned == 1
    assert len(result.orphaned) == 1
    assert result.orphaned[0]["name"] == "worker-claude"
    # last_message_at preserved.
    entries = load_registry()
    assert entries[0].status == "orphaned"
    assert entries[0].last_message_at == "2026-05-01T00:00:00Z"

    # AC3-HP forensic contract: reconcile_done aggregate with the right counts.
    events = _read_events(tmp_path)
    done = [e for e in events if e.get("kind") == "reconcile_done"]
    assert len(done) == 1
    assert done[0]["scanned"] == 1
    assert done[0]["orphaned"] == 1
    assert done[0]["recovered"] == 0


def test_reconcile_recovery(tmp_path: Path, monkeypatch) -> None:
    """AC3-ERR: orphaned claude agent flips back to live on reachable probe."""
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-claude",
            provider="claude",
            short_id="7c5dcf5d",
            status="orphaned",
        ),
    )
    _force_claude_on_path(monkeypatch, tmp_path)

    from fno.agents import dispatch
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.registry import load_registry

    monkeypatch.setattr(
        claude_mod, "claude_logs_reachable",
        lambda short_id, *, timeout=10.0: True,
    )

    result = dispatch.reconcile_agents()

    assert len(result.recovered) == 1
    assert load_registry()[0].status == "live"

    # AC3-ERR forensic contract.
    events = _read_events(tmp_path)
    done = [e for e in events if e.get("kind") == "reconcile_done"]
    assert len(done) == 1
    assert done[0]["recovered"] == 1
    assert done[0]["orphaned"] == 0


def test_reconcile_backfills_null_harness_session_id(tmp_path: Path, monkeypatch) -> None:
    """AC1-FR / AC1-UI (x-ec59): a live claude row whose canonical id never landed
    (the spawn-time uuid resolution raced) is healed from the harness store, and
    reconcile names the backfilled row in its result + the reconcile_done event."""
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-claude",
            provider="claude",
            short_id="7c5dcf5d",
            status="live",
        ),
    )
    _force_claude_on_path(monkeypatch, tmp_path)

    from fno.agents import dispatch
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.registry import load_registry

    monkeypatch.setattr(
        claude_mod, "claude_logs_reachable", lambda short_id, *, timeout=10.0: True
    )
    monkeypatch.setattr(
        claude_mod, "resolve_session_uuid", lambda short_id: "FULL-UUID-7c5dcf5d"
    )

    result = dispatch.reconcile_agents()

    assert len(result.backfilled) == 1
    assert result.backfilled[0]["name"] == "worker-claude"
    assert result.backfilled[0]["harness_session_id"] == "FULL-UUID-7c5dcf5d"

    # The row gains the canonical harness_session_id field.
    e = load_registry()[0]
    assert e.harness_session_id == "FULL-UUID-7c5dcf5d"

    events = _read_events(tmp_path)
    done = [ev for ev in events if ev.get("kind") == "reconcile_done"]
    assert done and done[0]["backfilled"] == 1


def test_reconcile_codex_index_missing(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """AC3-EDGE: missing ~/.codex/session_index.jsonl yields an error entry,
    codex statuses untouched, exit 0."""
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-codex",
            provider="codex",
            codex_session_id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            status="live",
        ),
    )

    from fno.agents import dispatch
    from fno.agents.harnesses import codex as codex_mod
    from fno.agents.registry import load_registry

    # Override the index path to a non-existent file.
    missing = tmp_path / "no-codex-index.jsonl"
    monkeypatch.setattr(
        codex_mod, "default_session_index_path", lambda: missing
    )

    result = dispatch.reconcile_agents()

    assert len(result.errors) == 1
    assert result.errors[0]["reason"] == "codex-session-index-missing"
    # Status unchanged.
    assert load_registry()[0].status == "live"
    err = capsys.readouterr().err
    assert "codex session index missing" in err


def test_reconcile_backfills_late_codex_pane_identity_without_index(
    tmp_path: Path, monkeypatch
) -> None:
    """AC2-CON: the live pane PID is sufficient authority for late binding."""
    use_tmpdir(monkeypatch, tmp_path)
    session_id = "019fb024-2327-75f3-8b80-06e9d5ade05f"
    _seed_registry(
        dict(
            name="worker-codex",
            provider="codex",
            status="spawning",
            pid=4242,
            pid_start_time=123,
            mux={"session": "main", "pane_id": 7},
        ),
    )

    from fno.agents import dispatch
    from fno.agents import mux_spawn
    from fno.agents import spawn_gate
    from fno.agents.registry import load_registry

    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda pid, started: pid == 4242)
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *_args: True)
    monkeypatch.setattr(
        mux_spawn,
        "_codex_session_id_for_pid",
        lambda pid: session_id if pid == 4242 else None,
    )

    result = dispatch.reconcile_agents(
        codex_session_index_path=tmp_path / "missing-index.jsonl"
    )

    row = load_registry()[0]
    assert row.harness_session_id == session_id
    assert row.status == "live"
    assert result.backfilled == [
        {
            "name": "worker-codex",
            "provider": None,
            "harness": "codex",
            "harness_session_id": session_id,
        }
    ]


def test_reconcile_orphans_a_dead_claude_pane_that_already_restamped(
    tmp_path: Path, monkeypatch
) -> None:
    """The pane is the oracle for a pane row's whole life, not just before it has
    an id. Gating the arm on a missing id covered the row only until its worker
    restamped, and a pane that died after that was reachable by no arm at all."""
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-happy",
            provider="claude",
            status="live",
            harness_session_id="019fb024-2327-75f3-8b80-06e9d5ade05f",
            pid=4242,
            pid_start_time=123,
            mux={"session": "main", "pane_id": 7},
        ),
    )

    from fno.agents import dispatch, mux_spawn, spawn_gate
    from fno.agents.registry import load_registry

    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *_args: False)
    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda pid, started: False)

    dispatch.reconcile_agents()

    assert load_registry()[0].status == "orphaned"


def test_reconcile_keeps_live_unresolved_codex_pane_pending(
    tmp_path: Path, monkeypatch
) -> None:
    """AC3-FR: a live PID with no rollout stays pending, never orphaned."""
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-codex",
            provider="codex",
            status="spawning",
            pid=4242,
            pid_start_time=123,
            mux={"session": "main", "pane_id": 7},
        ),
    )

    from fno.agents import dispatch, mux_spawn, spawn_gate
    from fno.agents.registry import load_registry

    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda _pid, _started: True)
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *_args: True)
    monkeypatch.setattr(mux_spawn, "_codex_session_id_for_pid", lambda _pid: None)

    result = dispatch.reconcile_agents(
        codex_session_index_path=tmp_path / "missing-index.jsonl"
    )

    row = load_registry()[0]
    assert row.status == "spawning"
    assert row.harness_session_id is None
    assert result.orphaned == []
    assert result.backfilled == []
    assert result.errors[0]["reason"] == "codex-session-id-pending"


def test_reconcile_refuses_duplicate_across_two_rows_in_one_pass(
    tmp_path: Path, monkeypatch
) -> None:
    """AC5-CON: two id-less rows resolving to ONE rollout cannot both take it.

    Both rows read the other as ``harness_session_id=None``, so a duplicate check
    that only scans stored rows clears both and stamps the identity twice - the
    exact collision the guard exists to prevent.
    """
    use_tmpdir(monkeypatch, tmp_path)
    session_id = "019fb024-2327-75f3-8b80-06e9d5ade05f"
    _seed_registry(
        dict(name="late-a", provider="codex", status="spawning", pid=4242,
             pid_start_time=123, mux={"session": "main", "pane_id": 7}),
        dict(name="late-b", provider="codex", status="spawning", pid=4242,
             pid_start_time=123, mux={"session": "main", "pane_id": 7}),
    )

    from fno.agents import dispatch, mux_spawn, spawn_gate
    from fno.agents.registry import load_registry

    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda _pid, _started: True)
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *_args: True)
    monkeypatch.setattr(
        mux_spawn, "_codex_session_id_for_pid", lambda _pid: session_id
    )

    result = dispatch.reconcile_agents(
        codex_session_index_path=tmp_path / "missing-index.jsonl"
    )

    rows = {row.name: row for row in load_registry()}
    bound = [n for n, r in rows.items() if r.harness_session_id == session_id]
    assert len(bound) <= 1, f"one rollout stamped onto {bound}"
    loser = next(n for n in ("late-a", "late-b") if n not in bound)
    assert rows[loser].harness_session_id is None
    assert rows[loser].status == "spawning"
    assert len(result.backfilled) == len(bound)


def test_reconcile_refuses_unconfirmed_legacy_pid(
    tmp_path: Path, monkeypatch
) -> None:
    """A recorded pid with no incarnation token is never trusted on its own.

    Legacy rows (written before pid_start_time stamping) are the population this
    heal repairs, and for them ``_pid_alive`` degrades to bare existence - so a
    recycled pid could bind a stranger's rollout. The pane's real child must agree.
    """
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(name="legacy", provider="codex", status="spawning", pid=4242,
             mux={"session": "main", "pane_id": 7}),
    )

    from fno.agents import dispatch, mux_spawn, spawn_gate
    from fno.agents.registry import load_registry

    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda _pid, _started: True)
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *_args: True)
    # The pane's actual child is a DIFFERENT pid: 4242 was recycled.
    monkeypatch.setattr(mux_spawn, "_lookup_child_pid", lambda *_a, **_k: 9999)
    monkeypatch.setattr(
        mux_spawn,
        "_codex_session_id_for_pid",
        lambda _pid: "019fbfff-dead-7000-8000-000000000000",
    )

    result = dispatch.reconcile_agents(
        codex_session_index_path=tmp_path / "missing-index.jsonl"
    )

    row = load_registry()[0]
    assert row.harness_session_id is None, "a stranger's rollout was stamped"
    assert result.backfilled == []
    assert any(e["reason"] == "codex-pane-pid-unconfirmed" for e in result.errors)


def test_reconcile_never_claims_a_refused_claude_backfill(
    tmp_path: Path, monkeypatch
) -> None:
    """`backfilled` reports what the write STAMPED, not what it intended.

    The under-lock guard refuses a claude row whose short_id changed between probe
    and apply (rm + re-register under the same name). Claiming the heal at queue
    time reported a bound identity for a row whose harness_session_id is still
    null - the same lie this PR removes from the codex path.
    """
    use_tmpdir(monkeypatch, tmp_path)
    healed_uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    _seed_registry(
        dict(name="w1", provider="claude", status="live", short_id="11111111"),
    )

    from fno.agents import dispatch
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.registry import AgentEntry, load_registry

    monkeypatch.setattr(dispatch, "is_provider_available", lambda _p: True)
    monkeypatch.setattr(
        claude_mod, "claude_logs_reachable", lambda *_a, **_k: True
    )
    monkeypatch.setattr(claude_mod, "resolve_session_uuid", lambda _s: healed_uuid)

    real_update = dispatch.update_registry
    replaced = False

    def replace_then_apply(updater):
        nonlocal replaced
        if not replaced:
            replaced = True
            # Same name, different transport handle: the row we probed is gone.
            real_update(lambda rows: [
                AgentEntry(
                    name="w1", harness="claude", cwd="/tmp", log_path="",
                    short_id="99999999", status="live",
                )
            ])
        return real_update(updater)

    monkeypatch.setattr(dispatch, "update_registry", replace_then_apply)
    result = dispatch.reconcile_agents()

    row = next(r for r in load_registry() if r.name == "w1")
    assert row.harness_session_id is None, "a refused stamp still landed"
    assert not any(
        b["name"] == "w1" for b in result.backfilled
    ), f"claimed a heal that was refused: {result.backfilled}"
    assert any(
        e["reason"] == "claude-session-id-backfill-raced" for e in result.errors
    )


def test_reconcile_preserves_legacy_row_when_mux_liveness_is_unknown(
    tmp_path: Path, monkeypatch
) -> None:
    """An unavailable mux is not proof that a reused PID owns the old pane."""
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(name="late", provider="codex", status="spawning", pid=4242,
             mux={"session": "missing", "pane_id": 7}),
    )

    from fno.agents import dispatch, mux_spawn, spawn_gate
    from fno.agents.registry import load_registry

    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *_args, **_kwargs: None)
    monkeypatch.setattr(
        mux_spawn, "_codex_session_id_for_pid",
        lambda _pid: pytest.fail("unknown pane liveness must not donate a rollout"),
    )

    result = dispatch.reconcile_agents(
        codex_session_index_path=tmp_path / "missing-index.jsonl"
    )

    assert load_registry()[0].status == "spawning"
    assert any(e["reason"] == "mux-pane-liveness-unavailable" for e in result.errors)


def test_reconcile_never_backfills_terminal_codex_rows(
    tmp_path: Path, monkeypatch
) -> None:
    """AC4-ERR: terminal lifecycle truth wins over a late rollout."""
    status = "exited"
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-codex",
            provider="codex",
            status=status,
            pid=4242,
            mux={"session": "main", "pane_id": 7},
        ),
    )

    from fno.agents import dispatch, mux_spawn, spawn_gate
    from fno.agents.registry import load_registry

    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda _pid, _started: True)
    monkeypatch.setattr(
        mux_spawn,
        "_codex_session_id_for_pid",
        lambda _pid: pytest.fail("terminal row must not be probed"),
    )

    dispatch.reconcile_agents(codex_session_index_path=tmp_path / "missing-index.jsonl")

    row = load_registry()[0]
    assert row.status == status
    assert row.harness_session_id is None


def test_reconcile_orphans_a_pending_codex_pane_after_process_exit(
    tmp_path: Path, monkeypatch
) -> None:
    """AC4-ERR: process death keeps the existing orphan lifecycle authoritative."""
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-codex",
            provider="codex",
            status="spawning",
            pid=4242,
            pid_start_time=123,
            mux={"session": "main", "pane_id": 7},
        ),
    )

    from fno.agents import dispatch, mux_spawn, spawn_gate
    from fno.agents.registry import load_registry

    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda _pid, _started: False)
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *_args: True)
    monkeypatch.setattr(
        mux_spawn,
        "_codex_session_id_for_pid",
        lambda _pid: pytest.fail("an exited process must not be probed"),
    )

    result = dispatch.reconcile_agents(
        codex_session_index_path=tmp_path / "missing-index.jsonl"
    )

    row = load_registry()[0]
    assert row.status == "orphaned"
    assert row.harness_session_id is None
    assert result.orphaned == [
        {"name": "worker-codex", "provider": None, "harness": "codex", "id": None}
    ]


# ---------------------------------------------------------------------------
# Sigma-review follow-ups: tests for fixes landed in the review-fixes commit.
# ---------------------------------------------------------------------------


def test_reconcile_skips_claude_when_cli_missing(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """Sigma C1: missing claude CLI must NOT mass-orphan every claude agent.

    Mirrors AC3-EDGE for the codex side: when reachability cannot be
    probed, statuses stay untouched and the entries land in `errors`
    with a precise reason.
    """
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-claude",
            provider="claude",
            short_id="7c5dcf5d",
            status="live",
        ),
        dict(
            name="worker-claude-2",
            provider="claude",
            short_id="abcd1234",
            status="live",
        ),
    )
    # Force claude OFF PATH so is_provider_available returns False.
    monkeypatch.setenv("PATH", "/nonexistent")

    from fno.agents import dispatch
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.registry import load_registry

    probe_called = False

    def fake_probe(short_id, *, timeout=10.0):
        nonlocal probe_called
        probe_called = True
        return False

    monkeypatch.setattr(claude_mod, "claude_logs_reachable", fake_probe)

    result = dispatch.reconcile_agents()

    assert probe_called is False, (
        "claude_logs_reachable must NOT be called when claude is not on PATH"
    )
    assert len(result.errors) == 2
    assert all(
        e["reason"] == "claude-cli-not-on-path" for e in result.errors
    )
    # Statuses must stay 'live' — false mass-orphaning would be the C1 bug.
    for entry in load_registry():
        assert entry.status == "live"
    err = capsys.readouterr().err
    assert "claude CLI not on PATH" in err


def test_reconcile_preserves_claude_status_on_probe_error(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """Codex P1 round-5: transient claude probe failures (timeout / OSError)
    must NOT flip healthy agents to orphaned.

    Pre-fix, claude_logs_reachable returned False on timeout / OSError,
    and reconcile interpreted False as "supervisor lost session" →
    orphaned flip. A single slow probe could mass-orphan the fleet.

    Fix: claude_logs_reachable raises ReachabilityProbeError on
    inconclusive outcomes; reconcile catches and routes to errors
    with reason=claude-probe-failed, preserving status.
    """
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-claude",
            provider="claude",
            short_id="7c5dcf5d",
            status="live",
        ),
    )
    _force_claude_on_path(monkeypatch, tmp_path)

    from fno.agents import dispatch
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.harnesses.base import ReachabilityProbeError
    from fno.agents.registry import load_registry

    def boom(short_id, *, timeout=10.0):
        raise ReachabilityProbeError(
            provider="claude", reason="timeout after 10s"
        )

    monkeypatch.setattr(claude_mod, "claude_logs_reachable", boom)

    result = dispatch.reconcile_agents()

    # Routed to errors, NOT orphaned/recovered.
    assert len(result.orphaned) == 0
    assert len(result.recovered) == 0
    assert len(result.errors) == 1
    assert result.errors[0]["reason"].startswith("claude-probe-failed")
    # Status preserved.
    assert load_registry()[0].status == "live"


def test_reconcile_codex_index_stat_permission_does_not_abort(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """Codex P1 round-4: a PermissionError from session_index_exists must
    NOT propagate out of reconcile_agents.

    Pre-fix: Path.exists() can raise PermissionError when the parent
    directory is unreadable, which used to abort the whole reconcile
    call. Fix wraps the probe in OSError catch and treats the codex
    side as 'unreadable' — codex agents land in errors, non-codex
    agents still get reconciled.
    """
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-codex",
            provider="codex",
            codex_session_id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            status="live",
        ),
        dict(name="worker-gemini", provider="gemini",
             gemini_session_id="g-1"),
    )

    from fno.agents import dispatch
    from fno.agents.harnesses import codex as codex_mod
    from fno.agents.registry import load_registry

    def boom(*args, **kwargs):
        raise PermissionError("simulated: ~/.codex unreadable")

    monkeypatch.setattr(codex_mod, "session_index_exists", boom)

    # Should NOT raise — must classify and continue.
    result = dispatch.reconcile_agents()

    # Codex side: routed to errors with unreadable reason.
    codex_errors = [e for e in result.errors if e["harness"] == "codex"]
    assert len(codex_errors) == 1
    assert codex_errors[0]["reason"] == "codex-session-index-unreadable"
    # Status untouched.
    codex_entry = next(e for e in load_registry() if e.name == "worker-codex")
    assert codex_entry.status == "live"

    # Legacy Gemini rows are retained for read compatibility but their
    # retired adapter is not probed.
    gemini_errors = [e for e in result.errors if e["harness"] == "gemini"]
    assert len(gemini_errors) == 1
    assert gemini_errors[0]["reason"] == "retired-provider"

    err = capsys.readouterr().err
    assert "codex session index path unreadable" in err


def test_reconcile_never_trusts_an_uncorrelated_claude_pane_pid(
    tmp_path: Path, monkeypatch
) -> None:
    """A legacy row carries no incarnation token, so `_pid_alive` degrades to bare
    existence. A mux restart can hand the same (session, pane_id) to a different
    child while the recorded pid is recycled and alive; trusting that keeps the
    row live and points name-based delivery at a stranger."""
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name="worker-happy",
            provider="claude",
            status="live",
            pid=4242,
            mux={"session": "main", "pane_id": 7},
        ),
    )

    from fno.agents import dispatch, mux_spawn, spawn_gate
    from fno.agents.registry import load_registry

    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *_args: True)
    # The pane's real child is someone else; the recorded pid is a recycled one
    # that happens to still exist.
    monkeypatch.setattr(mux_spawn, "_lookup_child_pid", lambda *_a: 9999)
    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda pid, started: True)

    result = dispatch.reconcile_agents()

    assert load_registry()[0].status == "live", "status is preserved, not asserted"
    assert any(e["reason"] == "claude-pane-pid-unconfirmed" for e in result.errors)


def test_reconcile_normalizes_under_lock_duplicate_to_spawning(
    tmp_path: Path, monkeypatch
) -> None:
    """A rollout claimed after probing still cannot leave the loser live."""
    use_tmpdir(monkeypatch, tmp_path)
    session_id = "019fb024-2327-75f3-8b80-06e9d5ade05f"
    _seed_registry(
        dict(name="late", provider="codex", status="live", pid=4242,
             pid_start_time=123, mux={"session": "main", "pane_id": 7}),
    )

    from fno.agents import dispatch, mux_spawn, spawn_gate
    from fno.agents.registry import AgentEntry, load_registry

    monkeypatch.setattr(spawn_gate, "_pid_alive", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *_args: True)
    monkeypatch.setattr(mux_spawn, "_codex_session_id_for_pid", lambda _pid: session_id)
    real_update = dispatch.update_registry
    injected = False

    def claim_then_apply(updater):
        nonlocal injected
        if not injected:
            injected = True
            real_update(lambda rows: rows + [AgentEntry(
                name="owner", harness="codex", cwd="/tmp", log_path="",
                harness_session_id=session_id, status="live",
            )])
        return real_update(updater)

    monkeypatch.setattr(dispatch, "update_registry", claim_then_apply)
    result = dispatch.reconcile_agents(
        codex_session_index_path=tmp_path / "missing-index.jsonl"
    )

    late = next(row for row in load_registry() if row.name == "late")
    assert late.status == "spawning"
    assert late.harness_session_id is None
    assert any(e["reason"] == "codex-session-id-backfill-raced" for e in result.errors)
