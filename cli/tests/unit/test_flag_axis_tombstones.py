"""AC13-INV: the axis tombstones keep their message and lose their expiry date.

The `--provider` tombstones are not deprecated aliases waiting to expire. Each
is a hidden option whose only behavior is to exit 2 with the axis map, and that
message is worth keeping indefinitely - deleting the option would trade a
message naming `--harness/-H` for Click's bare "No such option: --provider".

So the date goes and the message stays. These two tests pin both halves.
"""
from __future__ import annotations

from pathlib import Path

import pytest
import typer

from fno._flag_aliases import PROVIDER_AXIS_TOMBSTONE, refuse_retired_provider

_REPO = Path(__file__).resolve().parents[3]
# BOTH runtime emitters, not just the Python one. The axis tombstone is
# duplicated in Rust (`mail_inject.rs` keeps it "in lockstep", and `client.rs`
# raises its own copies), so a scan scoped to `cli/src/fno` passes green while
# the Rust half still promises the date - the "guard on one of N reachable
# paths is decorative" trap this very rename kept falling into.
_RUNTIME_ROOTS = (_REPO / "cli" / "src" / "fno", _REPO / "crates")
_SUFFIXES = (".py", ".rs")


def _is_cargo_build_artifact(path: Path) -> bool:
    """True only for `crates/<crate>/target/**` (cargo's build dir).

    Deliberately NOT a bare `"target" in path.parts`: the runtime package
    `cli/src/fno/target/` (manifest.py, denominator.py) lived under that exact
    name until the target family ported native, and the unanchored form
    silently dropped its Python files from this scan while it existed - the
    same over-exclusion trap the AGENTS.md pitfall corpus records, which this
    guard fell into while fixing a different decorative guard.
    """
    try:
        rel = path.relative_to(_REPO / "crates")
    except ValueError:
        return False
    return len(rel.parts) > 1 and rel.parts[1] == "target"


def test_cargo_build_artifact_exclusion_stays_anchored_to_crates():
    """The exclusion keys on the crates prefix, never the bare name.

    The positive control used to assert on the real `cli/src/fno/target/`
    package; the family port deleted that package, so the control pins the
    helper's shape directly: a same-named runtime path stays in scope and a
    crates build path stays out.
    """
    runtime_named = _REPO / "cli" / "src" / "fno" / "target" / "manifest.py"
    assert not _is_cargo_build_artifact(runtime_named)
    assert _is_cargo_build_artifact(_REPO / "crates" / "fno-agents" / "target" / "debug" / "x.py")
    assert not _is_cargo_build_artifact(_REPO / "crates" / "fno-agents" / "src" / "lib.rs")


def test_no_runtime_string_promises_a_0_4_0_removal():
    """0.3.x is the long-term home, so a 0.4.0 date is a promise that will not
    be kept. No runtime help string may carry one, in either language."""
    sources = [
        p
        for root in _RUNTIME_ROOTS
        for p in root.rglob("*")
        if p.suffix in _SUFFIXES and not _is_cargo_build_artifact(p)
    ]
    # A scan pointed at the wrong directory finds nothing and passes for the
    # wrong reason; anchor it so a moved test file fails loudly instead.
    assert len(sources) > 100, f"scan found only {len(sources)} files"
    assert any(p.suffix == ".rs" for p in sources), "Rust emitters not scanned"
    offenders = [
        f"{path.relative_to(_REPO)}:{n}"
        for path in sources
        for n, line in enumerate(path.read_text(errors="replace").splitlines(), 1)
        if "Removed at 0.4.0" in line
    ]
    assert offenders == [], f"runtime text promises a 0.4.0 removal: {offenders}"


def test_retired_provider_exits_2():
    with pytest.raises(typer.Exit) as excinfo:
        refuse_retired_provider("claude")
    assert excinfo.value.exit_code == 2
    # The refusal names what owns the vendor now.
    assert "the route owns the vendor" in PROVIDER_AXIS_TOMBSTONE
