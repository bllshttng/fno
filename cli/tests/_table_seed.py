"""Seed the registry table the way production writes it.

``registry.json`` is a migration fence once the native
store opens, so a test that writes it as a file after that point fails.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Optional


def seed_registry(rows: list[dict[str, Any]], path: Optional[Path] = None, **document: Any) -> None:
    """Replace the registry document with ``rows`` (plus any top-level keys)."""
    from fno import paths
    from fno.registry_door import commit_registry_document, read_registry_document

    target = Path(path) if path is not None else Path(paths.agents_registry_path())
    target.parent.mkdir(parents=True, exist_ok=True)
    current, revision = read_registry_document(target)
    payload = {**current, **document, "agents": rows}
    commit_registry_document(target, payload, revision)


def seed_legacy_registry(entries: list[Any], path: Path) -> Path:
    """Write ``entries`` as the pre-table ``registry.json`` the import reads.

    The table door refuses rows that collide on identity, but a legacy file
    can still hold them; this is the only way to seed that shape. It must run
    before anything opens the store at ``path``.
    """
    import json
    from dataclasses import asdict

    from fno.agents.registry import SCHEMA_VERSION

    rows = [asdict(e) if not isinstance(e, dict) else e for e in entries]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"schema_version": SCHEMA_VERSION, "agents": rows}))
    return path
