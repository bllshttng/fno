"""Installer for the fno-agents state-path stub used by init-driving tests.

Copies tests/helpers/fno-agents-state-path-stub.sh into a test-owned bin dir
and returns the env (PATH entry + FNO_TEST_SPACE + FNO_AGENTS_BIN) the test
must merge before invoking init-target-state.sh.
"""

from __future__ import annotations

import shutil
from pathlib import Path

STUB = Path(__file__).resolve().parents[2] / "tests" / "helpers" / "fno-agents-state-path-stub.sh"


def install_state_path_stub(bin_dir: Path, space: Path) -> dict[str, str]:
    bin_dir.mkdir(parents=True, exist_ok=True)
    space.mkdir(parents=True, exist_ok=True)
    dest = bin_dir / "fno-agents"
    shutil.copyfile(STUB, dest)
    dest.chmod(0o755)
    # FNO_AGENTS_BIN outranks PATH in init-target-state.sh, and the pytest
    # conftest pins that env to the checkout binary, so the stub must ride
    # the env name too or the pinned binary answers and the manifest lands
    # outside `space`.
    return {"FNO_TEST_SPACE": str(space), "FNO_AGENTS_BIN": str(dest)}
