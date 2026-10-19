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


def test_no_binary_is_noop() -> None:
    build_data: dict = {}
    out = hatch_build.apply_binary_bundle(None, build_data)
    assert out == {}  # pure-Python wheel: nothing touched


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


def test_binary_present_sets_py3_none_platform_tag(tmp_path, monkeypatch) -> None:
    monkeypatch.setattr(hatch_build, "_platform_tag", lambda: "macosx_11_0_arm64")
    build_data: dict = {}
    hatch_build.apply_binary_bundle([tmp_path / "fno-agents"], build_data)
    assert build_data["infer_tag"] is False
    assert build_data["tag"] == "py3-none-macosx_11_0_arm64"


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


# The events schema is no longer force-included: it lives at
# cli/src/fno/events/schema.yaml (package data) and ships naturally. The wheel
# smoke test (tests/smoke/test_build.sh) asserts it is present in a built wheel.


# -- ab-18563bcc US5: LICENSE + NOTICE bundling --


def test_license_sources_from_sdist(tmp_path) -> None:
    # No repo root above; the sdist vendored the licenses at the build root.
    vendor = tmp_path / hatch_build.LICENSE_SDIST_VENDOR_DIR
    vendor.mkdir()
    (vendor / "LICENSE").write_text("Apache-2.0\n")
    (vendor / "NOTICE").write_text("fno\n")
    found = hatch_build.license_sources(tmp_path)
    assert found == {"LICENSE": vendor / "LICENSE", "NOTICE": vendor / "NOTICE"}


def test_license_sources_prefers_repo_root(tmp_path) -> None:
    build_root = tmp_path / "cli"
    build_root.mkdir()
    (tmp_path / "LICENSE").write_text("Apache-2.0\n")
    (tmp_path / "NOTICE").write_text("fno\n")
    vendor = build_root / hatch_build.LICENSE_SDIST_VENDOR_DIR
    vendor.mkdir()
    (vendor / "LICENSE").write_text("stale\n")
    (vendor / "NOTICE").write_text("stale\n")
    # Direct-build location wins over the sdist vendor copy.
    found = hatch_build.license_sources(build_root)
    assert found["LICENSE"] == tmp_path / "LICENSE"
    assert found["NOTICE"] == tmp_path / "NOTICE"


def test_apply_license_bundle_composes_with_existing_force_include(tmp_path) -> None:
    # A pre-existing force_include entry (e.g. from another hook) must survive.
    lic = tmp_path / "LICENSE"
    lic.write_text("Apache-2.0\n")
    build_data: dict = {"force_include": {"/some/src": "pkg/other.txt"}}
    hatch_build.apply_license_bundle({"LICENSE": lic}, build_data)
    assert build_data["force_include"] == {
        "/some/src": "pkg/other.txt",
        str(lic): f"{hatch_build.LICENSE_REL_DEST_DIR}/LICENSE",
    }


def test_resolve_required_licenses_raises_when_missing(tmp_path) -> None:
    # A license-less wheel is non-compliant; a miss must raise, not return.
    build_root = tmp_path / "cli"
    build_root.mkdir()
    (tmp_path / "LICENSE").write_text("Apache-2.0\n")  # NOTICE missing
    with pytest.raises(FileNotFoundError, match="license files not found"):
        hatch_build.resolve_required_licenses(build_root)
