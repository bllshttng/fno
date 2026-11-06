from __future__ import annotations

from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parents[3]
PACK_DIR = REPO_ROOT / "plugins" / "growth-studio"

# The bounded tool allowlist a packaged agent may hold. No ceiling grants a
# network, shell, or delegation tool.
ALLOWED_TOOLS = {"Read", "Write", "Edit", "Glob", "Grep"}
FORBIDDEN_TOOLS = {"Bash", "Task", "WebSearch", "WebFetch"}
AGENT_GLOBS = (
    PACK_DIR / "agents",
    REPO_ROOT / "agents",
)


def _agent_files() -> list[Path]:
    files: list[Path] = []
    for base in AGENT_GLOBS:
        if base.is_dir():
            files.extend(base.glob("growth-*.md"))
    return files


def _frontmatter_tools(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8")
    if not text.startswith("---"):
        return []
    lines = text.splitlines()
    end = next((i for i in range(1, len(lines)) if lines[i].strip() == "---"), None)
    if end is None:
        return []
    block = yaml.safe_load("\n".join(lines[1:end])) or {}
    tools = block.get("tools", [])
    return [str(t) for t in tools]


# AC13-SEC: no packaged agent holds a network, shell, or delegation tool, and a
# declared publication effect still requires approval. The tool-list check is
# scoped to each agent's `tools:` field (not its disallowedTools refusal list).


def test_every_packaged_agent_tool_list_is_bounded() -> None:
    agents = _agent_files()
    assert agents, "expected bundled + pack agent files to exist"
    all_tools: set[str] = set()
    for agent in agents:
        tools = set(_frontmatter_tools(agent))
        # Positive control: the parse found a real allowed tool, so an empty
        # forbidden-set is a real result, not a missed search.
        assert tools & ALLOWED_TOOLS, f"no allowed tools parsed from {agent.name}"
        offenders = tools & FORBIDDEN_TOOLS
        assert not offenders, f"{agent.name} tools include forbidden {offenders}"
        offenders_mcp = {t for t in tools if t.startswith("mcp__")}
        assert not offenders_mcp, f"{agent.name} tools include MCP {offenders_mcp}"
        all_tools |= tools
    # Positive control at the sweep level: at least one allowed tool is present
    # across the agent set, proving the search reached the files.
    assert all_tools & ALLOWED_TOOLS
