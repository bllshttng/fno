"""The registry mint guard: production rows are born through mint_agent_entry.

Replaces the retired check-session-identity-parity.sh and
check-spawn-lineage-parity.sh sweeps. Rust enforces the same invariant with
RegistryEntry::new plus a source-scan guard over crates/fno-agents/src
(the mint_guard_tests module) that fails any non-test RegistryEntry literal
built on ..Default::default(); Python enforces it here and at the mint
constructor's signature.
"""

from pathlib import Path



SRC = Path(__file__).resolve().parents[2] / "src" / "fno"


def test_no_direct_agent_entry_construction_in_production():
    """Every production mint routes through mint_agent_entry; a direct
    AgentEntry( call in src is a mint that can silently omit the identity
    and lineage fields. registry.py holds the dataclass, the mint
    constructor, and the tolerant read path - the allowed exceptions."""
    offenders = []
    for path in sorted(SRC.rglob("*.py")):
        text = path.read_text(encoding="utf-8")
        for lineno, line in enumerate(text.splitlines(), start=1):
            stripped = line.strip()
            if stripped.startswith("#"):
                continue
            if "AgentEntry(" in line and path.name != "registry.py":
                offenders.append(f"{path.relative_to(SRC)}:{lineno}: {stripped}")
            if "AgentEntry(" in line and path.name == "registry.py":
                # Inside registry.py only the mint helper's return and the
                # read path construct directly.
                owner = "mint_agent_entry" if "return AgentEntry(" in line else None
                if owner is None and "**row" not in line:
                    offenders.append(f"{path.relative_to(SRC)}:{lineno}: {stripped}")
    assert not offenders, "direct AgentEntry( mint outside the allowed paths:\n" + "\n".join(offenders)
