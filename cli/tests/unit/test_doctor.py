"""Unit tests for `fno doctor` (ab-5a1fc285 + ab-a78c9731).

Covers US1 (detection: fresh / stale / unknown / json / no-source / probe-error),
US3-adjacent --fix behavior (delegates to `fno doctor update`, honors the IN_PROGRESS
guard), and US2 (rust staleness fold-in: full evidence mismatch -> stale,
partial evidence -> not stale, --fix rust-only leg runs the refresh helper,
never shells out to cargo directly).

The signal collectors (_resolve_source, _source_rev, _read_marker,
_probe_installed_command, _rust_report, _rust_source_rev,
_cargo_bin) are module-level so each test stubs them for a hermetic,
network-free verdict.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno import doctor
from fno.cli import app

runner = CliRunner()


def _stub_signals(
    monkeypatch: pytest.MonkeyPatch,
    *,
    src: Path | None,
    source_rev: str | None,
    marker: str | None,
    capture_present: str,  # ProbeResult: "present" | "missing" | "unknown"
    rust_binary: str | None = None,
    rust_marker: str | None = None,
    rust_source_rev: str | None = None,
    cargo_bin_present: bool = False,
    deployed_config_keys: frozenset[str] | None = frozenset({"backlog.id_prefix"}),
    source_config_keys: frozenset[str] | None = frozenset({"backlog.id_prefix"}),
    content_drift: int | None = 0,
    source_checkout_sync: dict | None = None,
    components: list[dict] | None = None,
) -> None:
    monkeypatch.setattr(doctor, "_resolve_source", lambda source: src)
    # Content-drift ground truth. Default 0 = "check ran, byte-identical" so a
    # fresh-marker test stays fresh; drift/indeterminate tests set >0 or None.
    # Never hashes the real installed package (hermetic).
    monkeypatch.setattr(doctor, "_python_content_drift", lambda source: content_drift)
    monkeypatch.setattr(doctor, "_source_rev", lambda source: source_rev)
    monkeypatch.setattr(doctor, "_read_marker", lambda: marker)
    monkeypatch.setattr(doctor, "_probe_installed_command", lambda: capture_present)
    # Config-schema surfaces (x-6c5b): default to EQUAL keysets so existing tests
    # exercise no drift; drift tests pass differing sets explicitly.
    monkeypatch.setattr(doctor, "_deployed_config_keys", lambda: deployed_config_keys)
    monkeypatch.setattr(doctor, "_source_config_keys", lambda source: source_config_keys)
    monkeypatch.setattr(
        doctor,
        "_source_checkout_sync",
        lambda source: source_checkout_sync or {
            "status": "current",
            "behind": 0,
            "source_head": "abc123",
            "remote_head": "abc123",
            "detail": "",
        },
    )
    # Post ab-716cd330 `revision` carries the binary's self-reported crates/ rev
    # (the verdict driver), not the installed-rust-rev marker. `rust_marker` is
    # the value the resolved binary reports here.
    monkeypatch.setattr(
        doctor,
        "_rust_report",
        lambda: {"binary": rust_binary, "revision": rust_marker},
    )
    monkeypatch.setattr(doctor, "_daemon_drift_warning", lambda: None)
    monkeypatch.setattr(doctor, "_rust_source_rev", lambda source: rust_source_rev)
    monkeypatch.setattr(doctor, "_cargo_bin_present", lambda: cargo_bin_present)
    # Component convergence (deployed-shape probes + native verdict): default
    # to "no evidence" so existing verdict tests keep their exact surface;
    # component tests pass rows explicitly.
    monkeypatch.setattr(
        doctor, "_component_convergence", lambda src, rust, marker, drift: components or []
    )
    # Agent health (x-1c7b): default to a quiet, healthy machine. Left unstubbed
    # these shell out to the real `launchctl` and read the real claims root, so
    # every verdict test would inherit the developer's own dead agents.
    monkeypatch.setattr(
        doctor,
        "_groom_health",
        lambda: {"state": "ran", "hours": 3.0, "stale": False, "agent_installed": True},
    )
    monkeypatch.setattr(
        doctor, "_launch_agent_failures", lambda: {"applicable": True, "dead": []}
    )
    # The mux-freshness and machine-fact probes read one native readiness
    # payload; seed it empty (unknown, never fresh) so no test inherits this
    # machine's live mux or checkout.
    monkeypatch.setattr(doctor, "_PROBES", {"mux_server_stale": []})
    # Control-plane arm staleness shells out to the real `fno-agents status
    # --json`. A developer machine with genuinely stale arms leaks STALE lines
    # into full-output assertions, so stub it like the other probes.
    monkeypatch.setattr(
        doctor,
        "_control_plane_arms_report",
        lambda: {"stale": [], "unknown_reason": None},
    )
    # Evals demand: build_report reads the real evals history through the
    # summary seam; pin it fresh and quiet so a developer machine's stale bank
    # cannot leak a STALE line into full-output assertions.
    monkeypatch.setattr("fno.paths.evals_history", lambda: Path("/evals/h.jsonl"))
    monkeypatch.setattr(
        "fno.evals.report.evals_health_summary",
        lambda _path, **_kw: {
            "regression_pass_rate": 1.0,
            "flake_count": 0,
            "regression_alarm": [],
            "age_days": 1.0,
            "stale": False,
            "never_ran": False,
        },
    )


# ---------------------------------------------------------------------------
# US1: detection
# ---------------------------------------------------------------------------


def test_ac1_hp_fresh_install_reports_healthy(monkeypatch: pytest.MonkeyPatch) -> None:
    """AC1-HP: marker == source HEAD => fresh, exit 0."""
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="abc123",
        marker="abc123",
        capture_present="present",
    )
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 0
    assert "up to date" in result.stdout


def test_ac1_err_missing_command_reports_skew_and_exits_nonzero(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """AC1-ERR: capability probe proves a missing command => stale, exit nonzero, names the command + remediation."""
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="abc123",
        marker="abc123",  # marker even matches, but the probe wins
        capture_present="missing",
    )
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code != 0
    assert "missing: backlog capture" in result.stdout
    assert "fno doctor update" in result.stdout


def test_source_checkout_behind_refuses_fix_before_repair(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="source",
        marker="installed",
        capture_present="present",
        source_checkout_sync={
            "status": "behind",
            "behind": 117,
            "source_head": "source",
            "remote_head": "remote",
            "detail": "",
        },
    )
    monkeypatch.setattr(doctor, "_post_merge_sync_health", lambda: {})

    monkeypatch.setattr(doctor, "_front_door", lambda: None)

    result = runner.invoke(app, ["doctor", "--fix"])

    assert result.exit_code == 1
    assert "behind origin/main" in result.stdout
    assert "refused" in result.stderr


def test_ac1_ui_json_is_single_object_on_stdout(monkeypatch: pytest.MonkeyPatch) -> None:
    """AC1-UI: --json emits one JSON object on stdout; human text to stderr."""
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="newsha",
        marker="oldsha",
        capture_present="present",
        rust_binary="/cargo/bin/fno-agents",
    )
    result = runner.invoke(app, ["doctor", "--json"])
    assert result.exit_code != 0  # stale
    # stdout must be a single parseable JSON object with the contract fields.
    payload = json.loads(result.stdout.strip())
    assert payload["status"] == "stale"
    assert payload["python_stale"] is True
    assert payload["rust_stale"] is False
    assert payload["missing_commands"] == []
    assert payload["source_rev"] == "newsha"
    assert payload["installed_rev"] == "oldsha"
    assert payload["rust_binary"] == "/cargo/bin/fno-agents"
    # Human/metadata text is on stderr, not mixed into the JSON stdout.
    assert "fno doctor" in result.stderr


def test_ac1_edge_no_source_degrades_to_unknown(monkeypatch: pytest.MonkeyPatch) -> None:
    """AC1-EDGE: no resolvable source => unknown, exit 0 (cannot prove stale)."""
    _stub_signals(
        monkeypatch,
        src=None,
        source_rev=None,
        marker="abc123",
        capture_present="present",
    )
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 0
    assert "no source checkout to compare against" in result.stdout


def test_daemon_drift_probe_uses_installed_status_and_relays_canonical_warning(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from fno import rust_binary

    warning = (
        "fno agents: the running daemon (pid 91627) is an older build than the installed "
        "binary; run `fno agents restart` to pick up the new build (it restarts the "
        "daemon only and keeps PTY workers)."
    )
    calls: list[tuple[list[str], dict]] = []
    monkeypatch.setattr(
        rust_binary, "resolve_installed_binary", lambda: Path("/cargo/bin/fno-agents")
    )

    def fake_run(cmd, **kwargs):
        calls.append((cmd, kwargs))
        return type(
            "Completed",
            (),
            {
                "returncode": 0,
                "stdout": '{"drift": "drifted", "daemon": {"pid": 91627}}',
                "stderr": warning,
            },
        )()

    monkeypatch.setattr(doctor.subprocess, "run", fake_run)
    assert doctor._daemon_drift_warning() == warning
    assert calls == [
        (
            ["/cargo/bin/fno-agents", "status", "--json"],
            {"capture_output": True, "text": True, "check": False, "timeout": 5},
        )
    ]


@pytest.mark.parametrize(
    "returncode,stdout,stderr",
    [
        (13, "", "fno-agents: daemon not running"),
        (
            0,
            '{"drift": "fresh", "daemon": {"pid": 7}}',
            "fno agents: the running daemon (pid 7) is an older build than the installed "
            "binary; run `fno agents restart` to pick up the new build (it restarts the "
            "daemon only and keeps PTY workers).",
        ),
    ],
)
def test_daemon_drift_probe_fails_silent_without_proven_status(
    monkeypatch: pytest.MonkeyPatch,
    returncode: int,
    stdout: str,
    stderr: str,
) -> None:
    from fno import rust_binary

    monkeypatch.setattr(
        rust_binary, "resolve_installed_binary", lambda: Path("/cargo/bin/fno-agents")
    )
    monkeypatch.setattr(
        doctor.subprocess,
        "run",
        lambda *args, **kwargs: type(
            "Completed", (), {"returncode": returncode, "stdout": stdout, "stderr": stderr}
        )(),
    )
    assert doctor._daemon_drift_warning() is None


# ---------------------------------------------------------------------------
# Python config-schema drift
# ---------------------------------------------------------------------------


_FLAT_REGISTRY = (
    "from dataclasses import dataclass\n"
    "@dataclass\n"
    "class Meta:\n"
    "    doc: str\n"
    'FIELD_META: dict[str, Meta] = {\n'
    '    "project.id": Meta("x"),\n'
    '    "backlog.id_prefix": Meta("y"),\n'
    "}\n"
)


def test_parse_field_meta_keys_spread_returns_none() -> None:
    """A `**spread` (or computed key) can't be read completely => None, never a
    truncated set that would risk a false 'fresh'."""
    spread = 'BASE = {"a.b": 1}\nFIELD_META = {**BASE, "backlog.id_prefix": 2}\n'
    assert doctor._parse_field_meta_keys(spread) is None
    computed = 'K = "x"\nFIELD_META = {K: 1}\n'
    assert doctor._parse_field_meta_keys(computed) is None


def _init_git_source(root: Path, registry_text: str) -> None:
    """Commit a registry.py into a throwaway git repo laid out like the cli source."""

    reg = root / "src" / "fno" / "config" / "registry.py"
    reg.parent.mkdir(parents=True)
    reg.write_text(registry_text, encoding="utf-8")
    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "t",
        "GIT_AUTHOR_EMAIL": "t@t",
        "GIT_COMMITTER_NAME": "t",
        "GIT_COMMITTER_EMAIL": "t@t",
    }
    for cmd in (["init", "-q"], ["add", "-A"], ["commit", "-qm", "init"]):
        subprocess.run(["git", "-C", str(root), *cmd], check=True, env=env)


def test_source_config_keys_reads_committed_head(tmp_path: Path) -> None:
    """The source keyset comes from committed HEAD, NOT the dirty working tree: an
    uncommitted edit must not leak into the verdict (matches _source_rev semantics)."""
    _init_git_source(tmp_path, _FLAT_REGISTRY)
    assert doctor._source_config_keys(tmp_path) == frozenset(
        {"project.id", "backlog.id_prefix"}
    )
    # Add a key in the WORKING TREE only (no commit). HEAD is unchanged, so the
    # committed keyset must not include it - else a dirty checkout false-STALEs.
    reg = tmp_path / "src" / "fno" / "config" / "registry.py"
    reg.write_text(
        _FLAT_REGISTRY.replace("}\n", '    "batch.enabled": Meta("z"),\n}\n'),
        encoding="utf-8",
    )
    assert doctor._source_config_keys(tmp_path) == frozenset(
        {"project.id", "backlog.id_prefix"}
    )


# ---------------------------------------------------------------------------
# _verdict: pure decision matrix
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "kwargs,expected_status,expected_python_stale,expected_rust_stale",
    [
        # Existing rows: no rust evidence passed - rust_stale must stay False.
        (dict(source_resolved=True, source_rev="a", marker="a", capture_present="present"), "fresh", False, False),
        (dict(source_resolved=True, source_rev="a", marker="b", capture_present="present"), "stale", True, False),
        (dict(source_resolved=True, source_rev="a", marker=None, capture_present="present"), "unknown", False, False),
        # Probe proves missing command => stale regardless of source/marker.
        (dict(source_resolved=True, source_rev="a", marker="a", capture_present="missing"), "stale", True, False),
        # Rust fold-in: full evidence mismatch + python fresh -> rust_stale True, status stale.
        (dict(source_resolved=True, source_rev="a", marker="a", capture_present="present",
              rust_installed_rev="aaa", rust_source_rev="bbb", cargo_bin_present=True), "stale", False, True),
        # Rust fold-in: partial evidence (no cargo bin) -> not stale.
        (dict(source_resolved=True, source_rev="a", marker="a", capture_present="present",
              rust_installed_rev="aaa", rust_source_rev="bbb", cargo_bin_present=False), "fresh", False, False),
        # Config drift: source defines a key deployed lacks, revs match -> python stale.
        (dict(source_resolved=True, source_rev="a", marker="a", capture_present="present",
              deployed_config_keys=frozenset({"a"}), source_config_keys=frozenset({"a", "b"})),
         "stale", True, False),
        # Config partial evidence (source keyset unknown) -> not stale.
        (dict(source_resolved=True, source_rev="a", marker="a", capture_present="present",
              deployed_config_keys=frozenset({"a"}), source_config_keys=None), "fresh", False, False),
    ],
)
def test_verdict_matrix(kwargs, expected_status, expected_python_stale, expected_rust_stale) -> None:
    v = doctor._verdict(**kwargs)
    assert v["status"] == expected_status
    assert v["python_stale"] is expected_python_stale
    assert v["rust_stale"] is expected_rust_stale


# ---------------------------------------------------------------------------
# x-8c3b: --fix acts on a dead pr-watch verdict (advisory, never flips exit)
# ---------------------------------------------------------------------------


def _dead_pr_watch(monkeypatch) -> None:
    monkeypatch.setattr(
        doctor,
        "_pr_watch_liveness",
        lambda: {
            "enabled": True, "verdict": "dead", "detail": "no tick recorded",
            "fix": "fno do pr watch install", "loaded": True, "last_tick": None,
        },
    )


def test_fix_heals_dead_pr_watch_on_fresh_binary(monkeypatch: pytest.MonkeyPatch) -> None:
    """A fresh binary with a dead watcher still heals it under --fix, exit 0."""
    _stub_signals(monkeypatch, src=Path("/src"), source_rev="abc", marker="abc",
                  capture_present="present")
    _dead_pr_watch(monkeypatch)
    import subprocess

    import fno.rust_binary as rb
    monkeypatch.setattr(rb, "resolve_binary", lambda: "/x/fno-agents")

    spawn: list = []
    real_run = subprocess.run

    def _fake_run(argv, **kw):
        if argv[:3] == ["/x/fno-agents", "pr-watch", "heal"]:
            spawn.append(argv)
            return subprocess.CompletedProcess(
                argv, 0, stdout="pr-watch heal: bounced x\n", stderr=""
            )
        return real_run(argv, **kw)

    monkeypatch.setattr(subprocess, "run", _fake_run)

    result = runner.invoke(app, ["doctor", "--fix"])
    assert result.exit_code == 0  # advisory: a dead watcher never flips the exit
    assert len(spawn) == 1
    assert spawn[0][1:3] == ["pr-watch", "heal"]
    assert "doctor-fix" in spawn[0]
    assert "pr-watch heal" in result.stderr


def _wedged_pr_watch(monkeypatch) -> None:
    monkeypatch.setattr(
        doctor,
        "_pr_watch_liveness",
        lambda: {
            "enabled": True, "verdict": "wedged",
            "detail": "last tick 25s ago but each of the last 3 ticks ended broken",
            "fix": "fno do pr watch refresh", "loaded": True,
            "last_tick": "2026-09-11T00:00:00Z", "interval_seconds": 600,
        },
    )


def test_fix_refreshes_wedged_pr_watch_instead_of_healing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A wedged watermark is fresh, so the tick already runs: the cure is the
    refresh command, never the plain bounce."""
    _stub_signals(monkeypatch, src=Path("/src"), source_rev="abc", marker="abc",
                  capture_present="present")
    _wedged_pr_watch(monkeypatch)
    import subprocess

    import fno.rust_binary as rb
    monkeypatch.setattr(rb, "resolve_binary", lambda: "/x/fno-agents")

    spawn: list = []
    real_run = subprocess.run

    def _fake_run(argv, **kw):
        if argv[:3] == ["/x/fno-agents", "pr-watch", "heal"]:
            raise AssertionError("wedged must re-render the plist, not bounce it")
        if argv[:3] == ["/x/fno-agents", "pr-watch", "refresh"]:
            spawn.append(argv)
            return subprocess.CompletedProcess(
                argv, 0, stdout="pr-watch refresh: re-rendered and bounced x\n", stderr=""
            )
        return real_run(argv, **kw)

    monkeypatch.setattr(subprocess, "run", _fake_run)

    result = runner.invoke(app, ["doctor", "--fix"])
    assert result.exit_code == 0  # advisory: never flips the exit
    assert len(spawn) == 1
    assert spawn[0][1:3] == ["pr-watch", "refresh"]
    assert "--force-bounce" in spawn[0]
    assert "doctor-fix" in spawn[0]
    assert "pr-watch refresh" in result.stderr


def test_ac3_hp_fix_delegates_to_update(monkeypatch: pytest.MonkeyPatch) -> None:
    """AC3-HP: --fix on a stale Python install delegates to `fno doctor update`."""
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="newsha",
        marker="oldsha",
        capture_present="present",
    )
    calls: list[list[str]] = []

    def _fake_run(door, source):
        calls.append([door, "doctor", "update", *(["--source", str(source)] if source else [])])
        return 0

    monkeypatch.setattr(doctor, "_front_door", lambda: "/fake/fno")
    monkeypatch.setattr(doctor, "_run_update_command", _fake_run)
    result = runner.invoke(app, ["doctor", "--fix", "--source", "/src"])
    assert calls == [["/fake/fno", "doctor", "update", "--source", "/src"]]
    assert result.exception is None


def test_ac3_edge_fix_reports_the_update_exit_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """--fix reports the native update command's exit code; the command's own
    IN_PROGRESS guard and source-pin gate are what refuse."""
    _stub_signals(
        monkeypatch,
        src=None,
        source_rev=None,
        marker=None,
        capture_present="missing",
    )

    def _refusing_update(door, source):
        return 1

    monkeypatch.setattr(doctor, "_front_door", lambda: "/fake/fno")
    monkeypatch.setattr(doctor, "_run_update_command", _refusing_update)

    result = runner.invoke(app, ["doctor", "--fix"])
    assert result.exit_code == 1


# ---------------------------------------------------------------------------
# Fix C1: _emit_human never prints STALE for non-cargo binaries
# ---------------------------------------------------------------------------


def test_ac2_ui_non_cargo_binary_never_reports_stale(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Fix C1: a bundled-wheel/PATH binary with a leftover marker mismatch must
    NOT print STALE. The JSON verdict has rust_stale: false (no cargo bin), so
    human output must not contradict it.

    cargo_bin_present=False + marker mismatch -> exit 0, output contains
    "not tracked", does NOT contain "STALE".
    """
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="xyz",
        marker="xyz",
        capture_present="present",
        rust_binary="/wheel/_bin/fno-agents",
        rust_marker="aaa",       # mismatch evidence but unproven
        rust_source_rev="bbb",
        cargo_bin_present=False,  # no cargo bin -> rust_stale is False in verdict
    )
    # Same reasoning as the tripwire test below: the control-plane arms readout
    # (`fno-agents status --json`) is a legit advisory subprocess, and the claim
    # door may legitimately put a binary in this test's PATH. Stub it so the
    # readout's environmental arm state cannot masquerade as the rust-staleness
    # contradiction this test pins.
    monkeypatch.setattr(
        doctor, "_control_plane_arms_report", lambda: {"arms": [], "stale": [], "unknown_reason": None}
    )
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 0, f"exit code {result.exit_code}, output: {result.stdout}{result.stderr}"
    combined = result.stdout + result.stderr
    assert "STALE" not in combined, (
        f"Non-cargo binary must never produce STALE output. Got:\n{combined}"
    )
    assert "not tracked" in combined, (
        f"Expected 'not tracked' for non-cargo binary. Got:\n{combined}"
    )


def test_control_plane_arms_report_consumes_the_rust_attention_set(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """x-6484: `_control_plane_arms_report` passes the Rust-owned
    `arms_attention` rows through as `red` and never re-derives the verdict
    from the legacy `stale`/`failing` booleans: a stale row left out of the
    Rust selection stays unreported.
    """
    from fno import rust_binary

    observed_fresh = {"arm": "watchdog", "stale": False, "failing": False,
                      "producer_evidence": "observed", "line": "watchdog ok"}
    stale_left_out = {"arm": "reap", "stale": True, "failing": False,
                      "producer_evidence": "observed"}
    unobserved = {"arm": "lead_wake", "stale": False, "failing": False,
                  "producer_evidence": "unobserved",
                  "line": "lead_wake         UNOBSERVED     never via=launchd"}
    payload = json.dumps({"arms": [observed_fresh, stale_left_out, unobserved],
                          "arms_attention": [unobserved]})

    class _FakeResult:
        stdout = payload

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: Path("/bin/fno-agents"))
    monkeypatch.setattr(
        doctor.subprocess, "run",
        lambda *a, **kw: _FakeResult(),
    )
    report = doctor._control_plane_arms_report()
    assert report["unknown_reason"] is None
    assert report["red"] == [unobserved], f"Got: {report}"


# ---------------------------------------------------------------------------
# ab-24a59d50: binary self-reported git rev (build.rs embed)
# ---------------------------------------------------------------------------


def test_rust_report_revision_comes_from_cargo_not_resolved(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # codex PR #491: a bundled sibling resolves, but the verdict rev must come
    # from the cargo binary the gate (_cargo_bin_present) + --fix target.
    from fno import rust_binary

    monkeypatch.setattr(
        rust_binary, "resolve_installed_binary", lambda: Path("/bundled/fno-agents")
    )
    monkeypatch.setattr(doctor, "_cargo_bin_path", lambda: "/cargo/bin/fno-agents")

    def fake_version_json(binary: str | None) -> dict:
        if binary == "/bundled/fno-agents":
            return {"git_rev": "bbbbbbbbbbbb", "crates_rev": "bundledcrates"}
        return {"git_rev": "cccccccccccc", "crates_rev": "cargocrates12"}

    monkeypatch.setattr(doctor, "_binary_version_json", fake_version_json)
    report = doctor._rust_report()
    assert report["binary"] == "/bundled/fno-agents"  # display = resolved binary
    assert report["binary_rev"] == "bbbbbbbbbbbb"  # informational = resolved HEAD
    assert report["revision"] == "cargocrates12"  # verdict = CARGO crates rev


# --- x-c267: mux front-door health (advisory) ---


@pytest.mark.parametrize(
    "mux, which_fno, probe, expected",
    [
        # `fno` on PATH but not a mux (probe says no) + no cargo mux -> not-installed
        (None, "/home/x/.local/bin/fno", False, "not-installed"),
        # custom --root mux: not at $CARGO_HOME/bin, but `fno` on PATH answers the
        # mux command -> active (this is the case the old code mislabeled not-installed)
        (None, "/custom/root/bin/fno", True, "active"),
        # cargo mux installed but a non-mux `fno` wins PATH -> shadowed
        ("/home/x/.cargo/bin/fno", "/home/x/.local/bin/fno", False, "shadowed"),
    ],
)
def test_mux_front_door_report_states(
    monkeypatch: pytest.MonkeyPatch, mux, which_fno, probe, expected
) -> None:
    """Front-door state: active when `fno` on PATH is the mux (== cargo binary OR
    answers the mux command, catching custom --root); shadowed when a cargo mux
    exists but isn't the `fno` on PATH; not-installed otherwise."""
    monkeypatch.setattr(doctor, "_cargo_installed_mux", lambda: Path(mux) if mux else None)
    monkeypatch.setattr(doctor.shutil, "which", lambda name: which_fno)
    monkeypatch.setattr(doctor, "_probe_is_mux", lambda p: probe)
    report = doctor._mux_front_door_report()
    assert report["mux_front_door"] == expected
    assert report["mux_binary"] == (mux if mux else None)
    assert report["path_fno"] == which_fno


# ---------------------------------------------------------------------------
# Content drift: ground-truth Python freshness (catches a lying installed-rev
# marker after a cache-hit reinstall).
# ---------------------------------------------------------------------------


def test_content_drift_overrides_fresh_marker(monkeypatch: pytest.MonkeyPatch) -> None:
    """The regression: marker == source HEAD (rev check says fresh) but installed
    bytes differ -> STALE, exit nonzero, message names the file count. This is the
    month-old-install-behind-a-HEAD-marker case."""
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="abc123",
        marker="abc123",  # marker agrees with HEAD - the lie
        capture_present="present",
        content_drift=3,  # but 3 .py files on disk differ
    )
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 1
    assert "STALE" in result.stdout
    assert "3 .py file" in result.stdout


def test_content_indeterminate_downgrades_fresh_to_unknown(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An undeterminable content check (None) must NOT leave a marker-only fresh
    standing: the marker can lie about a cache-hit reinstall, so downgrade to
    unknown. It never flips to stale (no positive drift proven)."""
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="abc123",
        marker="abc123",  # marker matches -> rev check alone would say fresh
        capture_present="present",
        content_drift=None,  # but the ground-truth check could not run
    )
    result = runner.invoke(app, ["doctor", "--json"])
    payload = json.loads(result.stdout)
    assert payload["status"] == "unknown"
    assert payload["content_stale"] is False
    assert payload["content_indeterminate"] is True
    assert payload["content_drift_count"] is None


def test_python_content_drift_counts_differing_py_files(tmp_path: Path) -> None:
    """_python_content_drift fingerprints installed vs source/src/fno and counts
    only files whose bytes differ; identical files do not count."""
    inst = tmp_path / "installed" / "fno"
    src = tmp_path / "source"
    src_pkg = src / "src" / "fno"
    inst.mkdir(parents=True)
    src_pkg.mkdir(parents=True)
    (inst / "same.py").write_text("x = 1\n")
    (src_pkg / "same.py").write_text("x = 1\n")
    (inst / "drift.py").write_text("old = True\n")
    (src_pkg / "drift.py").write_text("old = False\n")  # differs
    (src_pkg / "added.py").write_text("new = 1\n")  # only in source

    import fno as _fno_pkg

    # Point _installed_pkg_dir at our fake installed tree.
    orig_file = _fno_pkg.__file__
    try:
        _fno_pkg.__file__ = str(inst / "__init__.py")
        assert doctor._python_content_drift(src) == 2  # drift.py + added.py
    finally:
        _fno_pkg.__file__ = orig_file


# ---------------------------------------------------------------------------
# Agent health (x-1c7b): four grooming surfaces shipped and never ran, every
# time because nothing reported the silence.
# ---------------------------------------------------------------------------


def _fresh(monkeypatch: pytest.MonkeyPatch) -> None:
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="abc123",
        marker="abc123",
        capture_present="present",
    )


@pytest.mark.parametrize(
    "dead_row, expect_exit_text",
    [
        ({"label": "sh.fno.pr-watcher", "exit": 78}, "78"),
    ],
)
def test_dead_launch_agent_is_named_with_its_exit_and_reddens_doctor(
    monkeypatch: pytest.MonkeyPatch,
    dead_row: dict,
    expect_exit_text: str,
) -> None:
    """AC1-ERR: an installed-but-failing agent must not be a quiet line."""
    _fresh(monkeypatch)
    monkeypatch.setattr(
        doctor,
        "_launch_agent_failures",
        lambda: {"applicable": True, "dead": [dead_row]},
    )
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 1, "a dead agent must fail the exit code, not just print"
    assert "sh.fno.pr-watcher" in result.stdout
    assert expect_exit_text in result.stdout


def test_fix_installs_the_groom_agent_when_nothing_schedules_it(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _fresh(monkeypatch)
    monkeypatch.setattr(
        doctor,
        "_groom_health",
        lambda: {"state": "never", "hours": None, "stale": True, "agent_installed": False},
    )
    monkeypatch.setattr(doctor.sys, "platform", "darwin")  # the install is launchd-only
    calls: list = []
    monkeypatch.setattr(
        doctor,
        "_groom_install_agent",
        lambda: calls.append(1) or {"status": "installed", "detail": "ok"},
    )
    result = runner.invoke(app, ["doctor", "--fix"])
    assert len(calls) == 1
    assert "--fix groom agent: installed" in result.stderr


def test_agent_scan_refuses_closed_without_a_binary(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An unreadable fold reads not-applicable and fabricates no alarm."""
    import fno.rust_binary as rust_binary

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: None)
    assert doctor._launch_agent_failures() == {"applicable": False, "dead": []}


def _short_codex_home(monkeypatch):
    """A CODEX_HOME short enough for the 104-char AF_UNIX bind limit (pytest's
    tmp_path on macOS is not; dir="/tmp" escapes the long TMPDIR). Caller
    cleans up the returned parent."""
    import tempfile

    home = tempfile.mkdtemp(prefix="fno-x571-", dir="/tmp")
    monkeypatch.setenv("CODEX_HOME", home)
    socket_dir = Path(home) / "app-server-control"
    socket_dir.mkdir(parents=True)
    return socket_dir / "app-server-control.sock", home


def test_codex_app_server_report_dead_socket_inode_reads_absent(tmp_path, monkeypatch):
    """The measured specimen: a real socket inode whose creating process is
    gone. The file exists, nothing listens, the probe reads absent."""
    import shutil
    import socket as _socket

    monkeypatch.setattr(doctor, "_CODEX_APP_SERVER_PROBE_TIMEOUT_S", 0.5)
    sock_path, home = _short_codex_home(monkeypatch)
    try:
        listener = _socket.socket(_socket.AF_UNIX, _socket.SOCK_STREAM)
        listener.bind(str(sock_path))
        listener.close()  # inode remains on disk; no process listens
        assert sock_path.exists()
        assert doctor._codex_app_server_report()["present"] is False
    finally:
        shutil.rmtree(home, ignore_errors=True)


# ---------------------------------------------------------------------------
# Deployed fno plugin roots freshness: the Rust command enumerates every root
# (marketplace stage, registry installPath, orphan copies) and byte-checks
# each against source HEAD. Same fresh|stale|unknown vocabulary; stale only
# on proven evidence.
# ---------------------------------------------------------------------------


def _root_verdict(path: str, status: str, live: bool, **extra) -> dict:
    drift = extra.get("differing_count", 0) + extra.get("missing_count", 0)
    lag = f"hooks {drift}"
    note = f" ({drift} file(s) differ from source HEAD: {lag})" if drift else ""
    if status == "stale" and not live:
        note += ". Fix: cd /src && fno config plugin install claude removes the stale second copy"
    blocker = None
    if status == "stale":
        blocker = (
            f"plugin root {path} ({'live' if live else 'second copy'}) differs from "
            f"source HEAD in {drift} file(s) ({lag}; e.g. {(extra.get('sample') or ['?'])[0]}). "
            "Fix: cd /src && fno config plugin install claude"
        )
    verdict = {
        "path": path,
        "origin": "marketplace" if live else "registry",
        "live": live,
        "kind": "stage",
        "status": status,
        "source": "/src",
        "source_head": "a" * 40,
        "differing_count": 0,
        "missing_count": 0,
        "sample": [],
        "remedy": "cd /src && fno config plugin install claude",
        "detail": None,
        "note": note,
        "blocker": blocker,
    }
    verdict.update(extra)
    return verdict


def _roots_stdout(roots: list[dict], detail: str | None = None) -> str:
    live = next((r for r in roots if r["live"]), None)
    rank = {"fresh": 0, "absent": 0, "unknown": 1, "stale": 2}
    worst = max(roots, key=lambda r: rank.get(r["status"], 1), default=None)
    return json.dumps(
        {
            "status": (worst or {"status": "unknown"})["status"],
            "sha": (live or {}).get("sha"),
            "installed_at": None,
            "kind": "stage" if (live or not roots) else None,
            "stage": (live or {}).get("path"),
            "remedy": (live or {}).get("remedy"),
            "detail": detail,
            "roots": roots,
        }
    )


def test_plugin_cache_multi_root_folds_worst_and_names_cache(tmp_path, monkeypatch):
    """Two roots with the non-live one stale: the fold keeps both under
    roots, reads status stale, points the flat keys at the live root, and
    the blocker names the stale second copy."""
    monkeypatch.setattr(doctor, "_resolve_source", lambda source: tmp_path)
    monkeypatch.setattr(doctor, "_cargo_bin_path", lambda: "fno-agents-stub")
    monkeypatch.setattr(
        doctor,
        "_run_stage_check",
        lambda argv: (
            3,
            _roots_stdout(
                [
                    _root_verdict("/stage/fno", "fresh", True),
                    _root_verdict(
                        "/claude/plugins/cache/footnote/fno/0.3.2",
                        "stale",
                        False,
                        differing_count=1422,
                        sample=["hooks/lead-delegation-guard.sh"],
                    ),
                ]
            ),
            "",
        ),
    )

    report = doctor._plugin_cache_report()

    assert report["status"] == "stale"
    assert report["kind"] == "stage"
    assert report["stage"] == "/stage/fno"
    assert len(report["roots"]) == 2
    assert any("/claude/plugins/cache/footnote" in r["path"] for r in report["roots"])
    assert "fno config plugin install claude" in report["remedy"]
    blockers = doctor._blockers({"plugin_cache": report})
    assert any("second copy" in b and "1422 file(s)" in b for b in blockers)
    assert any("hooks/lead-delegation-guard.sh" in b for b in blockers)


def test_disk_free_blocker_fires_under_the_floor_and_names_the_reclaim(monkeypatch):
    """A free-disk reading under the floor is a blocker naming the reclaim;
    at or above the floor it is not, and an unreadable volume blocks nothing."""
    import shutil as real_shutil

    def usage(free_gb):
        total = 1_000_000_000_000
        free = int(free_gb * 1_000_000_000)
        return real_shutil._ntuple_diskusage(total, total - free, free)

    monkeypatch.setattr(doctor.shutil, "disk_usage", lambda path: usage(10.0))
    report = doctor._disk_free_report()
    assert report["verdict"] == "low"
    blockers = doctor._blockers({"disk_free": report})
    assert any("free disk is 10.0 GB" in b and "reclaim --apply" in b for b in blockers)

    monkeypatch.setattr(doctor.shutil, "disk_usage", lambda path: usage(40.0))
    assert doctor._disk_free_report()["verdict"] == "ok"
    assert doctor._blockers({"disk_free": {"verdict": "ok"}}) == []

    def boom(path):
        raise OSError("no volume")

    monkeypatch.setattr(doctor.shutil, "disk_usage", boom)
    assert doctor._disk_free_report()["verdict"] == "unreadable"
    assert doctor._blockers({"disk_free": {"verdict": "unreadable"}}) == []


def test_old_git_echo_falls_back_and_doctor_names_the_git(monkeypatch):
    """git under 2.31 echoes --path-format=absolute back as a line and exits 0.
    The common-dir reader refuses that text, reads plain, and joins it to cwd;
    doctor blocks on the version so the cause is named."""
    from fno.paths import git_common_dir

    def old_git(argv, **kwargs):
        out = "--path-format=absolute\n.git\n" if "--path-format=absolute" in argv else ".git\n"
        return subprocess.CompletedProcess(argv, 0, stdout=out, stderr="")

    assert git_common_dir("/work/repo", runner=old_git) == Path("/work/repo/.git")

    monkeypatch.setattr(doctor, "_bounded_command", lambda argv: (0, "git version 2.26.2\n", ""))
    report = doctor._git_version_report()
    assert report == {"version": "2.26", "verdict": "old"}
    assert any("git 2.26 is under 2.31" in b for b in doctor._blockers({"git_version": report}))
    monkeypatch.setattr(doctor, "_bounded_command", lambda argv: (0, "git version 2.50.1 (Apple Git-155)\n", ""))
    assert doctor._git_version_report()["verdict"] == "ok"


# --- x-2486: the stale-cache line must name a command that can perform the fix ---
#
# `fno update` was measured against this artifact: 15m07s, exit 0,
# installed_plugins.json byte-identical. update.py names ~/.claude/plugins only
# as a SOURCE to install FROM. The registry belongs to claude, which mints its
# own gitCommitSha, so the command that can refresh it is `claude plugin update`.
# These assert the RELATIONSHIP (the prescribed command owns the artifact), not the
# sentence: a string-only test re-encodes the same guess it is meant to catch.


def _write_skill_file(path: Path, content: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)


def test_plugin_file_stale_names_claude_refresh_and_digests(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source"
    active = (
        tmp_path
        / ".claude"
        / "plugins"
        / "cache"
        / "footnote"
        / "fno"
        / "0.3.1"
        / "skills"
        / "review"
        / "SKILL.md"
    )
    _write_skill_file(source / "skills" / "review" / "SKILL.md", b"source")
    _write_skill_file(active, b"deployed")
    monkeypatch.setattr(doctor, "_resolve_source", lambda _source: source)

    result = runner.invoke(app, ["doctor", "plugin-file", str(active)])

    assert result.exit_code == 3, result.output
    assert "PLUGIN_FILE_STALE" in result.stdout
    assert "active_sha256=" in result.stdout
    assert "source_sha256=" in result.stdout
    assert "claude plugin update fno@footnote" in result.stdout
    assert "fno doctor update" in result.stdout
    assert "does NOT refresh" in result.stdout
    assert "restart" in result.stdout


# ---------------------------------------------------------------------------
# Deployed-component convergence (verdict widening + rendering)
# ---------------------------------------------------------------------------


def test_verdict_widens_rust_stale_from_a_proven_stale_sibling() -> None:
    """A fresh client beside a stale daemon is invisible to the client's own
    rev check; the component verdict proves the sibling and gates rust_stale."""
    result = doctor._verdict(
        source_resolved=True,
        source_rev="abc",
        marker="abc",
        capture_present="present",
        rust_binary="/cargo/bin/fno-agents",
        rust_installed_rev="aaa",
        rust_source_rev="aaa",
        cargo_bin_present=True,
        component_statuses=[
            ("fno-agents", "fresh"),
            ("fno-agents-daemon", "stale"),
            ("fno-agents-worker", "fresh"),
            ("fno", "fresh"),
            ("python-tool", "fresh"),
        ],
    )
    assert result["rust_stale"] is True
    assert result["status"] == "stale"


def test_live_tool_env_scan_filters_and_names_in_stale_verdict(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Full chain over a fake ps: the scanner walks from the installed package
    dir (the real depth, <tool>/fno/lib/pythonX.Y/site-packages/fno) up to the
    ancestor named fno, drops its own ps/awk lines and the doctor process
    itself, returns [] on an unfamiliar layout, and the stale verdict names
    what survived (gap audit blocker 3: the repair replaces that env in
    place)."""
    pkg = tmp_path / "fno" / "lib" / "python3.11" / "site-packages" / "fno"
    pkg.mkdir(parents=True)
    monkeypatch.setattr(doctor, "_installed_pkg_dir", lambda: pkg)
    root = str(tmp_path / "fno")
    me_pid = os.getpid()
    ps_out = "\n".join(
        [
            f"  77 {root}/bin/fno-py backlog capture",
            f"{me_pid:6} {root}/bin/fno-py doctor --fix",
            f"  99 awk -v td={root} index($0, td)",
            "  55 /usr/sbin/syslogd",
        ]
    )

    class FakeProc:
        stdout = ps_out

    def fake_run(*args, **kwargs):
        return FakeProc()

    real_run = subprocess.run
    monkeypatch.setattr(subprocess, "run", fake_run)
    live = doctor._live_tool_env_processes()
    assert live == [f"77 {root}/bin/fno-py backlog capture"], live

    # An unfamiliar layout (no ancestor named fno) degrades to empty.
    other = tmp_path / "elsewhere" / "lib" / "python3.11" / "site-packages" / "fno"
    other.mkdir(parents=True)
    monkeypatch.setattr(doctor, "_installed_pkg_dir", lambda: other)
    assert doctor._live_tool_env_processes() == []

    # Restore real subprocess for the CLI collectors and pin the scanner's
    # output: the stale verdict must name the surviving process.
    monkeypatch.setattr(subprocess, "run", real_run)
    monkeypatch.setattr(
        doctor,
        "_live_tool_env_processes",
        lambda: [f"77 {root}/bin/fno-py backlog capture"],
    )
    _stub_signals(
        monkeypatch,
        src=Path("/src"),
        source_rev="abc123",
        marker="bbb222",
        capture_present="present",
    )
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code != 0
    assert "1 live process(es) run from the installed tool env" in result.stdout
    assert f"{root}/bin/fno-py backlog capture" in result.stdout
