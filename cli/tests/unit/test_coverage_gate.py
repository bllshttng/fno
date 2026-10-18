from __future__ import annotations

from pathlib import Path


def test_resolved_max_rounds_uses_each_worktree_root(monkeypatch, tmp_path: Path):
    first = tmp_path / "first"
    second = tmp_path / "second"
    for root, rounds in ((first, 5), (second, 7)):
        config_dir = root / ".fno"
        config_dir.mkdir(parents=True)
        (config_dir / "config.toml").write_text(
            f"[review]\nmax_rounds = {rounds}\n", encoding="utf-8"
        )

    from fno.pr import _coverage_gate

    monkeypatch.setattr(_coverage_gate, "_repo_root", lambda cwd: Path(cwd))
    monkeypatch.setattr(
        "fno.paths.resolve_canonical_worktree",
        lambda root=None, timeout=None: Path(root) if root else None,
    )
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(tmp_path / "global.toml"))
    monkeypatch.delenv("FNO_CONFIG", raising=False)

    assert _coverage_gate.resolved_max_rounds(str(first)) == 5
    assert _coverage_gate.resolved_max_rounds(str(second)) == 7


def test_attestation_chain_dedups_a_stamped_mirror_copy(monkeypatch, tmp_path: Path):
    """The push stamps the writing machine into the primary copy only, so
    the journal original and its mirror differ in the carrier keys alone.
    The mirror dedup drops both, or the reviewing machine counts a round
    twice."""
    import json

    from fno.pr import _coverage_gate, _reviews

    project = tmp_path / "project.jsonl"
    mirror = tmp_path / "global.jsonl"
    payload = {
        "reviewer": "code-review",
        "branch": "feature/x",
        "head_sha": "abc1234",
        "verdict": "pass",
    }
    project.write_text(
        json.dumps({"ts": "t1", "type": "review_attestation", "data": payload}) + "\n",
        encoding="utf-8",
    )
    stamped = dict(payload, repo="owner/repo", attest_machine="imac")
    mirror.write_text(
        json.dumps({"ts": "t1", "type": "review_attestation", "data": stamped}) + "\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(
        _reviews, "_coverage_logs", lambda cwd, project_events: (project, mirror, "owner/repo")
    )
    chain = _coverage_gate.attestation_chain(None, "feature/x", "abc1234")
    assert [e["head_sha"] for e in chain] == ["abc1234"]
