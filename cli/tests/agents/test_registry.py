"""Tests for fno.agents.registry: the lock, the write door, the load gate and
the register path. One test per contract; plain per-field round trips are
covered by the schema-field coverage test and the lossless Rust-row round trip.
"""
from __future__ import annotations

import fcntl
import json
import multiprocessing as mp
import time
from dataclasses import asdict
from pathlib import Path
from types import SimpleNamespace

import pytest

from fno.paths_testing import use_tmpdir
from fno.registry_door import read_registry_document


def _replace_path_on_first_flock(monkeypatch, module, lock_path: Path):
    real_flock = module.fcntl.flock
    replaced = False

    def racing_flock(handle, operation):
        nonlocal replaced
        if not replaced and operation & fcntl.LOCK_EX:
            replaced = True
            lock_path.unlink()
            lock_path.touch()
        return real_flock(handle, operation)

    monkeypatch.setattr(module.fcntl, "flock", racing_flock)
    return real_flock


def _as_deployed(monkeypatch) -> None:
    """Run the write path as a DEPLOYED fno rather than a source checkout.

    ``_refuse_source_ahead_schema_bump`` refuses to raise the process-global
    registry's schema when this fno is running from a checkout, because that
    bump exists only on one branch while every deployed reader on the machine
    degrades. A test that writes an OLD on-disk version and asserts the write
    upgrades it is asserting the deployed-upgrade path (AC2), so it says so
    here rather than being exempted by an is-this-a-test predicate. Every other
    test in this file leaves the guard live.
    """
    from fno.agents import registry as reg

    monkeypatch.setattr(reg, "_running_from_source", lambda: None)


@pytest.mark.parametrize("timeout", [float("nan"), -0.1])
def test_registry_lock_rejects_nonterminating_timeout(tmp_path, timeout) -> None:
    from fno.agents.registry import _hold_registry_lock

    with pytest.raises(ValueError, match="finite and non-negative"):
        with _hold_registry_lock(tmp_path / "registry.json", timeout=timeout):
            pass


def test_registry_lock_revalidates_inode_after_path_replacement(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents import registry

    registry_path = tmp_path / "registry.json"
    lock_path = registry._registry_lock_path(registry_path)
    lock_path.parent.mkdir(parents=True)
    lock_path.touch()
    real_flock = _replace_path_on_first_flock(monkeypatch, registry, lock_path)

    with registry._hold_registry_lock(registry_path, timeout=None):
        with lock_path.open("a") as contender:
            with pytest.raises(BlockingIOError):
                real_flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)


def test_registry_lock_replacement_loop_honors_timeout(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents import registry

    registry_path = tmp_path / "registry.json"
    lock_path = registry._registry_lock_path(registry_path)
    lock_path.parent.mkdir(parents=True)
    lock_path.touch()
    real_flock = registry.fcntl.flock
    attempts = 0

    def replace_each_time(handle, operation):
        nonlocal attempts
        if operation & fcntl.LOCK_EX:
            attempts += 1
            if attempts > 2:
                raise AssertionError("inode replacement loop ignored timeout")
            lock_path.unlink()
            lock_path.touch()
        return real_flock(handle, operation)

    ticks = iter([0.0, 2.0])
    monkeypatch.setattr(registry.fcntl, "flock", replace_each_time)
    monkeypatch.setattr(
        registry,
        "time",
        SimpleNamespace(monotonic=lambda: next(ticks), sleep=lambda _seconds: None),
    )

    with pytest.raises(
        registry.RegistryLockTimeout, match=r"registry lock timeout after 1s"
    ):
        with registry._hold_registry_lock(registry_path, timeout=1):
            pass
    assert attempts == 1


def test_xdead_heal_own_cwd_stamps_the_working_directory(
    tmp_path: Path, monkeypatch
) -> None:
    """Task 0.1 (x-dead): the spawner mints the row with the SPAWN directory;
    the worker's own SessionStart heal moves the cwd field to the directory
    it actually works in, so every cwd-keyed occupancy join can find it."""
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import (
        AgentEntry,
        heal_own_cwd,
        load_registry,
        registry_rows_by_cwd,
        write_registry,
    )

    write_registry(
        [
            AgentEntry(
                name="t-b7f8-worker",
                harness="claude",
                cwd="/Users/bb16/code/footnote/footnote",
                short_id="deadbeef",
                harness_session_id="aaaaaaaa-1111-7222-8333-4444deadbeef",
                log_path="/tmp/t-b7f8-worker.log",
            )
        ]
    )

    moved = heal_own_cwd(
        name="t-b7f8-worker",
        harness="claude",
        cwd="/Users/bb16/.fno/worktrees/footnote/x-b7f8",
    )
    assert moved == (
        "/Users/bb16/code/footnote/footnote",
        "/Users/bb16/.fno/worktrees/footnote/x-b7f8",
    )
    by_cwd, ok = registry_rows_by_cwd()
    assert ok is True
    assert list(by_cwd) == ["/Users/bb16/.fno/worktrees/footnote/x-b7f8"]
    assert load_registry()[0].cwd == "/Users/bb16/.fno/worktrees/footnote/x-b7f8"
    # Idempotent: a second SessionStart writes nothing and answers None.
    assert (
        heal_own_cwd(
            name="t-b7f8-worker",
            harness="claude",
            cwd="/Users/bb16/.fno/worktrees/footnote/x-b7f8",
        )
        is None
    )
    # No such row: a no-op, never a raise.
    assert (
        heal_own_cwd(name="nobody", harness="claude", cwd="/somewhere") is None
    )


def test_ac1_hp_round_trip_entry(tmp_path: Path, monkeypatch) -> None:
    """AC1-HP: write + read back a single agent entry preserving all fields."""
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import AgentEntry, load_registry, write_registry

    entry = AgentEntry(
        name="my-agent",
        harness="claude",
        cwd="/home/user/project",
        short_id="abc123",
        harness_session_id=None,
        log_path="/tmp/my-agent.log",
        context_used_pct=26,
        context_used_tokens=258_687,
        context_window_tokens=1_000_000,
        context_measured_at="2026-09-30T12:00:00Z",
        mail_unread=1,
    )

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    write_registry([entry], path=registry_path)

    loaded = load_registry(path=registry_path)
    assert len(loaded) == 1
    e = loaded[0]
    assert e.name == "my-agent"
    assert e.harness == "claude"
    assert e.cwd == "/home/user/project"
    assert e.short_id == "abc123"
    assert e.harness_session_id is None
    assert e.log_path == "/tmp/my-agent.log"
    assert e.context_used_pct == 26
    assert e.context_used_tokens == 258_687
    assert e.context_window_tokens == 1_000_000
    assert e.context_measured_at == "2026-09-30T12:00:00Z"
    assert e.mail_unread == 1
    # AC1-HP: model provider is explicit and unset; removed session aliases die.
    raw_row = read_registry_document(registry_path)[0]["agents"][0]
    assert raw_row["provider"] is None
    for dead in ("codex_session_id", "gemini_session_id", "claude_session_uuid"):
        assert dead not in raw_row
    # created_at must be ISO8601 UTC
    assert e.created_at.endswith("Z") or "+" in e.created_at


def test_ac2_err_atomic_write_on_exception(tmp_path: Path, monkeypatch) -> None:
    """AC2-ERR: exception mid-write leaves prior file intact (no corruption)."""
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import AgentEntry, load_registry, write_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)

    # Write an initial valid registry
    initial_entry = AgentEntry(
        name="safe-agent",
        harness="claude",
        cwd="/tmp",
        log_path="/tmp/safe.log",
    )
    write_registry([initial_entry], path=registry_path)
    original_content = read_registry_document(registry_path)[0]

    # Now simulate a write that dies inside the table door.
    import fno.registry_door as door_module

    def _exploding_commit(*args, **kwargs):
        raise RuntimeError("simulated kill -9 mid-write")

    monkeypatch.setattr(door_module, "commit_registry_document", _exploding_commit)

    new_entry = AgentEntry(
        name="corrupt-agent",
        harness="codex",
        cwd="/tmp",
        log_path="/tmp/corrupt.log",
    )
    with pytest.raises(RuntimeError, match="simulated kill -9"):
        write_registry([new_entry], path=registry_path)

    # The table must be intact
    assert read_registry_document(registry_path)[0] == original_content
    loaded = load_registry(path=registry_path)
    assert loaded[0].name == "safe-agent"


_CONFLICT_JSON = (
    '{"status":"refused","reason":"revision_conflict",'
    '"message":"registry changed since read. Reload before applying the mutation again."}'
)


def _seed_registry(registry_path: Path) -> None:
    from fno.agents.registry import AgentEntry, write_registry

    write_registry(
        [AgentEntry(name="resident", harness="claude", cwd="/tmp", log_path="/tmp/r.log")],
        path=registry_path,
    )


def test_update_registry_retries_revision_conflict_and_keeps_concurrent_row(
    tmp_path: Path, monkeypatch
) -> None:
    """A busy lock, then a conflict, then the retry lands with every row present."""
    use_tmpdir(monkeypatch, tmp_path)

    import fno.rust_binary as rb
    from fno.agents.registry import AgentEntry, load_registry, update_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    _seed_registry(registry_path)

    real_verb_call = rb.verb_call
    commits = {"n": 0, "applies": 0}

    def _conflict_once(verb, payload, **kwargs):
        if verb == "registry-commit" and "revision" in payload:
            commits["n"] += 1
            if commits["n"] == 1:
                raise rb.VerbUnavailable(
                    "registry-commit: lock at /x/graph.json.lock stayed busy past the 10s "
                    "deadline; no holder recorded in the lock file"
                )
            if commits["n"] == 2:
                raise rb.VerbUnavailable(_CONFLICT_JSON)
        return real_verb_call(verb, payload, **kwargs)

    monkeypatch.setattr(rb, "verb_call", _conflict_once)

    def _append_spawned(entries):
        commits["applies"] += 1
        return entries + [
            AgentEntry(name="spawned", harness="codex", cwd="/tmp", log_path="/tmp/s.log")
        ]

    update_registry(_append_spawned, path=registry_path)

    assert [e.name for e in load_registry(path=registry_path)] == ["resident", "spawned"]
    assert commits["n"] == 3
    assert commits["applies"] == 3


def test_update_registry_reapplies_when_a_writer_lands_between_load_and_commit(
    tmp_path: Path, monkeypatch
) -> None:
    """The guard covers the load, not just the commit's own read: a row a
    non-flock writer adds after the load survives, never refused as row loss."""
    use_tmpdir(monkeypatch, tmp_path)

    import fno.rust_binary as rb
    from fno.agents.registry import AgentEntry, load_registry, update_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    _seed_registry(registry_path)

    real_verb_call = rb.verb_call
    concurrent = AgentEntry(name="concurrent", harness="claude", cwd="/tmp", log_path="/tmp/c.log")
    landed = {"done": False}

    def _writer_lands_after_first_read(verb, payload, **kwargs):
        answer = real_verb_call(verb, payload, **kwargs)
        if payload.get("op") == "read" and not landed["done"]:
            landed["done"] = True
            doc = answer["document"]
            rows = doc["agents"] + [asdict(concurrent)]
            real_verb_call(
                verb,
                {"path": payload["path"], "schema_version": doc["schema_version"],
                 "agents": rows, "revision": answer["revision"]},
            )
        return answer

    monkeypatch.setattr(rb, "verb_call", _writer_lands_after_first_read)

    update_registry(
        lambda entries: entries
        + [AgentEntry(name="spawned", harness="codex", cwd="/tmp", log_path="/tmp/s.log")],
        path=registry_path,
    )

    assert [e.name for e in load_registry(path=registry_path)] == [
        "resident", "concurrent", "spawned"
    ]


def test_update_registry_conflict_exhaustion_and_non_conflict_single_shot(
    tmp_path: Path, monkeypatch
) -> None:
    """Conflicts retry until the deadline, past any fixed count, then keep
    RegistryVersionError; a non-conflict refusal never retries."""
    use_tmpdir(monkeypatch, tmp_path)

    import fno.agents.registry as reg
    import fno.rust_binary as rb
    from fno.agents.registry import AgentEntry, RegistryVersionError, update_registry

    clock = {"now": 0.0}
    fake_time = SimpleNamespace(**{k: getattr(time, k) for k in dir(time) if k[0] != "_"})
    fake_time.monotonic = lambda: clock["now"]
    fake_time.sleep = lambda s: clock.__setitem__("now", clock["now"] + s)
    monkeypatch.setattr(reg, "time", fake_time)
    monkeypatch.setattr(reg, "_CONFLICT_RETRY_SECONDS", 1.0)

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    _seed_registry(registry_path)

    real_verb_call = rb.verb_call
    commits = {"n": 0}

    def _always_conflict(verb, payload, **kwargs):
        if verb == "registry-commit" and "revision" in payload:
            commits["n"] += 1
            raise rb.VerbUnavailable(_CONFLICT_JSON)
        return real_verb_call(verb, payload, **kwargs)

    monkeypatch.setattr(rb, "verb_call", _always_conflict)

    def _append(entries):
        return entries + [
            AgentEntry(name="spawned", harness="codex", cwd="/tmp", log_path="/tmp/s.log")
        ]

    with pytest.raises(RegistryVersionError, match="revision_conflict"):
        update_registry(_append, path=registry_path)
    assert commits["n"] > 3
    assert clock["now"] >= 1.0

    commits["n"] = 0

    def _schema_refused(verb, payload, **kwargs):
        if verb == "registry-commit" and "revision" in payload:
            commits["n"] += 1
            raise rb.VerbUnavailable('{"status":"refused","reason":"schema_refused"}')
        return real_verb_call(verb, payload, **kwargs)

    monkeypatch.setattr(rb, "verb_call", _schema_refused)
    with pytest.raises(RegistryVersionError, match="schema_refused"):
        update_registry(_append, path=registry_path)
    assert commits["n"] == 1


def test_forward_read_marks_a_skipped_row_incomplete(
    tmp_path: Path, monkeypatch
) -> None:
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import SCHEMA_VERSION, load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": SCHEMA_VERSION + 1,
                "agents": [
                    {
                        "name": "kept",
                        "harness": "claude",
                        "cwd": "/tmp",
                        "log_path": "/tmp/kept.log",
                    },
                    {
                        "name": "skipped",
                        "harness": "claude",
                        "cwd": "/tmp",
                        "log_path": "/tmp/skipped.log",
                        "status": "future-state",
                    },
                ],
            }
        ),
        encoding="utf-8",
    )

    loaded = load_registry(path=registry_path)
    assert [row.name for row in loaded] == ["kept"]
    assert loaded.complete is False


def test_ac4_err_write_over_a_newer_store_still_raises_naming_versions(
    tmp_path: Path, monkeypatch
) -> None:
    """The refusal survives on the WRITE path, which is where it now belongs."""
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import SCHEMA_VERSION, RegistryVersionError, write_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps({"schema_version": 99, "agents": []}), encoding="utf-8"
    )

    with pytest.raises(RegistryVersionError, match=r"99") as exc_info:
        write_registry([], path=registry_path)

    assert f"schema_version={SCHEMA_VERSION}" in str(exc_info.value)


def test_x8dfc_unknown_provider_loads_undispatchable(tmp_path: Path, monkeypatch) -> None:
    """x-8dfc: a provider outside the dispatch roster no longer bricks the read.

    Pre-x-8dfc a typo'd provider raised RegistryVersionError, bricking the
    whole shared read. Now identity is a shape check: the row loads as an
    undispatchable identity row (mail-routable), and capability is refused
    later at the spawn/ask seam, not at load.
    """
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.dispatch import _check_known_provider
    from fno.agents.registry import load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "agents": [
                    {
                        "name": "rogue",
                        "provider": "calude",  # typo — masquerades as claude
                        "cwd": "/tmp",
                        "log_path": "/tmp/r.log",
                        "claude_short_id": None,
                        "codex_session_id": None,
                        "gemini_session_id": None,
                        "created_at": "2026-05-19T00:00:00Z",
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    entries = load_registry(path=registry_path)
    assert len(entries) == 1
    assert entries[0].harness == "calude"
    # ...but it is NOT dispatchable: the spawn/ask seam still refuses it.
    with pytest.raises(ValueError, match="calude"):
        _check_known_provider("calude")


def test_ac4_err_malformed_row_shape_rejected(tmp_path: Path, monkeypatch) -> None:
    """A row with unknown fields (future-schema drift) is rejected loudly, not silently."""
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import RegistryVersionError, load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "agents": [
                    {
                        "name": "ahead-of-time",
                        "provider": "claude",
                        "cwd": "/tmp",
                        "log_path": "/tmp/x.log",
                        # Field that doesn't exist on AgentEntry yet —
                        # mimics a future fno adding metadata without
                        # bumping schema_version.
                        "supervisor_pgid": 1234,
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(RegistryVersionError, match="malformed shape"):
        load_registry(path=registry_path)


def test_load_registry_rejects_invalid_json(tmp_path: Path, monkeypatch) -> None:
    """Invalid JSON surfaces as RegistryVersionError, not raw JSONDecodeError."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import RegistryVersionError, load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text("not even {valid", encoding="utf-8")

    with pytest.raises(RegistryVersionError):
        load_registry(path=registry_path)


def _concurrent_update_worker(worker_id: int, registry_path_str: str) -> None:
    """Module-scope worker for the concurrent-update test.

    Lives at module scope so multiprocessing can pickle it under both
    fork (Linux default ≤3.13) and spawn (macOS / Linux 3.14+) start
    methods. A test-local closure cannot be pickled under spawn.
    """
    from pathlib import Path as P

    from fno.agents.registry import AgentEntry, update_registry

    def add_entry(entries):
        entries.append(
            AgentEntry(
                name=f"worker-{worker_id}",
                harness="claude",
                cwd="/tmp",
                log_path=f"/tmp/w{worker_id}.log",
            )
        )
        return entries

    update_registry(add_entry, path=P(registry_path_str))


def test_update_registry_serializes_different_name_writes(
    tmp_path: Path, monkeypatch
) -> None:
    """Two concurrent update_registry calls for DIFFERENT agents don't lose updates.

    Codex review on PR #288 (P1): without a registry-wide lock, two ask
    calls for different names can both ``load_registry`` -> mutate ->
    ``write_registry`` and the loser's update is silently dropped. This
    test spawns N parallel workers; if the global lock holds, all N
    entries survive in the final registry.
    """
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"

    n_workers = 5
    procs = [
        mp.Process(
            target=_concurrent_update_worker,
            args=(i, str(registry_path)),
        )
        for i in range(n_workers)
    ]
    for p in procs:
        p.start()
    for p in procs:
        p.join(timeout=15)
        assert p.exitcode == 0, f"worker exited {p.exitcode}"

    loaded = load_registry(path=registry_path)
    names = {e.name for e in loaded}
    assert names == {f"worker-{i}" for i in range(n_workers)}, (
        f"expected {n_workers} distinct worker entries; got {names}"
    )


def test_session_transition_classifies_liveness_truth() -> None:
    from fno.agents.registry import classify_session_transition

    assert classify_session_transition("session-a", "session-b", False) == "succession"
    assert classify_session_transition("session-a", "session-b", True) == "branch"
    assert classify_session_transition("session-a", "session-b", None) == "deferred"
    assert classify_session_transition("", "session-b", False) == "deferred"


def test_v14_provider_still_backfills_harness_without_inventing_model_provider(
    tmp_path: Path, monkeypatch
) -> None:
    """Before v15, provider was the retired harness spelling and remains read-only."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    legacy_harness = "co" + "dex"
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": 14,
                "agents": [
                    {
                        "name": "legacy",
                        "provider": legacy_harness,
                        "cwd": "/tmp",
                        "log_path": "/tmp/legacy.log",
                        "status": "live",
                    }
                ],
            }
        )
    )

    loaded = load_registry(path=registry_path)
    assert loaded[0].harness == legacy_harness
    assert loaded[0].provider is None


def test_load_registry_accepts_all_projected_statuses(tmp_path: Path, monkeypatch) -> None:
    """registry.status is a projection of state.status, so every AgentStatus
    value the daemon can write must read back cleanly. In particular `exited`:
    the daemon writes it on child exit and retains the row until rm. The old
    {live, orphaned} guard hard-errored on `exited`, bricking every Python
    `fno agents` command until the row was rm'd (ab-3c063856 grid testing)."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    for status in (
        "spawning",
        "ready",
        "idle",
        "busy",
        "live",
        "restarting",
        "orphaned",
        "failed",
        "exited",
        "permanent_dead",
    ):
        registry_path = tmp_path / status / "registry.json"
        registry_path.parent.mkdir()
        registry_path.write_text(
            json.dumps(
                {
                    "schema_version": 3,
                    "agents": [
                        {
                            "name": "a",
                            "provider": "codex",
                            "cwd": "/tmp",
                            "log_path": "/tmp/a.log",
                            "status": status,
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        loaded = load_registry(path=registry_path)
        assert len(loaded) == 1, f"status {status!r} should load"
        assert loaded[0].status == status


def test_load_registry_still_rejects_garbage_status(tmp_path: Path, monkeypatch) -> None:
    """Widening KNOWN_STATUSES to the full AgentStatus set must not weaken the
    guard against genuinely-invalid values."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import RegistryVersionError, load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": 3,
                "agents": [
                    {
                        "name": "a",
                        "provider": "codex",
                        "cwd": "/tmp",
                        "log_path": "/tmp/a.log",
                        "status": "zombie",
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(RegistryVersionError, match="zombie"):
        load_registry(path=registry_path)


def test_ab_a171ceb2_v4_reads_host_mode_and_keeps_back_compat(
    tmp_path: Path, monkeypatch
) -> None:
    """The v4 host_mode forward-compat bump reads cleanly with host_mode
    preserved, and the widened accepted range still reads v1..=v4 (the bump
    must not drop back-compat reads; ab-a171ceb2)."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)

    def row(host_mode=None):
        r = {
            "name": "a",
            "provider": "codex",
            "cwd": "/tmp",
            "log_path": "/tmp/a.log",
            "status": "live",
        }
        if host_mode is not None:
            r["host_mode"] = host_mode
        return r

    # v4 round-trips an explicit interactive host_mode. Each version seeds its
    # own store: the table imports a legacy file once.
    registry_path = tmp_path / "v4" / "registry.json"
    registry_path.parent.mkdir()
    registry_path.write_text(
        json.dumps({"schema_version": 4, "agents": [row("interactive")]}),
        encoding="utf-8",
    )
    loaded = load_registry(path=registry_path)
    assert len(loaded) == 1
    assert loaded[0].host_mode == "interactive"

    # Every version in the widened accepted range still loads (no v1 drop),
    # and an absent host_mode coerces to exec regardless of version.
    for v in (1, 2, 3, 4):
        registry_path = tmp_path / f"v{v}-bare" / "registry.json"
        registry_path.parent.mkdir()
        registry_path.write_text(
            json.dumps({"schema_version": v, "agents": [row()]}), encoding="utf-8"
        )
        loaded = load_registry(path=registry_path)
        assert len(loaded) == 1, f"v{v} must still read after the bump"
        assert loaded[0].host_mode == "exec", f"v{v} absent host_mode => exec"


def test_us2_first_write_upgrades_on_disk_to_current(tmp_path: Path, monkeypatch) -> None:
    """A v1 file rewritten via write_registry persists as the current schema_version.

    Phase 5 bumped SCHEMA_VERSION to 3; v1 upgrades pass through to v3
    on the next write_registry, synthesizing both status+last_message_at
    (v2 additions) and mcp_channel_id (v3 addition).
    """
    use_tmpdir(monkeypatch, tmp_path)
    _as_deployed(monkeypatch)
    from fno.agents.registry import SCHEMA_VERSION, load_registry, write_registry

    registry_path = tmp_path / "agents-fixture" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "agents": [
                    {
                        "name": "upgraded",
                        "provider": "claude",
                        "cwd": "/tmp",
                        "log_path": "/tmp/u.log",
                        "claude_short_id": None,
                        "codex_session_id": None,
                        "gemini_session_id": None,
                        "created_at": "2026-05-19T00:00:00Z",
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    loaded = load_registry(path=registry_path)
    write_registry(loaded, path=registry_path)

    raw = read_registry_document(registry_path)[0]
    assert raw["schema_version"] == SCHEMA_VERSION  # 4 today
    assert raw["agents"][0]["status"] == "live"
    assert raw["agents"][0]["last_message_at"] is None
    assert raw["agents"][0]["mcp_channel_id"] is None


def test_us2_v1_corrupt_identity_still_rejected(tmp_path: Path, monkeypatch) -> None:
    """v1 synthesis MUST NOT swallow content validation: a structurally corrupt
    row still raises. Post-x-8dfc the identity check is a shape check (an alien
    provider now loads, tested in test_load_gate_x8dfc), so the surviving guard
    is corruption -- an empty identity with no harness -- which must still raise
    even under v1 synthesis."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import RegistryVersionError, load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "agents": [
                    {
                        "name": "bad",
                        "provider": "",  # corrupt: empty identity, no harness
                        "cwd": "/tmp",
                        "log_path": "/tmp/b.log",
                        "claude_short_id": None,
                        "codex_session_id": None,
                        "gemini_session_id": None,
                        "created_at": "2026-05-19T00:00:00Z",
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(RegistryVersionError, match="no valid identity"):
        load_registry(path=registry_path)


def test_agent_entry_accepts_every_field_in_registry_schema() -> None:
    """Every field the Rust writer declares in registry_schema.toml loads into AgentEntry."""
    import dataclasses
    import tomllib

    import fno.agents
    from fno.agents.registry import AgentEntry

    schema = Path(fno.agents.__file__).parent / "registry_schema.toml"
    declared = set(tomllib.loads(schema.read_text())["fields"])
    # Read-time legacy keys: load_registry folds or drops these before building the entry.
    legacy = {"claude_session_uuid", "claude_short_id", "codex_session_id",
              "gemini_session_id", "legacy_provider", "session_id"}
    missing = declared - legacy - {f.name for f in dataclasses.fields(AgentEntry)}
    assert not missing, f"AgentEntry lacks registry_schema.toml fields: {sorted(missing)}"


def test_every_resumable_harness_resolves_an_identity() -> None:
    """Every harness that DECLARES resume support must resolve a resume id.

    The lower bound is the capability contract, because that is what the
    resume verb's own support check reads. The bound this replaces was
    KNOWN_PROVIDERS, the Python dispatch set, which is ("claude", "codex") -
    so pi, gemini, agy and opencode all passed it vacuously and it could not
    fail when a harness joined the contract without joining the map (x-efd7).

    The live specimen: pi declared interactive_resume support in the
    contract while nothing taught the identity read about it, so
    `fno agents resume` answered "supported" and then "no recorded
    session_id for harness pi" about one row, which `fno agents attach`
    opened fine off harness_session_id.

    The row shape asserted here is the one every non-claude row actually
    has: a canonical harness_session_id and no transport key.

    The upper bound stays READABLE_PROVIDERS so a typo'd harness still fails.
    """
    from fno.agents.harness_map import DispatchResolveError, capabilities, known_harnesses
    from fno.agents.harnesses import READABLE_PROVIDERS
    from fno.agents.registry import HARNESS_SESSION_ID_FIELDS, AgentEntry
    from fno.agents.resume_cli import _session_id_for
    from fno.harness_names import known_providers

    checked = []
    for harness in known_harnesses():
        try:
            form = capabilities(harness)["resume_strategy"]["forms"]["interactive_resume"]
        except (DispatchResolveError, KeyError, TypeError):
            continue
        if form["kind"] == "unsupported":
            continue
        entry = AgentEntry(
            name=f"{harness}-row",
            harness=harness,
            cwd="/x",
            log_path="/tmp/x.log",
            harness_session_id="canonical-id-1",
        )
        assert entry.session_id == "canonical-id-1", (
            f"harness {harness!r} declares interactive_resume support but its "
            f"identity read returns nothing for a harness_session_id-only row"
        )
        assert _session_id_for(entry) == "canonical-id-1", harness
        # The gate is the Rust roster now, not the field map: a resumable
        # harness must be roster-known, or register_session raises on it and
        # discovery skips its rows. The field itself is derived: claude is the
        # one exception (its jobId transport key), everything else rides the
        # harness_session_id fallback the two reads above just exercised.
        assert harness in known_providers(), (
            f"harness {harness!r} declares interactive_resume support but is "
            f"absent from the Rust roster, so register_session raises on it "
            f"and discovery skips its rows"
        )
        expected_field = "short_id" if harness == "claude" else "harness_session_id"
        actual_field = HARNESS_SESSION_ID_FIELDS.get(harness) or "harness_session_id"
        assert actual_field == expected_field, (
            f"harness {harness!r} resolves {actual_field!r}, expected {expected_field!r}"
        )
        checked.append(harness)

    # A positive marker, not an absence. Without this the assertion body could
    # be skipped entirely by a contract that declared nothing resumable, which
    # is the exact shape of vacuous pass this test was written to end. The bar
    # proves the LOOP RAN and nothing more, so it stays well under the six
    # harnesses that declare support today: pinning it near that count would
    # turn one legitimate retirement into a failure here, and a gate that cries
    # about roster churn is one people learn to edit.
    assert len(checked) >= 2, f"the resumable-harness loop never ran; saw {checked}"

    assert set(HARNESS_SESSION_ID_FIELDS) <= set(READABLE_PROVIDERS)


def test_host_mode_alien_value_rejected(tmp_path: Path, monkeypatch) -> None:
    """An alien non-null host_mode (typo, wrong type) is rejected like an alien
    status, not silently coerced -- defense-in-depth (sigma-review)."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import RegistryVersionError, load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": 3,
                "agents": [
                    {
                        "name": "typo",
                        "provider": "codex",
                        "cwd": "/tmp",
                        "log_path": "/tmp/typo.log",
                        "status": "live",
                        "host_mode": "intractive",
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(RegistryVersionError):
        load_registry(path=registry_path)


def _rust_pty_row(name: str = "worker-claude", **overrides) -> dict:
    """A registry row exactly as the Rust daemon serializes a live PTY agent:
    non-empty short_id/project_root + pid + worker socket, etc. Before the fix,
    AgentEntry(**row) raised TypeError on `short_id`, bricking every Python read.
    """
    row = {
        "name": name,
        "short_id": "wk-abc123",
        "harness": "claude",
        "cwd": "/Users/x/proj",
        "project_root": "/Users/x/proj",
        "messaging_socket_path": "/tmp/fno/sock/wk-abc123.sock",
        "status": "live",
        "created_at": "2026-05-26T00:00:00Z",
        "pid": 4242,
        "pid_start_time": 123456789,
        "cc_session_id": "cc-xyz",
        "last_reconciled_at": "2026-05-26T01:00:00Z",
        "log_path": "/Users/x/.fno/agents/worker-claude.log",
    }
    row.update(overrides)
    return row


def _write_raw(registry_path: Path, rows: list[dict]) -> None:
    from fno.agents.registry import SCHEMA_VERSION

    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps({"schema_version": SCHEMA_VERSION, "agents": rows}),
        encoding="utf-8",
    )


def test_rust_pty_row_with_stored_session_id_is_not_a_brick(
    tmp_path: Path, monkeypatch
) -> None:
    """A row that also serializes `session_id` (a computed @property on the
    Python side) loads -- the key is dropped, and the property recomputes the
    same projection from the provider's session-id field."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    _write_raw(registry_path, [_rust_pty_row(session_id="abc123")])

    loaded = load_registry(path=registry_path)  # must not raise
    # session_id is the claude projection of short_id (v9 unified transport key).
    assert loaded[0].session_id == "wk-abc123"


def test_rust_pty_row_roundtrips_losslessly(tmp_path: Path, monkeypatch) -> None:
    """load -> write -> reload preserves the Rust-only PTY fields (no data loss
    when Python rewrites a registry that contains a Rust row)."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import load_registry, write_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    _write_raw(registry_path, [_rust_pty_row()])

    loaded = load_registry(path=registry_path)
    write_registry(loaded, path=registry_path)  # Python rewrites the store
    reloaded = load_registry(path=registry_path)

    e = reloaded[0]
    assert e.short_id == "wk-abc123"
    assert e.project_root == "/Users/x/proj"
    assert e.pid == 4242
    assert e.pid_start_time == 123456789
    assert e.cc_session_id == "cc-xyz"
    assert e.last_reconciled_at == "2026-05-26T01:00:00Z"


def test_python_write_emits_rust_readable_values(tmp_path: Path, monkeypatch) -> None:
    """A Python-authored row must serialize short_id/project_root as the EMPTY
    STRING (never null) so the Rust `String` fields deserialize it, and the
    Option fields as null. This is the load-bearing cross-language contract."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, write_registry

    entry = AgentEntry(
        name="py-ask",
        harness="codex",
        cwd="/p",
        log_path="/l",
        harness_session_id="sid",
    )
    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    write_registry([entry], path=registry_path)

    raw = read_registry_document(registry_path)[0]
    row = raw["agents"][0]
    # Rust `String` fields: empty string, NOT null (null would fail deserialize).
    assert row["short_id"] == ""
    assert row["project_root"] == ""
    # Rust `Option` fields: null is fine (reads as None).
    assert row["pid"] is None
    assert row["pid_start_time"] is None
    assert row["cc_session_id"] is None
    # `session_id` is a @property, never serialized as a stored field.
    assert "session_id" not in row


def test_write_registry_rejects_double_ref_rows(tmp_path: Path, monkeypatch) -> None:
    """One live ref per row (brief Locked 7): a mux ref alongside a non-empty
    short_id (a worker key or, since v9, a bg jobId) is refused at write time,
    and the prior store is left intact."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, write_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    write_registry(
        [AgentEntry(name="ok", harness="claude", cwd="/p", log_path="/l")],
        path=registry_path,
    )

    worker_double = AgentEntry(
        name="w",
        harness="codex",
        cwd="/p",
        log_path="/l",
        short_id="wk-1",
        mux={"session": "main", "pane_id": 1},
    )
    with pytest.raises(ValueError, match="one live ref"):
        write_registry([worker_double], path=registry_path)

    bg_double = AgentEntry(
        name="b",
        harness="claude",
        cwd="/p",
        log_path="/l",
        short_id="abcd1234",
        mux={"session": "main", "pane_id": 2},
    )
    with pytest.raises(ValueError, match="one live ref"):
        write_registry([bg_double], path=registry_path)

    # The refused writes must not have clobbered the store.
    raw = read_registry_document(registry_path)[0]
    assert [r["name"] for r in raw["agents"]] == ["ok"]


def test_v9_legacy_row_backfills_claude_short_id_into_short_id(
    tmp_path: Path, monkeypatch
) -> None:
    """AC2-EDGE: a legacy v8 row carrying only claude_short_id resolves by that
    value after load; on write-back it carries short_id and no claude_short_id."""
    use_tmpdir(monkeypatch, tmp_path)
    _as_deployed(monkeypatch)
    from fno.agents.registry import load_registry, write_registry

    registry_path = tmp_path / "agents-fixture" / "registry.json"
    registry_path.parent.mkdir(parents=True, exist_ok=True)
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": 8,
                "agents": [
                    {
                        "name": "legacy",
                        "provider": "claude",
                        "cwd": "/tmp",
                        "log_path": "/tmp/legacy.log",
                        "claude_short_id": "7c5dcf5d",
                        "created_at": "2026-05-19T00:00:00Z",
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    loaded = load_registry(path=registry_path)
    assert loaded[0].short_id == "7c5dcf5d"
    # The claude projection resolves by the backfilled short.
    assert loaded[0].session_id == "7c5dcf5d"

    # Write-back drops the legacy key and carries short_id at the current schema.
    # Read from the constant, not a literal: the claim is "whatever this binary
    # writes", which is exactly what a hand-edited number stops being.
    from fno.agents.registry import SCHEMA_VERSION

    write_registry(loaded, path=registry_path)
    raw = read_registry_document(registry_path)[0]
    assert raw["schema_version"] == SCHEMA_VERSION
    row = raw["agents"][0]
    assert "claude_short_id" not in row
    assert row["short_id"] == "7c5dcf5d"


#: `claude` is a HARNESS, and `register_existing_session`'s parameter is named
#: `provider`. Name the axis once rather than bind the literal at each call
#: site - the idiom `test_mail_escalation.py` uses.
CLAUDE_HARNESS = "claude"


def test_origin_is_write_once_and_a_refresh_never_changes_it(tmp_path, monkeypatch):
    """`origin` is a BIRTH fact, and nothing on the refresh path can observe a
    birth. Every weaker rule tried here lost a row to a later refresh, and none
    of the losses is recoverable because nothing ever clears the field.

    The one that survived review twice: an operator resuming a footnote-spawned
    worker in a fresh terminal fires the SessionStart register branch with
    `origin="operator"`. Restamping there takes that worker out of the retire
    lane for good and puts it in the attended mail escalation. Filling an EMPTY
    origin is still allowed, because that row never made a claim."""
    from fno.agents.registry import load_registry, register_existing_session

    use_tmpdir(monkeypatch, tmp_path)

    def register(origin):
        return register_existing_session(
            provider=CLAUDE_HARNESS,
            session_id="11111111-2222-3333-4444-555555555555",
            cwd=str(tmp_path),
            name="worker",
            origin=origin,
        )

    assert register("spawn").origin == "spawn"
    assert register("operator").origin == "spawn", (
        "a refresh must not move a worker into the attended lane"
    )
    assert register("adopted").origin == "spawn"
    assert register(None).origin == "spawn"
    assert [e.origin for e in load_registry()] == ["spawn"]


def test_register_re_stamps_role_from_a_live_manifest(tmp_path, monkeypatch):
    """x-2b6f: the manifest is the durable role record; register is the
    repair door for a row a restore stripped. A manifest naming the session
    stamps an unstamped row, a stamped row is never touched, and no manifest
    registers bare."""
    from fno.agents.registry import (
        AgentEntry,
        register_existing_session,
        write_registry,
    )
    from fno.paths import space_dir

    use_tmpdir(monkeypatch, tmp_path)
    sid = "77777777-8888-9999-aaaa-bbbbbbbbbbbb"

    def manifest_body(session: str, scope: str) -> str:
        return (
            "---\n"
            f"scope: {scope}\n"
            "shape: pass\n"
            "harness: claude\n"
            f"harness_session_id: {session}\n"
            "owner_pid: 1\n"
            "created_at: 2026-09-01T00:00:00Z\n"
            f"role_scope: {scope}\n"
            "role_level: 2\n"
            "role_grantor: vellum\n"
            "---\n"
        )

    leads = space_dir(Path(tmp_path)) / "leads"
    leads.mkdir(parents=True, exist_ok=True)
    (leads / "x-demo.md").write_text(manifest_body(sid, "x-demo"), encoding="utf-8")

    row = register_existing_session(
        provider=CLAUDE_HARNESS,
        session_id=sid,
        cwd=str(tmp_path),
        name="quill2",
    )
    assert row.role_scope == "x-demo"
    assert row.role_level == 2
    assert row.role_grantor == "vellum"

    # No manifest naming it: a fresh session registers bare.
    bare = register_existing_session(
        provider=CLAUDE_HARNESS,
        session_id="99999999-0000-0000-0000-000000000000",
        cwd=str(tmp_path),
        name="bare",
    )
    assert bare.role_scope is None

    # A malformed role_level registers the row BARE instead of raising:
    # a junk stamp must never cost the session its row.
    (leads / "x-junk.md").write_text(
        manifest_body("88888888-0000-0000-0000-000000000000", "x-junk").replace(
            "role_level: 2", "role_level: two"
        ),
        encoding="utf-8",
    )
    junky = register_existing_session(
        provider=CLAUDE_HARNESS,
        session_id="88888888-0000-0000-0000-000000000000",
        cwd=str(tmp_path),
        name="junky",
    )
    assert junky.role_level is None

    # A row already stamped is never touched by a manifest naming it.
    seeded = AgentEntry(
        name="quill2",
        cwd=str(tmp_path),
        log_path="",
        harness=CLAUDE_HARNESS,
        harness_session_id=sid,
    )
    seeded.role_scope = "x-kept"
    seeded.role_level = 1
    seeded.role_grantor = "human"
    write_registry([seeded])
    kept = register_existing_session(
        provider=CLAUDE_HARNESS,
        session_id=sid,
        cwd=str(tmp_path),
        name="quill2",
    )
    assert kept.role_scope == "x-kept"
    assert kept.role_level == 1
    assert kept.role_grantor == "human"


def test_node_field_stamps_and_round_trips_v21(tmp_path, monkeypatch):
    """x-98ab: a row carries the node it works, so a reap decision reads the
    node off the row instead of parsing it out of a name. Stamped at the
    register path from the session's own exported FNO_NODE; round-trips the
    v21 schema (asdict emits the key on every written row, so a pre-v21
    reader must reject the store rather than silently drop the stamp)."""
    from fno.agents.registry import (
        AgentEntry,
        load_registry,
        register_existing_session,
        write_registry,
    )

    use_tmpdir(monkeypatch, tmp_path)
    entry = register_existing_session(
        provider=CLAUDE_HARNESS,
        session_id="99999999-8888-7777-6666-555555555555",
        cwd=str(tmp_path),
        name="nodeworker",
        origin="operator",
        node="x-98ab",
    )
    assert entry.node == "x-98ab"
    loaded = load_registry()
    assert [e.node for e in loaded] == ["x-98ab"]

    # A caller that cannot know records None - never a value parsed out of
    # the name - and the v21 write still emits the key (forward-compat).
    write_registry(
        [
            AgentEntry(
                name="spawned",
                cwd=str(tmp_path),
                log_path="",
                harness=CLAUDE_HARNESS,
                harness_session_id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                origin="spawn",
                node="x-bb18",
            )
        ]
    )
    assert [e.node for e in load_registry()] == ["x-bb18"]

    # A refresh may FILL an empty node (a pre-v21 row gains the stamp its
    # session's own export names) and may never CHANGE a stamped one. A
    # caller saying nothing fills nothing - no evidence, no write.
    refreshed = register_existing_session(
        provider=CLAUDE_HARNESS,
        session_id="99999999-8888-7777-6666-555555555555",
        cwd=str(tmp_path),
        name="nodeworker",
        node="x-98ab",
    )
    assert refreshed.node == "x-98ab", "a refresh must fill an empty node"
    changed = register_existing_session(
        provider=CLAUDE_HARNESS,
        session_id="99999999-8888-7777-6666-555555555555",
        cwd=str(tmp_path),
        name="nodeworker",
        node="x-other",
    )
    assert changed.node == "x-98ab", "a refresh must never change a stamped node"


def _seed_rows(registry_path: Path, rows: list) -> None:
    from fno.agents.registry import update_registry

    def seed(entries):
        entries.extend(rows)
        return entries

    update_registry(seed, path=registry_path)


def test_update_registry_accounts_for_a_removed_row(
    tmp_path: Path, monkeypatch
) -> None:
    """A filtering updater leaves one event and one receipt, receipt first.

    The event rides the canonical writer (``source: "agents"``) onto the
    global stream beside the daemon's events, and the receipt lands in the
    same ``reap-receipts/`` directory the Rust and watchdog writers use.
    """
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, update_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    _seed_rows(
        registry_path,
        [
            AgentEntry(
                name="kept-a",
                harness="claude",
                harness_session_id="a-s",
                cwd="/tmp",
                log_path="/tmp/a.log",
            ),
            AgentEntry(
                name="dropped",
                harness="claude",
                harness_session_id="dropped-s",
                cwd="/tmp",
                log_path="/tmp/d.log",
            ),
        ],
    )

    calls: list[dict] = []

    def fake_spawn_axes_call(payload: dict) -> dict:
        calls.append(payload)
        return {"removed": 1}

    monkeypatch.setattr(
        "fno.agents.spawn_axes_client.spawn_axes_call", fake_spawn_axes_call
    )
    update_registry(
        lambda es: [e for e in es if e.name != "dropped"], path=registry_path
    )

    assert len(calls) == 1, "one accounting ask per write"
    ask = calls[0]["removal_accounting"]
    assert ask["registry"] == str(registry_path)
    dropped = [row for row in ask["before"] if row["name"] == "dropped"]
    assert len(dropped) == 1
    assert dropped[0]["harness_session_id"] == "dropped-s", (
        "the pre-write snapshot rides the payload whole"
    )


def test_write_registry_has_exactly_one_production_caller() -> None:
    """The low-level write stays a primitive of update_registry alone (x-a879).

    ``write_registry`` gains no removal accounting; its only legitimate
    production caller is ``update_registry`` itself. A future direct caller
    is a new silent door, so it must land as a failing count here first.
    """
    import fno.agents.registry as reg_module

    fno_pkg = Path(reg_module.__file__).parents[1]
    callers = []
    for py in sorted(fno_pkg.rglob("*.py")):
        if "test" in py.name:
            continue
        for lineno, line in enumerate(
            py.read_text(encoding="utf-8").splitlines(), 1
        ):
            stripped = line.strip()
            if "write_registry(" in line and not stripped.startswith(
                "def write_registry"
            ):
                callers.append(f"{py.relative_to(fno_pkg)}:{lineno}")

    assert len(callers) == 1, f"write_registry grew a second caller: {callers}"
    assert callers[0].startswith("agents/registry.py:")


def test_update_registry_reports_a_stale_binary_and_writes_nothing(
    tmp_path: Path, monkeypatch
) -> None:
    """An unavailable bridge skips the audit, never the write: the survivors
    persist and the write returns them."""
    use_tmpdir(monkeypatch, tmp_path)

    import fno.agents.spawn_axes_client as spawn_axes_client_module

    def _unavailable(payload):
        raise RuntimeError("spawn-axes answered no removal_accounting field")

    monkeypatch.setattr(spawn_axes_client_module, "spawn_axes_call", _unavailable)
    from fno.agents.registry import AgentEntry, load_registry, update_registry

    registry_path = tmp_path / ".fno" / "agents" / "registry.json"
    _seed_rows(
        registry_path,
        [
            AgentEntry(
                name="kept", harness="claude", harness_session_id="kept-s",
                cwd="/tmp", log_path="/tmp/k.log",
            ),
            AgentEntry(
                name="dropped", harness="claude", harness_session_id="dropped-s",
                cwd="/tmp", log_path="/tmp/d.log",
            ),
        ],
    )

    survivors = update_registry(
        lambda es: [e for e in es if e.name != "dropped"], path=registry_path
    )

    assert [e.name for e in survivors] == ["kept"]
    assert [e.name for e in load_registry(path=registry_path)] == ["kept"]

