"""Tests for durable typed resume receipts (x-c3a2).

Covers the two invariants the module exists to enforce:
  - a receipt is evidence, not authority (write is a snapshot; revalidate is
    the fail-closed gate and never mutates predecessor state/claims);
  - no resume database (event canonicalization reuses the scoreboard reducer's
    dedup + latest-by-parsed-timestamp rule, so duplicated/out-of-order events
    across the global + delivery-root journals fold to one observation).

Plus every fail-closed revalidation case named in the plan: malformed, stale
HEAD, foreign claim, dead worktree, duplicate generation, superseded-by-later.
"""
from __future__ import annotations

import json

import pytest

from fno.resume.receipt import (
    MalformedReceiptError,
    build_receipt,
    load_receipt,
    revalidate,
    write_receipt,
)


# ---------------------------------------------------------------------------
# fixtures
# ---------------------------------------------------------------------------


def _receipt(**overrides):
    base = dict(
        node="x-c3a2",
        session="s1",
        phase="execute",
        generation=2,
        repo="footnote",
        worktree="/wt/x-c3a2",
        branch="feature/x-c3a2",
        head="abc123def456",
        next_verb="/fno:execute waves",
        next_target="x-c3a2",
        written_at="2026-07-26T02:00:00Z",
        completed_tasks=["1.1"],
        remaining_tasks=["2.1", "2.2"],
        open_findings=["finding-1"],
        known_reds=["smoke"],
        claims=[{"key": "node:x-c3a2", "holder": "s1"}],
        watchers=["ci"],
        idempotency_keys=["pr_create:abc123d", "comment:abc123d"],
    )
    base.update(overrides)
    return build_receipt(**base)


def _ok_kwargs(**overrides):
    base = dict(
        live_head="abc123def456",
        live_branch="feature/x-c3a2",
        worktree_exists=True,
        live_claim_holder="s1",
        own_session="s1",
        node_events=[],
        harness="claude",
    )
    base.update(overrides)
    return base


# ---------------------------------------------------------------------------
# write + load + immutability
# ---------------------------------------------------------------------------


def test_write_and_load_round_trip(tmp_path):
    r = _receipt()
    p = write_receipt(r, tmp_path / "artifacts")
    assert p.name == "receipt-x-c3a2-execute-g2-abc123def456.json"
    loaded = load_receipt(p)
    assert loaded.identity == r.identity
    assert loaded.head == r.head
    assert loaded.remaining_tasks == ("2.1", "2.2")
    assert loaded.idempotency_keys == ("pr_create:abc123d", "comment:abc123d")
    assert loaded.next_action.verb == "/fno:execute waves"
    assert loaded.content_sha == r.content_sha


def test_write_is_immutable_refuses_overwrite(tmp_path):
    r = _receipt()
    write_receipt(r, tmp_path)
    with pytest.raises(FileExistsError):
        write_receipt(r, tmp_path)


# ---------------------------------------------------------------------------
# malformed receipt -> fail closed (never read as empty evidence)
# ---------------------------------------------------------------------------


def _corrupt_json(_d):
    return "{not json"


def _gen_bool(d):
    d["identity"]["generation"] = True


@pytest.mark.parametrize(
    "mutate",
    [
        _corrupt_json,  # corrupt JSON
        _gen_bool,  # bool-as-int generation
    ],
)
def test_malformed_receipt_rejected(tmp_path, mutate):
    p = write_receipt(_receipt(), tmp_path)
    data = json.loads(p.read_text())
    result = mutate(data)
    p.write_text(result if isinstance(result, str) else json.dumps(data))
    with pytest.raises(MalformedReceiptError):
        load_receipt(p)


def test_content_sha_tamper_detected(tmp_path):
    p = write_receipt(_receipt(), tmp_path)
    data = json.loads(p.read_text())
    data["remaining_tasks"] = ["sneaky-extra"]  # payload changed, sha unchanged
    p.write_text(json.dumps(data))
    with pytest.raises(MalformedReceiptError):
        load_receipt(p)


# ---------------------------------------------------------------------------
# duplicate generation
# ---------------------------------------------------------------------------


def _delegated(gen, frm, harness="claude"):
    return {
        "type": "delegated",
        "ts": "2026-07-26T02:30:00Z",
        "data": {"node_id": "x-c3a2", "harness": harness, "generation": gen, "from_session": frm},
    }


# ---------------------------------------------------------------------------
# revalidate: ok + every fail-closed case
# ---------------------------------------------------------------------------


def test_revalidate_ok():
    assert revalidate(_receipt(), **_ok_kwargs()).ok is True


def test_revalidate_stale_head():
    res = revalidate(_receipt(), **_ok_kwargs(live_head="zzzzzzz"))
    assert res.ok is False and res.reason == "stale_head"


def test_revalidate_stale_branch():
    res = revalidate(_receipt(), **_ok_kwargs(live_branch="feature/other"))
    assert res.ok is False and res.reason == "stale_branch"


def test_revalidate_dead_worktree():
    res = revalidate(_receipt(), **_ok_kwargs(worktree_exists=False))
    assert res.ok is False and res.reason == "dead_worktree"


def test_revalidate_foreign_claim():
    res = revalidate(_receipt(), **_ok_kwargs(live_claim_holder="other-session"))
    assert res.ok is False and res.reason == "foreign_claim"


def test_revalidate_duplicate_generation():
    res = revalidate(
        _receipt(),
        **_ok_kwargs(node_events=[_delegated(2, "OTHER-SESSION")]),
    )
    assert res.ok is False and res.reason == "duplicate_generation"


def test_revalidate_superseded_by_later_event():
    later_termination = [
        {"type": "termination", "ts": "2026-07-26T05:00:00Z", "data": {"node_id": "x-c3a2"}},
    ]
    res = revalidate(_receipt(), **_ok_kwargs(node_events=later_termination))
    assert res.ok is False
    assert res.reason.startswith("superseded_by_later_event")


def test_revalidate_own_delegation_does_not_supersede():
    # handoff.sh writes the receipt (Step 2) then emits its OWN `delegated` event
    # (Step 8); that own continuation must not supersede the receipt it produced.
    # Only a FOREIGN later event is stale authority reviving.
    own_delegation = [
        {"type": "delegated", "ts": "2031-12-31T23:59:59Z",
         "data": {"node_id": "x-c3a2", "from_session": "s1", "generation": 2, "harness": "claude"}},
    ]
    res = revalidate(_receipt(), **_ok_kwargs(node_events=own_delegation))
    assert res.ok is True
