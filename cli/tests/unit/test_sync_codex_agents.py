from __future__ import annotations

import subprocess
import sys
import tomllib
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]


def test_codex_agents_are_generated_and_parse() -> None:
    res = subprocess.run(
        [sys.executable, "scripts/sync-codex-agents.py", "--check"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    assert res.returncode == 0, res.stderr

    files = sorted((REPO_ROOT / ".codex" / "agents").glob("*.toml"))
    names = {path.stem for path in files}

    def _has_pack(path: Path) -> bool:
        text = path.read_text(encoding="utf-8")
        block = text.split("---\n", 1)[1].split("\n---", 1)[0] if text.startswith("---\n") else ""
        return any(line.strip().startswith("pack:") for line in block.splitlines())

    agent_mds = sorted((REPO_ROOT / "agents").glob("*.md"))
    non_pack = [path for path in agent_mds if not _has_pack(path)]
    # Every non-pack agent is registered on codex; pack-contributed agents are a
    # Claude-plugin-skill surface (their bounded-tool allowlist is enforced by
    # the Claude harness, not expressible in codex's coarse sandbox), so they
    # are deliberately NOT registered on the codex harness.
    assert names == {path.stem for path in non_pack}
    assert "growth-marketer" not in names
    for path in files:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
        assert data["name"] == path.stem
        assert data["description"]
        assert len(data["description"]) <= 360
        assert "<example>" not in data["description"]
        assert "\\n" not in data["description"]
        assert data["developer_instructions"]
        assert data["sandbox_mode"] in {"read-only", "workspace-write"}
