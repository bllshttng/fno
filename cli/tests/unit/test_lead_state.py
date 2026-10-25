"""The lead session manifest and the freshness predicate that proves a walk ran."""
from __future__ import annotations

import json

import pytest

from fno.lead.state import (
    last_run_is_fresh,
    parse_manifest,
    write_manifest,
)


@pytest.mark.parametrize("scope", ["../escape", r"a\b"])
def test_scope_keyed_manifest_path_refuses_escape_spellings(tmp_path, scope):
    import fno.lead.state as state

    path_for = getattr(state, "lead_manifest_path", None)
    assert path_for is not None, "scope-keyed lead manifest path is missing"
    with pytest.raises(ValueError, match="scope"):
        path_for(scope, state_root=tmp_path / ".fno")


@pytest.mark.dev_build
def test_cleanup_does_not_delete_a_successors_refreshed_manifest(tmp_path):
    import fno.lead.state as state

    root = tmp_path / ".fno"
    path = state.lead_manifest_path("alpha", state_root=root)
    write_manifest(path, scope="alpha", harness_session_id="successor")

    assert state.remove_lead_manifest(
        "alpha", state_root=root, expected_harness_session_id="vacating-owner"
    ) is False
    assert parse_manifest(path)["harness_session_id"] == "successor"


# --- the freshness predicate ------------------------------------------------


def _journal(tmp_path, *events):
    path = tmp_path / "events.jsonl"
    path.write_text(
        "".join(json.dumps(e) + "\n" for e in events), encoding="utf-8"
    )
    return path


def _terminated(ts, *, driver="lead", reason="NoWork"):
    return {
        "ts": ts,
        "type": "loop_terminated",
        "source": "loop",
        "data": {"driver": driver, "reason": reason},
    }


NOW = "2026-08-18T12:00:00Z"


def test_a_lead_termination_inside_the_window_is_fresh(tmp_path):
    path = _journal(tmp_path, _terminated("2026-08-18T02:00:00Z"))
    assert last_run_is_fresh(path, since_s=24 * 3600, now_iso=NOW) is True


def test_a_target_termination_does_not_satisfy_the_lead_predicate(tmp_path):
    path = _journal(tmp_path, _terminated("2026-08-18T02:00:00Z", driver="target"))
    assert last_run_is_fresh(path, since_s=24 * 3600, now_iso=NOW) is False


def test_the_in_session_arms_termination_also_counts_as_a_walk(tmp_path):
    """Both arms end a lead walk. Reading only the runtime's event would report
    no lead walk right after a lead drained its board and exited."""
    path = _journal(
        tmp_path,
        {
            "ts": "2026-08-18T02:00:00Z",
            "type": "termination",
            "source": "hook",
            "data": {"driver": "lead", "reason": "NoWork", "session_id": "k-1"},
        },
    )
    assert last_run_is_fresh(path, since_s=24 * 3600, now_iso=NOW) is True


def test_window_parsing():
    from fno.lead.state import parse_window

    assert parse_window("90m") == 90 * 60


def test_an_unparseable_window_is_refused():
    from fno.lead.state import parse_window

    with pytest.raises(ValueError):
        parse_window("soon")


# --- the two refusals that make a role real -------------------------------


def _init(
    monkeypatch,
    tmp_path,
    *,
    enabled=True,
    harness_id="sess-1",
    scopes=("drain",),
    readiness_error=None,
    readiness_calls=None,
):
    """Run `fno agents lead init` in tmp_path and return (exit_code, stderr)."""
    import fno.lead.state as state
    from typer.testing import CliRunner

    from fno.lead.cli import lead_app

    def readiness(command, args):
        if readiness_calls is not None:
            readiness_calls.append((command, args))
        if readiness_error:
            return readiness_error, None
        return None, {"ready": True}

    monkeypatch.setattr(state, "lead_loop_enabled", lambda: enabled)
    monkeypatch.setattr("fno.rust_binary.call_binary_json", readiness)
    monkeypatch.chdir(tmp_path)
    (tmp_path / ".fno").mkdir(exist_ok=True)
    result = CliRunner().invoke(
        lead_app,
        ["init", *[arg for scope in scopes for arg in ("--scope", scope)],
         "--harness-session-id", harness_id],
    )
    return result.exit_code, result.output


def test_a_disabled_lead_loop_writes_no_manifest(monkeypatch, tmp_path):
    """`config.lead.enabled` must gate the loop, not just describe it.

    Every arm - the stop hook, `loop-check --driver lead`, and `LeadQueue` -
    arms on this manifest existing. So the manifest is the one chokepoint where
    the flag can gate all three. Before this, the flag was read ONLY by
    `fno agents autonomy status`: the corpus's "guard on one of N reachable paths"
    with N of zero, and a default-off lead still held sessions open.
    """
    code, _ = _init(monkeypatch, tmp_path, enabled=False)

    assert code == 3
    assert not (tmp_path / ".fno" / "leads" / "drain.md").exists()


def test_rust_readiness_refusal_writes_no_role_manifest(monkeypatch, tmp_path):
    import fno.lead.state as state

    calls = []
    monkeypatch.setenv("FNO_HARNESS", "codex")
    code, out = _init(
        monkeypatch,
        tmp_path,
        readiness_error="Stop readiness is blocked: session-refresh-unverified",
        readiness_calls=calls,
    )

    assert code == 2
    assert "session-refresh-unverified" in out
    assert calls == [
        (
            "loop",
            ["readiness", "--scope", "drain", "--session", "sess-1", "--ensure-goal"],
        )
    ]
    assert not state.lead_manifest_path("drain", state_root=tmp_path / ".fno").exists()


@pytest.mark.dev_build
def test_a_repeated_scope_roles_one_epic_set(monkeypatch, tmp_path):
    code, out = _init(monkeypatch, tmp_path, scopes=("x-4d9b", "x-119e"))

    assert code == 0, out
    import fno.lead.state as state

    assert state.lead_manifest_path("x-119e,x-4d9b").exists()
    assert "scope:  x-119e,x-4d9b" in out


