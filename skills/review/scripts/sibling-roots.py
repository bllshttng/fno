#!/usr/bin/env python3
"""Print the sibling repos of one repo, read from the workspace config.

Usage: fno config get work.workspaces | python3 sibling-roots.py <repo-root>

stdin is the JSON object `fno config get work.workspaces` prints. A sibling
is any other project listed in a workspace that also lists <repo-root>.
Output is one `name<TAB>path` row per sibling whose path exists on disk,
sorted by name. A linked worktree resolves to its canonical checkout, so a
review inside a worktree still finds the workspace that names the repo.

Exit 0 with rows. Exit 3 when no workspace lists the repo: the caller must
say so in the report, because an empty sibling list is not a measured zero.
Exit 2 on unreadable input.
"""
import json
import os
import subprocess
import sys


def canonical_root(path: str) -> str:
    try:
        common = subprocess.run(
            ["git", "-C", path, "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return os.path.realpath(path)
    if os.path.basename(common) == ".git":
        return os.path.realpath(os.path.dirname(common))
    return os.path.realpath(path)


def norm(path: str) -> str:
    return os.path.realpath(os.path.expanduser(path))


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: fno config get work.workspaces | sibling-roots.py <repo-root>", file=sys.stderr)
        return 2
    try:
        workspaces = json.loads(sys.stdin.read() or "null")
    except ValueError as exc:
        print(f"sibling-roots: stdin is not JSON: {exc}", file=sys.stderr)
        return 2
    if not isinstance(workspaces, dict):
        print("sibling-roots: stdin is not a workspaces object", file=sys.stderr)
        return 2

    root = canonical_root(sys.argv[1])
    siblings: dict[str, str] = {}
    listed = False
    for ws in workspaces.values():
        projects = (ws or {}).get("projects") or []
        paths = [norm(p.get("path", "")) for p in projects if p.get("path")]
        if root not in paths:
            continue
        listed = True
        for p in projects:
            path = norm(p.get("path", ""))
            if path and path != root and os.path.isdir(path):
                siblings.setdefault(p.get("name") or os.path.basename(path), path)

    if not listed:
        print(f"sibling-roots: no workspace lists {root}", file=sys.stderr)
        return 3
    for name in sorted(siblings):
        print(f"{name}\t{siblings[name]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
