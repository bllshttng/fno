"""Unit tests for fno.claims.io: atomic-write + YAML round-trip + URL-encoding."""
from __future__ import annotations

import os
from pathlib import Path

import pytest

from fno.claims.io import (
    ClaimAlreadyHeld,
    atomic_create_exclusive,
    claims_dir,
    decode_key,
    encode_key,
    read_claim_file,
)


# ---------------------------------------------------------------------------
# Key encoding
# ---------------------------------------------------------------------------


def test_encode_decode_round_trip():
    for key in [
        "node:ab-1234",
        "fleet:ab-mission01",
        "project:ab-mission:proj1",
        "simple",
        "with spaces and / slashes",
    ]:
        encoded = encode_key(key)
        assert "/" not in encoded
        decoded = decode_key(encoded + ".lock")
        assert decoded == key


# ---------------------------------------------------------------------------
# Atomic create exclusive
# ---------------------------------------------------------------------------


def test_atomic_create_exclusive_collision_raises(tmp_path):
    path = tmp_path / "x.lock"
    path.parent.mkdir(parents=True, exist_ok=True)
    atomic_create_exclusive(path, "first")
    with pytest.raises(ClaimAlreadyHeld):
        atomic_create_exclusive(path, "second")
    assert path.read_text() == "first", "loser must NOT overwrite"


# ---------------------------------------------------------------------------
# Global node-claims root resolution (ab-fcf9cec5)
# ---------------------------------------------------------------------------


def test_claims_dir_defaults_to_the_repo_space_without_env(tmp_path, monkeypatch):
    """No root + no env => the repo's space (no claims dir inside a checkout)."""
    monkeypatch.delenv("FNO_CLAIMS_ROOT", raising=False)
    monkeypatch.chdir(tmp_path)
    from fno.paths import space_dir, space_slug

    # The slug is a cross-language wire format (fno-agents::paths swaps the
    # same separators); a golden value here catches a one-sided drift.
    assert space_slug(Path("/repos/web")) == "-repos-web"
    assert claims_dir() == space_dir(tmp_path) / "claims"


# Identity-based root routing (node/dispatch/reconcile -> global, repo-local
# keys -> the default) is owned by the Rust leg now: its characterization
# golden lives in crates/fno-agents/tests/claims_root_parity.rs.


# --- The state-root denial (x-f22f) -----------------------------------------
#
# A permission denial and lock contention both leave the caller without a
# claim, and they have opposite remedies: contention clears on its own, a
# missing write grant never does. A worker told only "no claim" reads it as
# contention and waits for a holder that does not exist.


def test_denied_claim_write_is_not_reported_as_contention(tmp_path, monkeypatch):
    from fno.claims.io import ClaimStateRootDenied

    state_root = tmp_path / ".fno"
    claims = state_root / "claims"
    claims.mkdir(parents=True)
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda: repo)
    claims.chmod(0o500)  # readable, not writable: the sandbox's shape
    try:
        with pytest.raises(ClaimStateRootDenied) as caught:
            atomic_create_exclusive(claims / "node%3Aab-1.lock", "x: 1\n")
    finally:
        claims.chmod(0o700)

    message = str(caught.value)
    assert str(state_root) in message, message
    assert "NOT lock contention" in message, message


def test_a_later_successful_claim_clears_the_breadcrumb(tmp_path, monkeypatch):
    claims = tmp_path / ".fno" / "claims"
    claims.mkdir(parents=True)
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda: repo)
    crumb = repo / ".fno" / "state-root-denied.json"
    crumb.parent.mkdir(parents=True)
    crumb.write_text("{}")

    atomic_create_exclusive(claims / "node%3Aab-3.lock", "x: 1\n")

    assert not crumb.exists(), "a successful claim proves the grant is back"


def test_denial_on_a_not_yet_created_claims_dir_is_still_named(tmp_path, monkeypatch):
    """The case this feature exists for, and the one the first version missed.

    A sandboxed worker usually meets a state root whose ``claims`` child does
    not exist yet. The create then fails ``FileNotFoundError``, the retry branch
    calls ``mkdir``, and THAT is the call the sandbox denies. An earlier version
    mapped denials only on the first attempt, so this escaped as a raw
    ``PermissionError``: no named refusal, no breadcrumb, and a worker left to
    read it as lock contention.
    """
    import json

    from fno.claims.io import ClaimStateRootDenied

    state_root = tmp_path / ".fno"
    state_root.mkdir()  # exists, but has NO claims child
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda: repo)
    state_root.chmod(0o500)
    try:
        with pytest.raises(ClaimStateRootDenied) as caught:
            atomic_create_exclusive(state_root / "claims" / "node%3Aab-9.lock", "x: 1\n")
    finally:
        state_root.chmod(0o700)

    assert str(state_root) in str(caught.value)
    crumb = repo / ".fno" / "state-root-denied.json"
    assert crumb.exists(), "the denial must leave a breadcrumb here too"
    assert json.loads(crumb.read_text())["denied_root"] == str(state_root)


def test_a_claim_under_a_different_root_leaves_a_live_denial_alone(tmp_path, monkeypatch):
    """An unconditional clear erased a denial the worker still had.

    A worker holds claims under more than one root. A success under the
    repo-local store must not delete the report that the GLOBAL store is
    denied: the problem stands and the only record of it disappears.
    """
    import json

    other_root = tmp_path / "other" / ".fno"
    claims = other_root / "claims"
    claims.mkdir(parents=True)
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda: repo)
    crumb = repo / ".fno" / "state-root-denied.json"
    crumb.parent.mkdir(parents=True)
    crumb.write_text(json.dumps({"denied_root": "/some/other/.fno"}))

    atomic_create_exclusive(claims / "node%3Aab-10.lock", "x: 1\n")

    assert crumb.exists(), "a success elsewhere must not erase a live denial"
    assert json.loads(crumb.read_text())["denied_root"] == "/some/other/.fno"


# ---------------------------------------------------------------------------
# The read and archive locators over the claims table
# ---------------------------------------------------------------------------


def test_read_claim_file_round_trips_ttl_and_session_id(tmp_path):
    from fno.claims import acquire_claim
    from fno.claims.io import claim_path

    acquire_claim(
        "node:ab-1", "h", ttl_ms=60_000, pid=os.getpid(),
        harness_session_id="abc123", root=tmp_path,
    )
    parsed = read_claim_file(claim_path("node:ab-1", root=tmp_path))
    assert parsed.holder == "h"
    assert parsed.expires_at is not None
    assert parsed.session_id == "abc123"


def test_an_invalid_row_raises_claim_corrupted(tmp_path):
    from fno.claims import acquire_claim, claim_status
    from fno.claims.io import ClaimCorrupted, claim_path
    from tests._table_seed import update_claim

    acquire_claim("node:ab-3", "h", root=tmp_path)
    update_claim("node:ab-3", root=tmp_path, schema_version=999)
    with pytest.raises(ClaimCorrupted):
        read_claim_file(claim_path("node:ab-3", root=tmp_path))
    assert claim_status("node:ab-3", root=tmp_path)["state"] == "corrupted"


def test_archive_claim_retires_the_row(tmp_path):
    from fno.claims import acquire_claim, claim_status
    from fno.claims.io import archive_claim, claim_path

    acquire_claim("node:ab-4", "h", root=tmp_path)
    archive_claim(claim_path("node:ab-4", root=tmp_path), ts_ms=1234567890)
    assert claim_status("node:ab-4", root=tmp_path)["state"] == "free"
