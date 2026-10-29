#!/usr/bin/env python3
"""Capture driver for the `paths_hub` goldens.

The Python leg is `fno.paths`, the path hub `fno config doctor` reads its
accessor lines from. Each case in `golden/paths_hub/cases.json` builds a
fixture root (a HOME, a project dir, an optional settings file) and runs the
ten doctor accessors in a fresh interpreter, so no resolver cache crosses
cases. `paths_hub_parity.rs` builds the same fixture from the same file and
compares the Rust owners against each `<case>.out`.

    python3 crates/fno-agents/tests/paths_hub_golden_driver.py           # capture all
    python3 crates/fno-agents/tests/paths_hub_golden_driver.py <case> <root>  # print one case

The second form is the live oracle the Rust test calls: it prints the
normalized lines for one case built under `<root>`.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
GOLDEN = HERE / "golden" / "paths_hub"
CLI_SRC = (HERE / ".." / ".." / ".." / "cli" / "src").resolve()

ACCESSORS = (
    "state_dir",
    "graph_json",
    "ledger_json",
    "briefs_dir",
    "fleet_dir",
    "postmortems_dir",
    "worktrees_base",
    "memory_dir",
    "hook_logs_dir",
    "space_dir",
)

SNIPPET = (
    "from fno import paths\n"
    f"for name in {ACCESSORS!r}:\n"
    "    print(f'{name}={getattr(paths, name)()}')\n"
)


def normalize(text: str, root: str) -> str:
    """Fold the fixture root, and its space-slug spelling, to tokens."""
    return text.replace(root.replace("/", "-"), "<FIXTURE_SLUG>").replace(root, "<FIXTURE>")


def build(case: dict, root: Path) -> tuple[dict[str, str], Path]:
    """Lay out one case under `root`; return its env and project dir."""
    home = root / "home"
    proj = root / "proj"
    home.mkdir(parents=True, exist_ok=True)
    proj.mkdir(parents=True, exist_ok=True)
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith("FNO_") and k not in ("HOME", "PYTHONPATH")
    }
    env["HOME"] = str(home)
    env["FNO_NO_CANONICAL_CONFIG"] = "1"
    if case.get("config"):
        cfg = root / "settings.yaml"
        cfg.write_text(case["config"].replace("<FIXTURE>", str(root)))
        env["FNO_CONFIG"] = str(cfg)
    for key, value in case.get("env", {}).items():
        env[key] = value.replace("<FIXTURE>", str(root))
    return env, proj


def run_case(case: dict, root: Path) -> str:
    env, proj = build(case, root)
    env["PYTHONPATH"] = str(CLI_SRC)
    out = subprocess.run(
        [sys.executable, "-c", SNIPPET],
        cwd=proj,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    return normalize(out.stdout, str(root))


def cases() -> list[dict]:
    return json.loads((GOLDEN / "cases.json").read_text())


def main(argv: list[str]) -> int:
    if len(argv) == 2:
        name, root = argv
        case = next(c for c in cases() if c["name"] == name)
        sys.stdout.write(run_case(case, Path(root)))
        return 0
    for case in cases():
        root = Path(os.path.realpath(tempfile.mkdtemp(prefix="fno-paths-hub-")))
        (GOLDEN / f"{case['name']}.out").write_text(run_case(case, root))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
