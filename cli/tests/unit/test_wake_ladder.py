"""Wake-ladder rung 2: identity-preserving respawn.

wake_and_deliver revived an asleep session by forking a NEW incarnation
(dispatch_spawn --resume), even when the session was still rostered and
`claude respawn` would revive the SAME identity. Rung 2 revives an
exited-but-rostered session in place (respawn + re-inject); any miss falls
through to the fork rung so the mail is never dropped.
"""
from types import SimpleNamespace

import pytest

import fno.agents.dispatch as dispatch
from fno.agents.dispatch import wake_and_deliver


@pytest.fixture(autouse=True)
def _admitting_gate(monkeypatch):
    """Every revival asks the spawn gate now, routed or not, so an unrouted
    test that never stubbed the gate would reach the real Rust gate. A test's
    own ``run_gate`` monkeypatch wins over this one."""

    class _Guard:
        def retain_revived_worker(self, *a, **k):
            return None

        def release_gate_mutex(self):
            return None

        def release(self):
            return None

    monkeypatch.setattr(
        "fno.agents.spawn_gate.run_gate",
        lambda *args, **kwargs: _Guard(),
    )


def _entry(
    status,
    *,
    short="abc12345",
    name="wk-abc12345",
    sid="uuid-full",
    provider=None,
    route_settings_path=None,
    node=None,
    spawned_by=None,
):
    return SimpleNamespace(
        status=status,
        short_id=short,
        name=name,
        harness_session_id=sid,
        provider=provider,
        route_settings_path=route_settings_path,
        node=node,
        spawned_by_session=spawned_by[0] if spawned_by else None,
        spawned_by_harness=spawned_by[1] if spawned_by else None,
        spawned_by_cwd=spawned_by[2] if spawned_by else None,
    )


def _allow_rung2_claim(monkeypatch):
    """Stub the rung-2 single-writer guard so the revive path runs without touching
    the real claims substrate (F5)."""
    monkeypatch.setattr(dispatch, "_acquire_rung2_guard", lambda u, s: "revive:test")
    monkeypatch.setattr(dispatch, "_release_rung2_guard", lambda u, h: None)


def test_roster_exited_revives_in_place(monkeypatch):
    # AC1-HP: a rostered-exited session revives via respawn + re-inject, never forks.
    _allow_rung2_claim(monkeypatch)
    monkeypatch.setattr(dispatch, "_roster_entry_for_session", lambda u: _entry("exited"))
    monkeypatch.setattr(dispatch, "_respawn_claude_session", lambda s: 0)
    stamped = []
    monkeypatch.setattr(
        dispatch, "_stamp_revived_live", lambda entry: stamped.append(entry.name)
    )
    monkeypatch.setattr(dispatch, "_mail_inject_claude", lambda u, t, **_k: True)
    spawned = []
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **k: spawned.append(k) or SimpleNamespace(short_id="FORK"),
    )
    ok, detail = wake_and_deliver("uuid-full", "wake")
    assert ok is True
    assert detail == "abc12345"  # revived short_id, not a fork id
    assert spawned == []  # never forked - one roster row, same uuid
    assert stamped == ["wk-abc12345"]


def test_routed_respawn_acquires_provider_gate_before_side_effect(monkeypatch):
    _allow_rung2_claim(monkeypatch)
    monkeypatch.setattr(
        dispatch,
        "_roster_entry_for_session",
        lambda u: _entry(
            "exited", provider="zai", route_settings_path="/route.json"
        ),
    )
    events = []

    class _Gate:
        def retain_revived_worker(
            self,
            short,
            *,
            worker_name=None,
            worker_pid=None,
            positive_marker="claude-respawn-ok",
        ):
            events.append(
                ("retain", short, worker_name, worker_pid, positive_marker)
            )

        def release_gate_mutex(self):
            events.append("release-mutex")

        def release(self):
            events.append("release")

    monkeypatch.setattr(
        "fno.agents.spawn_gate.run_gate",
        lambda *args, **kwargs: events.append("gate") or _Gate(),
    )
    monkeypatch.setattr(
        dispatch,
        "_respawn_claude_session",
        lambda short: events.append("respawn") or 0,
    )
    monkeypatch.setattr(
        dispatch,
        "_stamp_revived_live",
        lambda entry: events.append("stamp-live"),
    )
    monkeypatch.setattr(
        dispatch,
        "_mail_inject_claude",
        lambda uuid, text, **kwargs: events.append("inject") or True,
    )
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **kwargs: (_ for _ in ()).throw(AssertionError("forked after revive")),
    )

    assert wake_and_deliver("uuid-full", "wake") == (True, "abc12345")
    assert events == [
        "gate",
        (
            "retain",
            "abc12345",
            "wk-abc12345",
            dispatch.os.getpid(),
            "claude-respawn-pending",
        ),
        "respawn",
        ("retain", "abc12345", "wk-abc12345", None, "claude-respawn-ok"),
        "stamp-live",
        "inject",
        "release-mutex",
    ]


def test_incomplete_forward_registry_refuses_wake(monkeypatch):
    from fno.agents.registry import LoadedRegistry

    monkeypatch.setattr(
        dispatch, "load_registry", lambda: LoadedRegistry([], complete=False)
    )
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **kwargs: (_ for _ in ()).throw(AssertionError("spawned from partial read")),
    )

    assert wake_and_deliver("uuid-full", "wake") == (
        False,
        "registry-incomplete",
    )


def test_unrostered_falls_through_to_fork(monkeypatch):
    # No roster row -> rung 3 fork (the existing identity-breaking path).
    monkeypatch.setattr(dispatch, "_roster_entry_for_session", lambda u: None)
    spawned = []
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **k: spawned.append(k) or SimpleNamespace(short_id="FORK"),
    )
    ok, detail = wake_and_deliver("uuid-full", "wake")
    assert ok is True and detail == "FORK"
    assert spawned and spawned[0]["resume_session_id"] == "uuid-full"
    assert spawned[0]["parent_edge"] == (None, None, None)


def test_respawn_ok_inject_miss_does_not_create_second_worker(monkeypatch):
    _allow_rung2_claim(monkeypatch)
    monkeypatch.setattr(dispatch, "_roster_entry_for_session", lambda u: _entry("exited"))
    monkeypatch.setattr(dispatch, "_respawn_claude_session", lambda s: 0)
    monkeypatch.setattr(dispatch, "_stamp_revived_live", lambda entry: None)
    monkeypatch.setattr(dispatch, "_mail_inject_claude", lambda u, t, **_k: False)
    monkeypatch.setattr(dispatch.time, "sleep", lambda s: None)  # no real waits
    spawned = []
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **k: spawned.append(k) or SimpleNamespace(short_id="FORK"),
    )
    ok, detail = wake_and_deliver("uuid-full", "wake")
    assert ok is False and detail == "respawn-inject-unconfirmed"
    assert spawned == []


def test_respawn_stamp_failure_stops_worker_before_releasing_admission(monkeypatch):
    from fno.agents.registry import RegistryVersionError

    _allow_rung2_claim(monkeypatch)
    monkeypatch.setattr(
        dispatch,
        "_roster_entry_for_session",
        lambda u: _entry(
            "exited", provider="zai", route_settings_path="/route.json"
        ),
    )
    events = []

    class _Gate:
        def retain_revived_worker(
            self,
            short,
            *,
            worker_name=None,
            worker_pid=None,
            positive_marker="claude-respawn-ok",
        ):
            events.append(
                ("retain", short, worker_name, worker_pid, positive_marker)
            )

        def release_gate_mutex(self):
            events.append("release-mutex")

        def release(self):
            events.append("release")

    monkeypatch.setattr(
        "fno.agents.spawn_gate.run_gate", lambda *args, **kwargs: _Gate()
    )
    monkeypatch.setattr(dispatch, "_respawn_claude_session", lambda s: 0)
    monkeypatch.setattr(
        dispatch,
        "_stamp_revived_live",
        lambda entry: (_ for _ in ()).throw(RegistryVersionError("changed")),
    )
    monkeypatch.setattr(
        "fno.agents.harnesses.claude.claude_stop",
        lambda short: events.append(("stop", short)) or (0, ""),
    )
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **kwargs: (_ for _ in ()).throw(AssertionError("forked")),
    )

    ok, detail = wake_and_deliver("uuid-full", "wake")

    assert ok is False and detail == "spawn-error-RuntimeError"
    assert events == [
        (
            "retain",
            "abc12345",
            "wk-abc12345",
            dispatch.os.getpid(),
            "claude-respawn-pending",
        ),
        ("retain", "abc12345", "wk-abc12345", None, "claude-respawn-ok"),
        ("stop", "abc12345"),
        "release",
    ]


def test_respawn_stop_failure_retains_provider_reservation(monkeypatch):
    from fno.agents.registry import RegistryVersionError

    _allow_rung2_claim(monkeypatch)
    monkeypatch.setattr(
        dispatch,
        "_roster_entry_for_session",
        lambda u: _entry(
            "exited", provider="zai", route_settings_path="/route.json"
        ),
    )
    events = []

    class _Gate:
        def retain_revived_worker(
            self,
            short,
            *,
            worker_name=None,
            worker_pid=None,
            positive_marker="claude-respawn-ok",
        ):
            events.append(
                ("retain", short, worker_name, worker_pid, positive_marker)
            )

        def release_gate_mutex(self):
            events.append("release-mutex")

        def release(self):
            events.append("release")

    monkeypatch.setattr(
        "fno.agents.spawn_gate.run_gate", lambda *args, **kwargs: _Gate()
    )
    monkeypatch.setattr(dispatch, "_respawn_claude_session", lambda s: 0)
    monkeypatch.setattr(
        dispatch,
        "_stamp_revived_live",
        lambda entry: (_ for _ in ()).throw(RegistryVersionError("changed")),
    )
    monkeypatch.setattr(
        "fno.agents.harnesses.claude.claude_stop",
        lambda short: (_ for _ in ()).throw(OSError("stop unavailable")),
    )
    monkeypatch.setattr(
        "fno.agents.harnesses._claude_session_registry.roster_sessions",
        lambda: [{"short_id": "abc12345", "pid": 4242}],
    )

    ok, detail = wake_and_deliver("uuid-full", "wake")

    assert ok is False and detail == "spawn-error-RuntimeError"
    assert events == [
        (
            "retain",
            "abc12345",
            "wk-abc12345",
            dispatch.os.getpid(),
            "claude-respawn-pending",
        ),
        ("retain", "abc12345", "wk-abc12345", 4242, "claude-respawn-ok"),
        "release-mutex",
    ]


def test_respawn_reservation_failure_releases_admission_without_respawn(monkeypatch):
    from fno.agents.spawn_gate import ProviderCountUnavailable

    _allow_rung2_claim(monkeypatch)
    monkeypatch.setattr(
        dispatch,
        "_roster_entry_for_session",
        lambda u: _entry(
            "exited", provider="zai", route_settings_path="/route.json"
        ),
    )
    events = []

    class _Gate:
        def retain_revived_worker(self, *_args, **_kwargs):
            events.append("retain")
            raise ProviderCountUnavailable("claim store unavailable")

        def release_gate_mutex(self):
            events.append("release-mutex")

        def release(self):
            events.append("release")

    monkeypatch.setattr(
        "fno.agents.spawn_gate.run_gate", lambda *args, **kwargs: _Gate()
    )
    monkeypatch.setattr(
        dispatch,
        "_respawn_claude_session",
        lambda _short: events.append("respawn") or 0,
    )

    ok, detail = wake_and_deliver("uuid-full", "wake")

    assert ok is False and detail == "spawn-error-ProviderCountUnavailable"
    assert events == ["retain", "release"]


def test_rung2_respawn_gate_refusal_never_respawns(monkeypatch):
    # AC6-ERR: a reentry refusal skips rung 2 and falls through to the fork
    # rung - never a respawn on an unverified route.
    import fno.agents.fork_lineage as fork_lineage

    monkeypatch.setattr(dispatch, "_roster_entry_for_session", lambda u: _entry("exited"))
    _allow_rung2_claim(monkeypatch)
    monkeypatch.setattr(
        fork_lineage, "spawn_axes_call", lambda p: {"refused": "session records no model"}
    )
    respawned = []
    monkeypatch.setattr(dispatch, "_respawn_claude_session", lambda s: respawned.append(s) or 0)
    spawned = []
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **k: spawned.append(k) or SimpleNamespace(short_id="FORK"),
    )
    ok, detail = wake_and_deliver("uuid-full", "wake")
    assert ok is True and detail == "FORK"
    assert respawned == []


def test_routed_fork_provider_refusal_launches_nothing(monkeypatch):
    from fno.agents.spawn_gate import GateRefused

    monkeypatch.setattr(
        dispatch,
        "_roster_entry_for_session",
        lambda u: _entry("live", provider="zai", route_settings_path="/route.json"),
    )
    monkeypatch.setattr(
        "fno.agents.spawn_gate.run_gate",
        lambda *args, **kwargs: (_ for _ in ()).throw(GateRefused(78)),
    )
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **kwargs: (_ for _ in ()).throw(AssertionError("spawned past cap")),
    )

    assert wake_and_deliver("uuid-full", "wake") == (False, "spawn-exit-78")


def test_rung2_claim_held_falls_through_to_fork(monkeypatch):
    # F5: a concurrent wake holds session:<uuid>; this caller must NOT respawn+
    # inject (double delivery) but fall through to the fork rung, which claims/pins.
    monkeypatch.setattr(dispatch, "_roster_entry_for_session", lambda u: _entry("exited"))
    monkeypatch.setattr(dispatch, "_acquire_rung2_guard", lambda u, s: None)  # held by other
    respawned = []
    monkeypatch.setattr(dispatch, "_respawn_claude_session", lambda s: respawned.append(s) or 0)
    spawned = []
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **k: spawned.append(k) or SimpleNamespace(short_id="FORK"),
    )
    ok, detail = wake_and_deliver("uuid-full", "wake")
    assert ok is True and detail == "FORK"
    assert respawned == []  # never respawned: the guard was held
    assert spawned and spawned[0]["resume_session_id"] == "uuid-full"
    # No registry name is recorded for this uuid (the stubbed row never hit
    # disk), so the uuid-derived wake- alias is the fallback (deterministic,
    # so wakes still serialize).
    from fno.harness_identity import canonical_handle

    assert spawned[0]["name"] == f"wake-{canonical_handle('uuid-full')}"


def test_resume_unpinned_refusal_reports_wake_unpinned(monkeypatch):
    # AC4-ERR: the resolver's refusal rides the receipt as wake-unpinned(...)
    # and never as wake-already-in-flight, though ResumeUnpinned carries exit 2.
    from fno.agents.fork_lineage import ResumeUnpinned

    _allow_rung2_claim(monkeypatch)
    monkeypatch.setattr(dispatch, "_roster_entry_for_session", lambda u: None)

    def _refuse(**k):
        raise ResumeUnpinned(
            "session uuid-full last ran glm-5.3-flash[1m], which only route zai "
            "serves, and this resume restores no route; resume it with: "
            "fno agents spawn --resume uuid-full -P zai -m 'glm-5.3-flash[1m]'",
            exit_code=2,
        )

    monkeypatch.setattr(dispatch, "dispatch_spawn", _refuse)
    ok, detail = wake_and_deliver("uuid-full", "wake")
    assert ok is False
    assert detail.startswith("wake-unpinned(")
    assert "-P zai" in detail


# every revival is gated and charged to the woken row's parent (x-a876) ------- #
def test_unrouted_exited_row_under_refusing_gate_never_respawns(monkeypatch):
    # AC1-ERR: an unrouted exited row asks the gate before rung 2; a refusal
    # stops the revival before any respawn or fork.
    from fno.agents.spawn_gate import GateRefused

    _allow_rung2_claim(monkeypatch)
    monkeypatch.setattr(dispatch, "_roster_entry_for_session", lambda u: _entry("exited"))
    monkeypatch.setattr(
        "fno.agents.spawn_gate.run_gate",
        lambda *args, **kwargs: (_ for _ in ()).throw(GateRefused(83)),
    )
    respawned = []
    monkeypatch.setattr(
        dispatch, "_respawn_claude_session", lambda s: respawned.append(s) or 0
    )
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **kwargs: (_ for _ in ()).throw(AssertionError("spawned past refusal")),
    )

    assert wake_and_deliver("uuid-full", "wake") == (False, "spawn-exit-83")
    assert respawned == []


def test_the_fork_carries_the_woken_rows_parent_not_the_sender(monkeypatch):
    # AC3-HP: a rung-3 fork is handed the woken row's parent triple, never the
    # sender's ambient identity.
    monkeypatch.setattr(
        dispatch,
        "_roster_entry_for_session",
        lambda u: _entry("live", spawned_by=("k1-parent", "claude", "/k1")),
    )
    spawned = []
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **k: spawned.append(k) or SimpleNamespace(short_id="FORK"),
    )

    ok, detail = wake_and_deliver("uuid-full", "wake")
    assert ok is True and detail == "FORK"
    assert spawned[0]["parent_edge"] == ("k1-parent", "claude", "/k1")
