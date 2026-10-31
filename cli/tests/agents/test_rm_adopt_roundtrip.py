"""The store probe behind `fno agents adopt`.

`fno agents rm` forfeits a resume handle REVERSIBLY: the harness session record
goes, the transcript stays, and `fno agents adopt` rebuilds the row from what is
left. `adopt` itself is a Rust command, so this test pins the Python half its
recovery depends on: the store probe that `adopt`'s third resolution step runs
(`heal_token` -> `probe_stores`), which is what finds a reaped session by its
8-hex short id.
"""
from __future__ import annotations

from pathlib import Path

import pytest

# A full claude session uuid and the 8-hex short id an operator actually peeks
# and adopts by. The short is the uuid's first eight, matching canonical_handle.
SESSION_ID = "0a6e775f-1111-2222-3333-444444444444"
SHORT_ID = "0a6e775f"


@pytest.fixture
def isolated_state(tmp_path: Path, monkeypatch) -> Path:
    from fno import paths

    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setattr(paths, "agents_registry_path", lambda: tmp_path / "registry.jsonl")
    monkeypatch.setattr(paths, "state_dir", lambda: tmp_path / "state")
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: fake_home))
    return tmp_path


def _seed_transcript(tmp_path: Path, monkeypatch) -> Path:
    """A claude transcript on disk, named by its session uuid as claude names it."""
    from fno.agents.discover import PROJECTS_DIR_ENV

    root = tmp_path / "projects" / "-tmp-project"
    root.mkdir(parents=True)
    path = root / f"{SESSION_ID}.jsonl"
    path.write_text(
        '{"type":"user","cwd":"/tmp","message":{"role":"user","content":"hi"}}\n',
        encoding="utf-8",
    )
    monkeypatch.setenv(PROJECTS_DIR_ENV, str(tmp_path / "projects"))
    return path


def test_the_store_probe_resolves_a_reaped_short_id(isolated_state, tmp_path, monkeypatch):
    """Adopt's recovery is real: the harness store probe still answers for the
    short id once the row is gone.

    `adopt`'s third resolution step is the harness store probe, so if the
    probe answers for the short id of a row that no longer exists, every
    `fno agents adopt` hint the notices print is a promise the tool keeps.
    (The rm half of the old round trip is Rust-owned now; the crate's index
    tests pin that rm drops the index record, never the transcript.)
    """
    from fno.agents.store_fallback import probe_stores

    _seed_transcript(tmp_path, monkeypatch)

    hits = probe_stores(SHORT_ID)
    assert [h.session_id for h in hits] == [SESSION_ID], (
        "adopt's store probe must answer for the short id of a reaped row; "
        "otherwise every `fno agents adopt` hint this change prints is a lie"
    )
