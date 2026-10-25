"""Tests for cli/src/fno/setup/shim_check.py (x-c911)."""

import os
import stat
from pathlib import Path

import pytest

from fno.setup import shim_check
from fno.setup.shim_check import main, scan


def _make_durable(tmp_path: Path) -> Path:
    durable = tmp_path / "durable-bin"
    durable.mkdir()
    binary = durable / "fno-py"
    binary.write_text("#!/bin/sh\ntrue\n")
    binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
    return durable


def _dangling(bin_dir: Path, name: str = "fno-py") -> Path:
    link = bin_dir / name
    os.symlink(bin_dir.parent / "nowhere" / name, link)
    return link


@pytest.fixture()
def bin_dir(tmp_path: Path) -> Path:
    d = tmp_path / "bin"
    d.mkdir()
    return d


@pytest.fixture(autouse=True)
def no_cargo_bin(tmp_path, monkeypatch):
    """The real ~/.cargo/bin would answer as the durable copy of fno."""
    monkeypatch.setattr(shim_check, "CARGO_BIN", tmp_path / "absent-cargo" / "bin")


@pytest.fixture()
def no_durable_anywhere(tmp_path, monkeypatch):
    """Point every durable candidate at absent dirs: pytest's own venv or the
    real uv tools dir would otherwise answer as the durable copy."""
    empty = tmp_path / "empty"
    empty.mkdir()
    monkeypatch.setattr(shim_check.sys, "executable", str(empty / "python"))
    monkeypatch.setattr(shim_check, "UV_TOOL_FNO_BIN", tmp_path / "absent-uv" / "bin")


def test_dangling_fno_link_detected_and_unrelated_link_ignored(bin_dir, no_durable_anywhere):
    _dangling(bin_dir, "fno-py")
    os.symlink(bin_dir.parent / "nowhere" / "pyfiglet", bin_dir / "pyfiglet")

    report = scan(bin_dir)

    assert report["healthy"] is False
    defect = report["defects"][0]
    assert defect["name"] == "fno-py"
    assert defect["problem"] == "dangling"
    assert defect["repair"] is None, "no durable copy was ever created"
    assert not any(d["name"] == "pyfiglet" for d in report["defects"])


def test_link_into_the_running_venv_is_healthy_even_under_the_temp_root(bin_dir, tmp_path, monkeypatch):
    # The release smoke's topology: UV_TOOL_DIR under mktemp, so the tool venv
    # itself is under the temp root and the bin shims resolve there. The
    # running venv is the durable copy the repair arm targets; condemning it
    # made repair a no-op relink the re-scan condemned again, and the
    # installer aborted before its receipt.
    venv_bin = tmp_path / "uv-tools" / "fno" / "bin"
    venv_bin.mkdir(parents=True)
    (venv_bin / "fno-py").write_text("#!/bin/sh\ntrue\n")
    monkeypatch.setattr(shim_check.sys, "executable", str(venv_bin / "python"))
    monkeypatch.setattr(shim_check, "_temp_root", lambda: str(tmp_path))
    os.symlink(venv_bin / "fno-py", bin_dir / "fno-py")

    report = scan(bin_dir)

    assert report["healthy"] is True, report
    assert main(["--bin-dir", str(bin_dir)]) == 0


def test_temp_resolving_live_link_detected(bin_dir, tmp_path):
    staging = tmp_path / "staging"
    staging.mkdir()
    (staging / "fno-gh-proxy").write_text("#!/bin/sh\ntrue\n")
    os.symlink(staging / "fno-gh-proxy", bin_dir / "fno-gh-proxy")

    report = scan(bin_dir)

    assert report["healthy"] is False
    defect = report["defects"][0]
    assert defect["name"] == "fno-gh-proxy"
    assert defect["problem"] == "temp-resolving"


def test_absent_required_shim_restored_from_cargo_bin(bin_dir, tmp_path, monkeypatch, no_durable_anywhere):
    # The 2026-10-22 shape: fno and fno-gh-proxy gone from the tool bin, the
    # uv venv shipping neither, the durable copies only in the cargo bin.
    cargo = tmp_path / "cargo-bin"
    cargo.mkdir()
    (cargo / "fno-gh-proxy").write_text("#!/bin/sh\ntrue\n")
    (cargo / "fno-gh-proxy").chmod(0o755)
    monkeypatch.setattr(shim_check, "CARGO_BIN", cargo)
    monkeypatch.setattr(shim_check, "_temp_root", lambda: "/fno-no-temp-root")

    report = scan(bin_dir)

    # fno has no durable copy anywhere, so its absence is not a link defect.
    assert [(d["name"], d["problem"]) for d in report["defects"]] == [("fno-gh-proxy", "absent")]
    assert main(["--bin-dir", str(bin_dir), "--repair"]) == 0
    assert os.path.realpath(bin_dir / "fno-gh-proxy") == str((cargo / "fno-gh-proxy").resolve())


def test_main_exit_codes(bin_dir, tmp_path, monkeypatch):
    # The running-venv candidate is the durable bin: point sys.executable at
    # it. The real durable bin lives outside the temp root; in tests it lands
    # under pytest's tmp_path, so point the temp root away or the repaired
    # link reads as temp-resolving.
    durable = tmp_path / "durable-bin"
    monkeypatch.setattr(shim_check, "UV_TOOL_FNO_BIN", durable)
    monkeypatch.setattr(shim_check.sys, "executable", str(durable / "python"))
    monkeypatch.setattr(shim_check, "_temp_root", lambda: "/fno-no-temp-root")

    assert main(["--bin-dir", str(bin_dir)]) == 0

    _dangling(bin_dir)
    assert main(["--bin-dir", str(bin_dir)]) == 1

    _make_durable(tmp_path)
    assert main(["--bin-dir", str(bin_dir), "--repair"]) == 0
    assert os.path.realpath(bin_dir / "fno-py") == str(durable / "fno-py")
