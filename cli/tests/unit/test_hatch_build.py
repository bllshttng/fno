"""Unit tests for the wheel binary-bundling build hook (Phase 6 W6 Wave 3).

The end-to-end behavior (real `uv build` producing a platform wheel with the
binary on PATH vs a pure-Python wheel) is verified at build time; these tests
pin the pure decision logic so a regression surfaces without a full build.
"""
from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

# hatch_build.py lives at the cli/ project root, not inside the package.
_HOOK_PATH = Path(__file__).resolve().parents[2] / "hatch_build.py"
_spec = importlib.util.spec_from_file_location("hatch_build", _HOOK_PATH)
assert _spec and _spec.loader
hatch_build = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(hatch_build)


def test_all_binaries_bundle_as_scripts(tmp_path) -> None:
    # US6 + x-538e: the wheel ships the front door + client + daemon + worker,
    # each on PATH.
    binaries = [tmp_path / name for name in hatch_build.BINARY_NAMES]
    for b in binaries:
        b.write_text("#!/bin/sh\n")
    build_data: dict = {}
    hatch_build.apply_binary_bundle(binaries, build_data)
    assert build_data["shared_scripts"] == {str(b): b.name for b in binaries}
    # value is the basename so the script lands under its own name on PATH
    assert set(build_data["shared_scripts"].values()) == set(hatch_build.BINARY_NAMES)
    assert build_data["pure_python"] is False


# -- ab-18563bcc US6: all-or-nothing binary staging resolution --

def _stage(bin_dir, names) -> None:
    bin_dir.mkdir(parents=True, exist_ok=True)
    for name in names:
        (bin_dir / name).write_text("#!/bin/sh\n")


def test_resolve_binary_bundle_partial_hard_fails(tmp_path) -> None:
    # Only the front door staged: a release wheel must carry the complete set,
    # so a partial set is a build defect, not a degraded variant (AC6-ERR).
    _stage(tmp_path / "src" / "fno" / "_bin", [hatch_build.BINARY_NAMES[0]])
    with pytest.raises(FileNotFoundError, match="binary-complete wheel staging is incomplete"):
        hatch_build.resolve_binary_bundle(tmp_path)


# The events schema needs no build machinery: it left the wheel with the
# events package residue port and lives beside its Rust readers in
# crates/fno-agents/src/event_store/.


# -- ab-18563bcc US5: LICENSE + NOTICE bundling --
