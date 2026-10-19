"""The relay registry's one live surface: ``transcript_path_for``.

Recovery resolves a session's transcript through it. The rest of the module
(``index``, ``register``, the cross-harness bridge) has no reachable caller.
"""
from __future__ import annotations

from fno.relay import registry as reg


def test_transcript_path_for_globs_by_session_id(tmp_path):
    proj = tmp_path / "projects"
    # claude encodes both "/" and "." in the cwd to "-", so the dir name is
    # unpredictable; glob by the <session_id>.jsonl filename instead.
    d = proj / "-Users-bb16--claude-jobs-x"
    d.mkdir(parents=True)
    (d / "sid-123.jsonl").write_text("{}")
    assert reg.transcript_path_for("sid-123", projects_dir=proj) == str(d / "sid-123.jsonl")
    assert reg.transcript_path_for("missing", projects_dir=proj) is None
