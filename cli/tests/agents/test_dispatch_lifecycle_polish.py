"""``reconcile_agents`` preserves a concurrent ``last_message_at`` bump.

Its batched ``_apply`` writes a status flip onto the CURRENT registry entry,
not the probe-time snapshot, so a dispatch interleaving between the probe
loop and the atomic apply keeps its ``last_message_at``. The bump rides a
``Callable[[], str]`` that ``_stamp_status`` evaluates inside the lock.
"""
from __future__ import annotations

import threading
from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir


def _seed_registry(*entries):
    """Persist the given AgentEntry-kwargs dicts as the active registry."""
    from fno.agents.registry import AgentEntry, write_registry

    out: list[AgentEntry] = []
    for kwargs in entries:
        kwargs.setdefault("cwd", "/tmp")
        kwargs.setdefault("log_path", "/tmp/x.log")
        # v10 (x-880e): map legacy identity kwargs to the canonical fields.
        if "provider" in kwargs:
            kwargs["harness"] = kwargs.pop("provider")
        for _k in ("codex_session_id", "gemini_session_id", "claude_session_uuid"):
            if _k in kwargs:
                kwargs.setdefault("harness_session_id", kwargs.pop(_k))
        out.append(AgentEntry(**kwargs))
    write_registry(out)
    return out


def _load_entry(name: str):
    from fno.agents.registry import load_registry

    for entry in load_registry():
        if entry.name == name:
            return entry
    raise AssertionError(f"entry {name!r} not found in registry")


@pytest.mark.parametrize(
    "provider,session_field,session_id",
    [
        (
            "codex",
            "codex_session_id",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        ),
    ],
)
def test_concurrent_reconcile_and_ask_preserves_both_fields(
    tmp_path: Path,
    monkeypatch,
    provider: str,
    session_field: str,
    session_id: str,
) -> None:
    """Reconcile + ask race: both ``status`` and ``last_message_at`` land.

    Setup:
      1. Seed a registry entry with ``status=live, last_message_at=None``.
      2. Monkeypatch the provider's reachability probe so reconcile
         decides to flip ``status`` to ``"orphaned"``. Block the probe
         on a barrier so reconcile pauses with ``pending_updates`` held
         but before the atomic apply.
      3. From a second thread, run an inline ``_stamp_status`` cycle
         simulating dispatch_ask's ``last_message_at`` bump.
      4. Release the barrier; let reconcile's atomic apply finish.

    Pre-fix: reconcile's ``_apply`` builds the new entries from the
    pre-probe snapshot via ``dataclasses.replace(entry, status=...)``
    where ``entry`` is the captured-at-probe snapshot. The mid-flight
    ``last_message_at`` bump is overwritten.

    Post-fix: ``_apply`` uses the current registry entry as the base
    and only overrides ``status`` from the pending-updates dict.
    ``last_message_at`` survives.

    Final state asserted:
      - No ``OSError`` raised by either thread.
      - ``entry.status == "orphaned"`` (reconcile won the status flip).
      - ``entry.last_message_at == "2026-05-21T19:30:00Z"`` (ask
        survived the apply).
    """
    use_tmpdir(monkeypatch, tmp_path)
    _seed_registry(
        dict(
            name=f"worker-{provider}",
            provider=provider,
            status="live",
            last_message_at=None,
            **{session_field: session_id},
        ),
    )

    from fno.agents import dispatch
    from fno.agents.registry import update_registry

    probe_started = threading.Event()
    apply_may_proceed = threading.Event()

    if provider == "codex":
        from fno.agents.harnesses import codex as codex_mod

        # Make the codex session index "ready" so the reconcile loop
        # reaches the per-entry reachability check.
        monkeypatch.setattr(
            codex_mod, "session_index_exists", lambda *, session_index_path=None: True
        )

        def fake_load_known(*, session_index_path=None):
            # Block until the test thread has run the dispatch_ask
            # simulation; report "not known" so reconcile flips to
            # orphaned.
            probe_started.set()
            apply_may_proceed.wait(timeout=5.0)
            return set()

        monkeypatch.setattr(codex_mod, "load_known_session_ids", fake_load_known)

    errors: list[Exception] = []

    def run_reconcile():
        try:
            dispatch.reconcile_agents()
        except Exception as exc:  # noqa: BLE001
            errors.append(exc)

    reconciler = threading.Thread(target=run_reconcile, daemon=True)
    reconciler.start()

    # Wait for reconcile to be paused INSIDE the probe loop (i.e. it
    # has loaded the registry snapshot, started probing, and is
    # blocked on the barrier).
    assert probe_started.wait(timeout=5.0), "reconcile probe never started"

    # Simulate dispatch_ask bumping last_message_at while reconcile is
    # paused with a stale snapshot. We use _stamp_status directly to
    # mimic what dispatch_ask does post-shellout without spinning up a
    # whole subprocess fixture.
    update_registry(
        dispatch._stamp_status(
            f"worker-{provider}",
            status="live",
            last_message_at=lambda: "2026-05-21T19:30:00Z",
        ),
    )

    # Let reconcile finish.
    apply_may_proceed.set()
    reconciler.join(timeout=5.0)
    assert not reconciler.is_alive(), "reconcile thread did not finish"

    assert errors == [], f"reconcile raised: {errors}"

    entry = _load_entry(f"worker-{provider}")
    # Reconcile flipped status to orphaned.
    assert entry.status == "orphaned", (
        f"expected reconcile to flip status to orphaned, got {entry.status!r}"
    )
    # Dispatch_ask's last_message_at survived reconcile's apply.
    assert entry.last_message_at == "2026-05-21T19:30:00Z", (
        f"dispatch_ask's last_message_at was lost by reconcile's _apply; "
        f"got {entry.last_message_at!r}"
    )
