"""US4 CI drift guard tests (ab-acbde274) - `fno doctor lint shellout-drift`.

Covers the five BDD acceptance criteria plus the precision regressions that
keep the scope decisions honest (cost/paths_cli/worktree exclusions).
"""
from __future__ import annotations

import stat
from pathlib import Path


from fno import lint_shellout_drift as g


REPO_ROOT = Path(__file__).resolve().parents[3]


# --------------------------------------------------------------------------- #
# Fixture helpers
# --------------------------------------------------------------------------- #
def _make_module(scan_root: Path, relname: str, body: str) -> Path:
    path = scan_root / relname
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    return path


def _write_allowlist(tmp_path: Path, lines: list[str]) -> Path:
    al = tmp_path / "allow.txt"
    al.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return al


def _stub_fno(tmp_path: Path, name: str, script: str) -> list[str]:
    """Write an executable stub standing in for the `fno` CLI; return argv base."""
    p = tmp_path / name
    p.write_text(script, encoding="utf-8")
    p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return [str(p)]


# A command module that shells out to a resolve_repo_root()-rooted script (the
# canonical AC4-ERR shape).
BAD_COMMAND = '''
import subprocess
from fno.paths import resolve_repo_root

def go():
    script = resolve_repo_root() / "scripts" / "new.sh"
    subprocess.run(["bash", str(script)])
'''


# --------------------------------------------------------------------------- #
# AC4-ERR + AC4-UI: a new un-allowlisted shell-out fails with actionable output
# --------------------------------------------------------------------------- #


def test_ac4_ui_output_is_actionable(tmp_path):
    scan_root = tmp_path / "cli" / "src" / "fno"
    _make_module(scan_root, "badverb.py", BAD_COMMAND)
    al = _write_allowlist(tmp_path, [])

    report = g.run(repo_root=tmp_path, scan_root=scan_root, allowlist_path=al,
                   do_degrade=False)
    joined = "\n".join(report.lines)

    # file:line, the resolved relpath, and BOTH remediation paths.
    assert "badverb.py:" in joined  # file:line
    assert "scripts/new.sh" in joined  # resolved relpath
    assert "Remedy 1" in joined and "Remedy 2" in joined
    assert "eliminate the shell-out" in joined
    assert ".clone-only-scripts.txt" in joined


# --------------------------------------------------------------------------- #
# AC4-EDGE: an allowlist entry whose command does not degrade is rejected
# --------------------------------------------------------------------------- #
def test_ac4_edge_nondegrading_command_rejected_exit_zero(tmp_path):
    stub = _stub_fno(tmp_path, "fake-fno", "#!/bin/sh\nexit 0\n")
    failures = g.degrade_proof({"scripts/x.sh": ["bogus"]}, fno_cmd=stub)
    assert len(failures) == 1
    assert "exited 0" in failures[0].reason


def test_ac4_edge_traceback_command_rejected(tmp_path):
    stub = _stub_fno(tmp_path, "fake-fno",
                     "#!/bin/sh\necho 'Traceback (most recent call last):' >&2\nexit 1\n")
    failures = g.degrade_proof({"scripts/x.sh": ["bogus"]}, fno_cmd=stub)
    assert len(failures) == 1
    assert "traceback" in failures[0].reason.lower()


# --------------------------------------------------------------------------- #
# AC4-FR: the guard fails closed on its own error
# --------------------------------------------------------------------------- #
def test_ac4_fr_parse_failure_is_red(tmp_path):
    scan_root = tmp_path / "cli" / "src" / "fno"
    _make_module(scan_root, "broken.py", "def f(:\n    pass\n")  # SyntaxError
    al = _write_allowlist(tmp_path, [])
    report = g.run(repo_root=tmp_path, scan_root=scan_root, allowlist_path=al,
                   do_degrade=False)
    assert report.exit_code == 3, "\n".join(report.lines)
    assert "fail-closed" in "\n".join(report.lines).lower()


def test_ac4_fr_malformed_allowlist_is_red(tmp_path):
    scan_root = tmp_path / "cli" / "src" / "fno"
    _make_module(scan_root, "ok.py", "x = 1\n")
    al = _write_allowlist(tmp_path, ["this-line-has-no-separator"])
    report = g.run(repo_root=tmp_path, scan_root=scan_root, allowlist_path=al,
                   do_degrade=False)
    assert report.exit_code == 3
    assert "fail-closed" in "\n".join(report.lines).lower()


def test_ac4_fr_missing_allowlist_is_red(tmp_path):
    scan_root = tmp_path / "cli" / "src" / "fno"
    _make_module(scan_root, "ok.py", "x = 1\n")
    report = g.run(repo_root=tmp_path, scan_root=scan_root,
                   allowlist_path=tmp_path / "nope.txt", do_degrade=False)
    assert report.exit_code == 3


# --------------------------------------------------------------------------- #
# Precision regressions: the documented scope exclusions stay correct
# --------------------------------------------------------------------------- #


def test_segmented_and_fullstring_and_plugin_script_all_detected(tmp_path):
    """The three construction idioms are each caught."""
    scan_root = tmp_path / "cli" / "src" / "fno"
    mod = '''
import subprocess
from fno.paths import resolve_repo_root, resolve_plugin_script

A = "scripts/lib/full.sh"           # full string
def go():
    seg = resolve_repo_root() / "scripts" / "lib" / "seg.sh"   # segmented
    p = resolve_plugin_script("hooks/helpers/plug.sh")          # plugin script
    subprocess.run(["bash", str(seg)])
'''
    _make_module(scan_root, "threeforms.py", mod)
    al = _write_allowlist(tmp_path, [])
    report = g.run(repo_root=tmp_path, scan_root=scan_root, allowlist_path=al,
                   do_degrade=False)
    joined = "\n".join(report.lines)
    assert report.exit_code == 1
    assert "scripts/lib/full.sh" in joined
    assert "scripts/lib/seg.sh" in joined
    assert "hooks/helpers/plug.sh" in joined


def test_real_allowlist_parses_and_matches_real_scan():
    """The shipped allowlist parses and exactly covers the real tree's shell-outs
    (no stale entries, no missing entries) under a scan-only run."""
    entries = g.parse_allowlist(REPO_ROOT / g.ALLOWLIST_REL)
    assert entries, "allowlist should be non-empty"
    # scan with the real allowlist -> no violations (covers all) ...
    violations = g.scan_tree(REPO_ROOT / g.SCAN_REL, set(entries), REPO_ROOT)
    assert violations == [], [f"{v.file}:{v.line} {v.relpath}" for v in violations]
    # ... and scan with an EMPTY allowlist -> every flagged relpath is one we
    # listed (no allowlist entry is stale / unmatched by the tree).
    all_flagged = {v.relpath for v in g.scan_tree(REPO_ROOT / g.SCAN_REL, set(), REPO_ROOT)}
    assert set(entries) == all_flagged, (
        f"allowlist drift: listed-but-not-found={set(entries) - all_flagged}, "
        f"found-but-not-listed={all_flagged - set(entries)}"
    )
