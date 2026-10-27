"""`fno doctor test rust` derives its thread cap from the native lanes door.

Every claim here is a positive marker: the flag in the built command, the
printed reading line. "The suite passed" proves nothing about the cap (the
defect this cap fixes was exactly that a green run hid an 18-worker pile-up),
so no test in this file treats a zero-exit as evidence. The lanes reading
itself is the native census's contract (Rust tests); these tests pin
`_lanes_threads` and own the cap/clamp/print behavior around it.
"""

from fno import test_cmd

MARKER_LOAD = 319.21
MARKER_CEILING = 96.0


def _fake_checkout(tmp_path, monkeypatch):
    """A repo-root-shaped tmp cwd with one crate to sweep."""
    monkeypatch.chdir(tmp_path)
    (tmp_path / "cli" / "src" / "fno").mkdir(parents=True)
    (tmp_path / "cli" / "src" / "fno" / "__init__.py").write_text("", encoding="utf-8")
    crate = tmp_path / "crates" / "alpha"
    crate.mkdir(parents=True)
    (crate / "Cargo.toml").write_text("[package]\n", encoding="utf-8")


def _capture_rust_cmds(monkeypatch, nextest=True):
    """Fake subprocess.run, fake nextest presence; returns the cmds list."""
    cmds: list[list[str]] = []

    class _Proc:
        returncode = 0

    def _record(cmd):
        argv = cmd.split() if isinstance(cmd, str) else list(cmd)
        if argv and argv[0] != "git":
            cmds.append(argv)
        return _Proc()

    class _GroupProc:
        def __init__(self, cmd, env=None, **kw):
            _record(cmd)

        def wait(self, timeout=None):
            return 0

    monkeypatch.setattr(test_cmd.subprocess, "run", lambda cmd, env=None, **kw: _record(cmd))
    monkeypatch.setattr(test_cmd.subprocess, "Popen", _GroupProc)
    monkeypatch.setattr(
        test_cmd.shutil,
        "which",
        lambda name: "/x/cargo-nextest" if (nextest and name == "cargo-nextest") else None,
    )
    return cmds


def _pin_lanes(monkeypatch, lane_count, refusal=""):
    """Pin the native lanes reading at the runner's edge (the one-read
    cache is bypassed the same way the old reader tests did)."""
    if lane_count is None:
        note = f"reading refused ({refusal})"
    else:
        note = f"{lane_count} more fit (fno doctor lanes)"
    monkeypatch.setattr(test_cmd, "_lanes_threads", lambda: (lane_count, note))


def _reset_lanes(monkeypatch):
    monkeypatch.setattr(test_cmd, "_LANES_READING", None)


# ---------------------------------------------------------------------------
# THE MARKER: the full production wiring, not a stubbed reader.
# ---------------------------------------------------------------------------


def test_marker_breached_ceiling_caps_threads_at_one(tmp_path, monkeypatch, capsys):
    """Load 319.21 against a 96.0 ceiling (status exceeded) - the measured
    receipt that motivated the cap - must both cap the threads at 1 and print
    the reading the cap was chosen from."""
    _fake_checkout(tmp_path, monkeypatch)
    _reset_lanes(monkeypatch)
    cmds = _capture_rust_cmds(monkeypatch)
    _pin_lanes(monkeypatch, 0)

    assert test_cmd._run_rust([]) == 0

    assert cmds, "no cargo command was built"
    cmd = cmds[0]
    assert cmd[:5] == ["cargo", "nextest", "run", "--test-threads", "1"]
    assert "--test-threads" not in cmd[5:]  # exactly one flag, in the base
    out = capsys.readouterr().out
    assert "0 more fit" in out
    assert "capped at 1" in out


def test_lanes_exceed_cpu_ceiling_caps_at_core_count(tmp_path, monkeypatch, capsys):
    """The bug this cap fixes: 54 lanes on a 12-core box must never become
    `--test-threads 54` - the CPU count is the hard ceiling, not the lane
    reading."""
    _fake_checkout(tmp_path, monkeypatch)
    _reset_lanes(monkeypatch)
    cmds = _capture_rust_cmds(monkeypatch)
    monkeypatch.setattr(test_cmd.os, "cpu_count", lambda: 12)
    _pin_lanes(monkeypatch, 54)

    assert test_cmd._run_rust([]) == 0
    assert cmds[0][:5] == ["cargo", "nextest", "run", "--test-threads", "12"]
    out = capsys.readouterr().out
    assert "54 more fit" in out
    assert "capped at 12 (requested 54, ceiling 12)" in out


def test_cargo_test_path_with_existing_separator_reuses_it(tmp_path, monkeypatch, capsys):
    """A caller's own `--` must not be duplicated: our flag rides inside the
    existing separator, ahead of the caller's binary args."""
    _fake_checkout(tmp_path, monkeypatch)
    _reset_lanes(monkeypatch)
    cmds = _capture_rust_cmds(monkeypatch, nextest=False)
    monkeypatch.setattr(test_cmd.os, "cpu_count", lambda: 12)
    _pin_lanes(monkeypatch, 3)

    assert test_cmd._run_rust(["--manifest-path", "crates/alpha/Cargo.toml", "--", "--nocapture"]) == 0
    assert cmds[0] == [
        "cargo",
        "test",
        "-q",
        "--manifest-path",
        "crates/alpha/Cargo.toml",
        "--",
        "--test-threads",
        "3",
        "--nocapture",
    ]
    assert cmds[0].count("--") == 1


def test_refused_reading_keeps_runner_default(tmp_path, monkeypatch, capsys):
    """A dark sensor is never headroom, but the suite must still run: no
    threads flag, and the note names the refusal."""
    _fake_checkout(tmp_path, monkeypatch)
    _reset_lanes(monkeypatch)
    cmds = _capture_rust_cmds(monkeypatch)
    _pin_lanes(monkeypatch, None, refusal="cpu arm dark")

    assert test_cmd._run_rust([]) == 0
    assert all("--test-threads" not in cmd for cmd in cmds)
    out = capsys.readouterr().out
    assert "reading refused" in out
    assert "cpu arm dark" in out


def test_space_separated_build_jobs_flag_is_clamped(tmp_path, monkeypatch, capsys):
    """`-j 32` (space-separated) must clamp the same as `-j32` (glued)."""
    _fake_checkout(tmp_path, monkeypatch)
    _reset_lanes(monkeypatch)
    cmds = _capture_rust_cmds(monkeypatch)
    monkeypatch.setattr(test_cmd.os, "cpu_count", lambda: 12)
    _pin_lanes(monkeypatch, 64)

    assert test_cmd._run_rust(["-j", "32"]) == 0
    assert cmds[0][cmds[0].index("-j") + 1] == "12"
    assert "build jobs 32 capped at 12" in capsys.readouterr().out


def test_explicit_test_threads_equals_form_is_clamped(tmp_path, monkeypatch, capsys):
    _fake_checkout(tmp_path, monkeypatch)
    _reset_lanes(monkeypatch)
    cmds = _capture_rust_cmds(monkeypatch)
    monkeypatch.setattr(test_cmd.os, "cpu_count", lambda: 12)
    _pin_lanes(monkeypatch, 64)

    assert test_cmd._run_rust(["--test-threads=54"]) == 0
    assert "--test-threads=12" in cmds[0]
    assert "explicit --test-threads 54 capped at 12" in capsys.readouterr().out
