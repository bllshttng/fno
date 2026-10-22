"""``term_state``: the Python client over the Rust term reader.

The reader's semantics (agreeing, split, no manifest, unreadable registry,
terminal rows, unsafe scopes, the shape rewrite) live in
``crates/fno-agents/src/lead_state.rs`` and are pinned by that module's own
tests. What these tests pin is the client half: a missing binary answers
unknown with a named reason (never a clean ``False``), and the manifest
writer records the shape the reader keys on.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir


@pytest.mark.dev_build
def test_ac1_manifest_written_by_init_carries_shape_pass(tmp_path: Path) -> None:
    from fno.lead.state import lead_manifest_path, parse_manifest, write_manifest

    path = lead_manifest_path("alpha", state_root=tmp_path / ".fno")
    write_manifest(
        path,
        scope="alpha",
        harness_session_id="aaaa1111-0000-4000-8000-000000000001",
        owner_cwd=str(tmp_path),
    )
    fields = parse_manifest(path)
    assert fields["shape"] == "pass"
    # A team-shaped write records team verbatim, not normalized away.
    team = lead_manifest_path("beta", state_root=tmp_path / ".fno")
    write_manifest(
        team,
        scope="beta",
        harness_session_id="aaaa2222-0000-4000-8000-000000000002",
        shape="team",
        owner_cwd=str(tmp_path),
    )
    assert parse_manifest(team)["shape"] == "team"


def test_missing_binary_answers_unknown_never_clean_false(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.lead.state import term_state

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: None)
    state = term_state(scope="alpha")
    assert state.live is None
    assert state.promoted is None
    assert state.split is None
    assert state.unknown_reason is not None
    assert "binary" in state.unknown_reason
