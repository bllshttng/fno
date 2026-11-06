"""Tests for the sweeps and fail-closed controls of scripts/diagnostics/command-callers.py.

command-callers.py is the canonical caller-finding instrument (its iter_corpus
include-list is what avoids the skills/target rg-glob pitfall).
"""
from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "scripts" / "diagnostics" / "command-callers.py"


def _load():
    spec = importlib.util.spec_from_file_location("command_callers", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


vc = _load()


def test_rust_argv_sweep_credits_the_array_not_the_command(tmp_path):
    """The fourth shell shape, and the two ways of getting its filter wrong.

    A Rust shell-out puts the command inside an argv array with no binary token
    beside it, so the whitespace sweep credits nothing. The array is therefore
    the signal. Two directions matter and both are asserted here: an ENUMERATED
    foreign literal is skipped, and an UNKNOWN literal is credited, because a
    "skip anything that is not fno" rule deletes a live command the day a wrapper
    lands under a new name.
    """
    src = tmp_path / "crates" / "fno" / "src"
    src.mkdir(parents=True)
    (src / "sites.rs").write_text(
        'Command::new(fno_bin()).args(["plan", "fidelity", "--json"]);\n'
        # the qualifier shape that defeated keying on Command::new
        "tokio::process::Command::new(crate::digest_overlay::fno_agents_bin())\n"
        '    .args(["needs", "--json"]);\n'
        'Command::new("git").args(["notify"]);\n'
        'Command::new("candidate_fno").args(["mail", "send"]);\n'
    )
    (tmp_path / "crates" / "fno" / "target").mkdir()
    (tmp_path / "crates" / "fno" / "target" / "build.rs").write_text(
        'Command::new(fno_bin()).args(["backlog", "done"]);\n'
    )

    counts = vc.sweep_rust_argv(
        tmp_path, {"plan fidelity", "agents needs", "notify", "mail send", "backlog done"}
    )
    assert counts["plan fidelity"] == 1
    # the array reads ["needs"]; the leaf carries the `agents` the argv omits
    assert counts["agents needs"] == 1
    assert counts["notify"] == 0, "an enumerated foreign literal must be skipped"
    assert counts["mail send"] == 1, "an UNKNOWN literal must be credited, never skipped"
    assert counts["backlog done"] == 0, "crates/*/target is build output, not source"


def _broken_argv_sweep(tmp_path) -> Path:
    """A copy of the script whose argv-array pattern matches nothing."""
    broken = tmp_path / "command-callers.py"
    text = SCRIPT.read_text()
    needle = 'r"(?:\\.args\\(|\\bfno\\()\\s*(?:[^()\\[\\]]*,\\s*)?&?\\s*\\[([^\\]]*)\\]"'
    assert needle in text, "the argv-array pattern moved; update this test"
    broken.write_text(
        text.replace(needle, 'r"\\.NO_SUCH_CALL\\(\\[([^\\]]*)\\]"')
    )
    return broken


def test_rust_argv_controls_refuse_to_emit_a_list_when_the_sweep_breaks(tmp_path):
    """The RED path, pinned. A green-only test cannot tell a working gate from a dead one.

    Runs a COPY of the script with the argv-array pattern replaced by one that
    matches nothing, against the real corpus. The four controls must fail, the
    run must exit 2, and no dead set may be printed - an emitted list is the
    failure mode this control set exists to prevent.
    """
    proc = subprocess.run(
        [sys.executable, str(_broken_argv_sweep(tmp_path)), "--dead"],
        cwd=REPO_ROOT, capture_output=True, text=True, timeout=180,
    )
    assert proc.returncode == 2, f"a failed control must exit 2, got {proc.returncode}"
    out = proc.stdout + proc.stderr
    assert "no list emitted" in out, out
    for leaf in vc.RUST_ARGV_CONTROLS:
        assert f"rust-argv/{leaf}" in out, f"{leaf} not named in the refusal:\n{out}"
    assert "dead:" not in out, f"a broken sweep must emit NO candidate list:\n{out}"
