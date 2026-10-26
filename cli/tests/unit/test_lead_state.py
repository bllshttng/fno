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


