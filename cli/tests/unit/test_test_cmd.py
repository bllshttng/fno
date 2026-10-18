"""Unit tests for fno.test_cmd smoke discovery.

discover_shell_harnesses() walks the owned shell-harness trees and returns the
standalone harnesses, so a new tests/hooks/test_x.sh runs with zero registry
edits (the two-test-trees lesson: a green subset is not proof everything ran).
It excludes libraries (scripts/lib/), the orchestrated cli/tests/smoke/
subtree, files without a shell shebang, and source-only helpers.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest
from click.testing import CliRunner

from fno.test_cmd import (
    changed_snapshot,
    discover_shell_harnesses,
    select_changed,
    test_command,
)


def _write(path: Path, body: str, *, executable: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    if executable:
        path.chmod(0o755)


def test_discovers_new_harness_and_excludes_non_harnesses(tmp_path: Path) -> None:
    root = tmp_path

    # AC4-EDGE: a brand-new harness dropped into an owned tree is discovered
    # with no registry edit (the consolidation's core win).
    _write(root / "tests/zzz-probe.sh", "#!/usr/bin/env bash\necho probe\n", executable=True)
    _write(
        root / "tests/hooks/test_new_probe.sh", "#!/usr/bin/env bash\necho hi\n", executable=True
    )
    # A harness in scripts/tests/ and one under skills/*/tests/.
    _write(root / "scripts/tests/test_one.sh", "#!/usr/bin/env bash\necho one\n", executable=True)
    _write(
        root / "skills/agent/tests/test_one.sh",
        "#!/usr/bin/env bash\necho skill\n",
        executable=True,
    )

    # A source-only helper: a sibling sources it -> helper, not a harness.
    _write(
        root / "tests/hooks/test_helper.sh",
        "#!/usr/bin/env bash\n# sourced by a sibling, never standalone\n",
        executable=True,
    )
    _write(
        root / "tests/hooks/test_uses_helper.sh",
        "#!/usr/bin/env bash\nsource ./test_helper.sh\necho ok\n",
        executable=True,
    )
    # A helper sourced via the conventional variable form ($SCRIPT_DIR/...):
    # the resolver falls back to the basename under the sourcer's dir.
    _write(
        root / "tests/hooks/test_var_helper.sh",
        "#!/usr/bin/env bash\n# sourced via $SCRIPT_DIR\n",
        executable=True,
    )
    _write(
        root / "tests/hooks/test_uses_var_helper.sh",
        '#!/usr/bin/env bash\nSCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"\n'
        'source "$SCRIPT_DIR/test_var_helper.sh"\necho ok\n',
        executable=True,
    )

    # Excluded classes.
    _write(
        root / "scripts/lib/libthing.sh",  # library, out of owned scope
        "#!/usr/bin/env bash\necho lib\n",
        executable=True,
    )
    _write(
        root / "cli/tests/smoke/test_smoke_one.sh",  # orchestrated subtree
        "#!/usr/bin/env bash\necho smoke\n",
        executable=True,
    )
    _write(
        root / "tests/hooks/not_shell.sh",  # no shell shebang
        "#!/usr/bin/env python3\nprint('no')\n",
        executable=True,
    )

    found = set(discover_shell_harnesses(root))

    assert "tests/zzz-probe.sh" in found  # AC4-EDGE
    assert "tests/hooks/test_new_probe.sh" in found
    assert "scripts/tests/test_one.sh" in found
    assert "skills/agent/tests/test_one.sh" in found
    assert "tests/hooks/test_uses_helper.sh" in found  # real harness that sources a helper
    assert "tests/hooks/test_helper.sh" not in found  # source-only helper excluded
    assert "tests/hooks/test_uses_var_helper.sh" in found  # real harness, variable source ref
    assert "tests/hooks/test_var_helper.sh" not in found  # $SCRIPT_DIR-sourced helper excluded
    assert "scripts/lib/libthing.sh" not in found  # library excluded
    assert "cli/tests/smoke/test_smoke_one.sh" not in found  # orchestrated subtree excluded
    assert "tests/hooks/not_shell.sh" not in found  # non-shell shebang excluded


# --- changed-surface selection (fno doctor test smoke --changed) -------------------


def _repo(root: Path) -> None:
    """Minimal shape select_changed() reads: a cli/tests tree + owned harnesses."""
    _write(root / "cli/src/fno/widget.py", "x = 1\n")
    _write(root / "cli/tests/unit/test_widget.py", "def test_x(): pass\n")
    _write(root / "cli/tests/unit/test_test_cmd.py", "def test_y(): pass\n")
    _write(root / "tests/lib/helper.sh", "#!/usr/bin/env bash\n:\n", executable=True)
    for name in ("test_a", "test_b"):
        _write(
            root / f"tests/lib/{name}.sh",
            '#!/usr/bin/env bash\nsource "$(dirname "$0")/helper.sh"\n:\n',
            executable=True,
        )


def test_python_source_selects_conventional_test_and_names_the_rule(tmp_path: Path) -> None:
    """AC2: the receipt names the selected test AND the rule that selected it."""
    _repo(tmp_path)
    sel, unmapped = select_changed(tmp_path, ["cli/src/fno/widget.py"])
    assert [(s["rule"], s["target"]) for s in sel] == [
        ("python-source-stem", "cli/tests/unit/test_widget.py")
    ]
    assert unmapped == []


def test_shell_helper_selects_both_owning_harnesses_once(tmp_path: Path) -> None:
    """AC3: reverse source-reference rule, each owner exactly once."""
    _repo(tmp_path)
    sel, unmapped = select_changed(tmp_path, ["tests/lib/helper.sh"])
    assert sorted(s["target"] for s in sel) == ["tests/lib/test_a.sh", "tests/lib/test_b.sh"]
    assert {s["rule"] for s in sel} == {"shell-helper-reverse"}
    assert unmapped == []


def test_unknown_path_is_unmapped_and_selects_nothing(tmp_path: Path) -> None:
    """AC4: an unknown path is visible as unmapped, never silently green."""
    _repo(tmp_path)
    sel, unmapped = select_changed(tmp_path, ["docs/some-note.md"])
    assert sel == []
    assert unmapped == ["docs/some-note.md"]


def test_infix_test_family_is_selected_by_name_containment(tmp_path: Path) -> None:
    """An envelope change also owns test_fno_mail_envelope.py: the same-stem
    map caught the exact spelling and left the infix one unselected, and the
    merge went green while main ran it red. Every indexed tree name-matches:
    the reach index covers cli/tests, the root tests/ tree, and tests
    co-located in cli/src, and the infix rule filters none of them out."""
    _write(tmp_path / "cli/src/fno/mail/envelope.py", "x = 1\n")
    _write(tmp_path / "cli/tests/relay/test_envelope.py", "def test_a(): pass\n")
    _write(tmp_path / "cli/tests/unit/test_fno_mail_envelope.py", "def test_b(): pass\n")
    _write(tmp_path / "tests/unit/test_envelope_parse.py", "def test_c(): pass\n")
    _write(tmp_path / "cli/src/fno/test_envelope_smoke.py", "def test_d(): pass\n")
    sel, _ = select_changed(tmp_path, ["cli/src/fno/mail/envelope.py"])
    by_rule = {(s["rule"], s["target"]) for s in sel}
    assert ("python-source-stem", "cli/tests/relay/test_envelope.py") in by_rule
    assert ("python-source-infix", "cli/tests/unit/test_fno_mail_envelope.py") in by_rule
    assert ("python-source-infix", "tests/unit/test_envelope_parse.py") in by_rule
    assert ("python-source-infix", "cli/src/fno/test_envelope_smoke.py") in by_rule


def test_import_chain_through_a_source_module_is_selected(tmp_path: Path) -> None:
    """A test importing dispatch owns an envelope change, because dispatch
    imports envelope - at function level, so the scan must walk the AST, not
    the first column. One source hop is the bound: a test of a module that
    merely imports an importer stays out and leans on the full gate."""
    _write(tmp_path / "cli/src/fno/mail/envelope.py", "def wrap(): ...\n")
    _write(
        tmp_path / "cli/src/fno/agents/dispatch.py",
        "def _send():\n    from fno.mail.envelope import wrap\n",
    )
    _write(tmp_path / "cli/src/fno/agents/hop.py", "from fno.agents.dispatch import _send\n")
    _write(
        tmp_path / "cli/tests/unit/test_pane_guarded.py", "import fno.agents.dispatch as dispatch\n"
    )
    _write(tmp_path / "cli/tests/unit/test_two_hops.py", "import fno.agents.hop as hop\n")
    sel, _ = select_changed(tmp_path, ["cli/src/fno/mail/envelope.py"])
    by_rule = {(s["rule"], s["target"]) for s in sel}
    assert ("python-source-importers", "cli/tests/unit/test_pane_guarded.py") in by_rule
    assert ("python-source-importers", "cli/tests/unit/test_two_hops.py") not in by_rule


def _git_repo(root: Path) -> None:
    for args in (("init", "-q"), ("config", "user.email", "t@t.t"), ("config", "user.name", "t")):
        subprocess.run(["git", "-C", str(root), *args], check=True)


def test_snapshot_diffs_explicit_base_and_head(tmp_path: Path) -> None:
    """The CI path diffs the given revisions, not a mutable remote ref."""
    _git_repo(tmp_path)
    _write(tmp_path / "a.txt", "one\n")
    subprocess.run(["git", "-C", str(tmp_path), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-qm", "base"], check=True)
    base = subprocess.run(
        ["git", "-C", str(tmp_path), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    _write(tmp_path / "b.txt", "two\n")
    subprocess.run(["git", "-C", str(tmp_path), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-qm", "head"], check=True)

    paths, reason, resolved = changed_snapshot(tmp_path, base, "HEAD")
    assert reason == ""
    assert paths == ["b.txt"]
    assert resolved == base


def test_snapshot_local_mode_does_not_fall_back_past_a_resolvable_main(
    tmp_path: Path,
) -> None:
    """origin/main resolves but shares no history with HEAD (unrelated
    histories), while origin/master is a shared ancestor: the fallback must
    NOT silently re-base onto master - that would size the packet from a
    whole era. The run is unevaluated instead."""
    _git_repo(tmp_path)
    _write(tmp_path / "a.txt", "one\n")
    subprocess.run(["git", "-C", str(tmp_path), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-qm", "base"], check=True)
    base = subprocess.run(
        ["git", "-C", str(tmp_path), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    subprocess.run(
        ["git", "-C", str(tmp_path), "update-ref", "refs/remotes/origin/master", base], check=True
    )
    subprocess.run(["git", "-C", str(tmp_path), "checkout", "-q", "--orphan", "other"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "rm", "-rq", "--cached", "."], check=True)
    _write(tmp_path / "b.txt", "unrelated\n")
    subprocess.run(["git", "-C", str(tmp_path), "add", "b.txt"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-qm", "orphan"], check=True)
    unrelated = subprocess.run(
        ["git", "-C", str(tmp_path), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    subprocess.run(["git", "-C", str(tmp_path), "checkout", "-qf", "-B", "work", base], check=True)
    subprocess.run(
        ["git", "-C", str(tmp_path), "update-ref", "refs/remotes/origin/main", unrelated],
        check=True,
    )
    _write(tmp_path / "c.txt", "three\n")
    subprocess.run(["git", "-C", str(tmp_path), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-qm", "work"], check=True)

    paths, reason, resolved = changed_snapshot(tmp_path)
    assert paths == []
    assert resolved == ""
    assert "cannot resolve merge-base" in reason


def test_snapshot_fails_closed_on_an_unresolvable_base(tmp_path: Path) -> None:
    """AC5: a missing base is an explicit unevaluated result, not an empty diff."""
    _git_repo(tmp_path)
    _write(tmp_path / "a.txt", "one\n")
    subprocess.run(["git", "-C", str(tmp_path), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-qm", "base"], check=True)

    paths, reason, _ = changed_snapshot(
        tmp_path, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef", "HEAD"
    )
    assert paths == []
    assert "does not resolve" in reason


def test_snapshot_refuses_a_half_specified_range(tmp_path: Path) -> None:
    """AC5: --base without --head cannot silently fall back to local mode."""
    _git_repo(tmp_path)
    paths, reason, _ = changed_snapshot(tmp_path, "HEAD", "")
    assert paths == []
    assert "both --base and --head" in reason


def test_journey_harness_carries_its_build_prerequisite(tmp_path: Path) -> None:
    """A harness needing the debug binary must not run without the build step.

    Without it the harness exits 77 (a SKIP), which the packet counts as a
    failure - a false red instead of early feedback.
    """
    from fno.test_cmd import _RUST_BUILD_STEP, _changed_steps

    _repo(tmp_path)
    _write(
        tmp_path / "tests/hooks/test_journey.sh",
        '#!/usr/bin/env bash\nB="$REPO_ROOT/crates/fno-agents/target/debug/fno-agents"\n'
        '[[ -x "$B" ]] || exit 77\n',
        executable=True,
    )
    sel, _ = select_changed(tmp_path, ["tests/hooks/test_journey.sh"])
    names = [s[0] for s in _changed_steps(tmp_path, sel)]
    assert names[0] == _RUST_BUILD_STEP, names
    assert "tests/hooks/test_journey.sh" in names


def test_rust_build_precedes_pytest_in_the_changed_packet(tmp_path: Path) -> None:
    """The graph store's read path is the keeper, and the keeper needs
    fno-agents-worker on disk. A packet that runs pytest before the build
    makes every graph-touching test crash with StoreUnavailable instead of
    answering, so a crates change that selects both must build first - the
    full smoke's own ordering."""
    from fno.test_cmd import _RUST_BUILD_STEP, _changed_steps

    _repo(tmp_path)
    sel, _ = select_changed(
        tmp_path, ["crates/fno-agents/src/loop_runtime.rs", "cli/src/fno/widget.py"]
    )
    names = [s[0] for s in _changed_steps(tmp_path, sel)]
    assert _RUST_BUILD_STEP in names, names
    assert names.index(_RUST_BUILD_STEP) < names.index("Pytest (changed subset, 1 file(s))"), names


def test_quarantined_harnesses_are_never_selected(tmp_path: Path) -> None:
    """The full gate refuses to run these; the packet must not either.

    Selecting one turns an ordinary edit into a red packet that aborts
    preflight BEFORE the real gate, on a test CI would never have run.
    """
    import fno.test_cmd as tc

    _repo(tmp_path)
    _write(tmp_path / "scripts/lib/shared.sh", "#!/usr/bin/env bash\n:\n")
    for name in ("test_rotten.sh", "test_healthy.sh"):
        _write(
            tmp_path / f"scripts/tests/{name}",
            '#!/usr/bin/env bash\nSCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"\n'
            'source "$SCRIPT_DIR/../lib/shared.sh"\n',
            executable=True,
        )
    original = tc._DISCOVERY_DEFERRED
    tc._DISCOVERY_DEFERRED = frozenset({"scripts/tests/test_rotten.sh"})
    try:
        sel, _ = select_changed(tmp_path, ["scripts/lib/shared.sh"])
    finally:
        tc._DISCOVERY_DEFERRED = original
    targets = [s["target"] for s in sel]
    assert "scripts/tests/test_healthy.sh" in targets
    assert "scripts/tests/test_rotten.sh" not in targets


# --- the changed-packet estimate (what CI sizes the job ceiling from) ---------


def _selections(kind_counts: dict[str, int], *, with_build: bool = False) -> list[dict]:
    """Synthetic selections shaped like select_changed() output."""
    sels: list[dict] = []
    for kind, n in kind_counts.items():
        for i in range(n):
            sels.append(
                {"rule": kind, "path": f"{kind}-{i}", "kind": kind, "target": f"{kind}-{i}-target"}
            )
    if with_build:
        from fno.test_cmd import _RUST_BUILD_STEP

        sels.append(
            {
                "rule": "rust-family",
                "path": "crates/x.rs",
                "kind": "step",
                "target": _RUST_BUILD_STEP,
            }
        )
    return sels


def test_estimate_puts_tonights_failing_packet_well_above_the_old_cap(tmp_path: Path) -> None:
    """The measured packet (93 pytest files, 14 shell harnesses, the build and
    two registry steps) could not finish inside a fixed 15-minute cap. The
    estimate must land far above that cap and inside the 75 ceiling CI clamps
    to, or the sizing would repeat the starvation it exists to remove."""
    from fno.test_cmd import _estimate_changed_minutes

    sels = _selections({"pytest": 93, "shell": 14, "step": 2}, with_build=True)
    est = _estimate_changed_minutes(tmp_path, sels)
    assert est > 15, est
    assert est <= 75, est


def test_changed_estimate_line_is_printed_for_the_sizer_to_read(tmp_path: Path) -> None:
    """CI's sizer parses one stdout line; its absence would read as a zero."""
    import contextlib
    import io

    import fno.test_cmd as tc

    _git_repo(tmp_path)
    _repo(tmp_path)
    subprocess.run(["git", "-C", str(tmp_path), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-qm", "base"], check=True)
    base = subprocess.run(
        ["git", "-C", str(tmp_path), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    _write(tmp_path / "cli/src/fno/widget.py", "x = 2\n")
    subprocess.run(["git", "-C", str(tmp_path), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(tmp_path), "commit", "-qm", "head"], check=True)
    opts = {"changed": True, "base": base, "head": "HEAD", "list": True, "shard": ""}
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        rc = tc._run_changed(tmp_path, opts, {})
    assert rc == 0
    assert "changed-estimate minutes=" in buf.getvalue()


def test_empty_flag_values_are_refused(tmp_path: Path) -> None:
    """An unset shell variable is a caller bug, never an instruction.

    `--only=` silently disabled subset selection and ran the FULL suite;
    `--base=` silently fell back to local mode. Both look like the subset the
    caller asked for, which is the whole failure class this mode guards.
    """
    import pytest

    from fno.test_cmd import _parse_smoke_args

    for argv in (
        ["--only="],
        ["--only"],
        ["--changed", "--base=", "--head", "HEAD"],
        ["--changed", "--base", "HEAD", "--head="],
        ["--changed", "--head", ""],
        # A following option is a forgotten value, not the value:
        # `--only --retry-failed` otherwise became an only-glob of
        # "--retry-failed" and skipped the mutual-exclusion check.
        ["--only", "--retry-failed"],
        ["--only", "--changed"],
        ["--changed", "--base", "--head", "HEAD"],
    ):
        with pytest.raises(ValueError, match="needs a value"):
            _parse_smoke_args(argv)
    # Real values still parse.
    opts = _parse_smoke_args(["--changed", "--base", "HEAD~1", "--head", "HEAD"])
    assert opts["changed"] and opts["base"] == "HEAD~1" and opts["head"] == "HEAD"


def test_changed_shard_slices_pytest_files_round_robin(tmp_path: Path) -> None:
    """The changed packet accepts --shard and leg k of N runs the same files
    every time: round-robin over the sorted selected list, one contract with
    the full lane's --shard. Non-pytest steps run in every leg because each
    matrix leg is a fresh runner. Measured 2026-10-05: the whole packet on
    one runner died 7 of 8 attempts at a fixed suite point; the full lane's
    sharding survived, so volume on one runner is what kills it."""
    from fno.test_cmd import _changed_steps, _parse_smoke_args

    # The selector mode and the shard mode compose; --retry-failed stays
    # exclusive with --shard.
    assert _parse_smoke_args(["--only", "Pytest*", "--shard", "2/4"])["shard"] == "2/4"
    changed = _parse_smoke_args(
        ["--changed", "--base", "HEAD~1", "--head", "HEAD", "--shard", "1/4"]
    )
    assert changed["changed"] and changed["shard"] == "1/4"
    with pytest.raises(ValueError, match="separate subset modes"):
        _parse_smoke_args(["--retry-failed", "--shard", "1/4"])

    files = [f"cli/tests/unit/test_z{x}.py" for x in range(5)]
    selections = [{"kind": "pytest", "target": f} for f in files]
    selections.append({"kind": "step", "target": "Skill bundles freshness check"})

    legs = []
    for leg in (1, 2, 3):
        steps = _changed_steps(tmp_path, selections, f"{leg}/3")
        pytest_steps = [s for s in steps if s[0].startswith("Pytest (changed subset")]
        assert len(pytest_steps) == 1, steps
        assert "Skill bundles freshness check" in [s[0] for s in steps]
        cmd = pytest_steps[0][2]
        legs.append([t for t in cmd.split() if t.endswith(".py")])

    flat = sorted(t for leg in legs for t in leg)
    assert flat == sorted(files), (flat, files)
    assert legs[0] == [files[0], files[3]]
    assert not (set(legs[0]) & set(legs[1])) and not (set(legs[0]) & set(legs[2]))


def test_smoke_shard_partitions_steps_and_scopes_pytest_env(tmp_path: Path, monkeypatch) -> None:
    import fno.test_cmd as tc

    registry = tmp_path / "registry.py"
    registry.write_text(
        "STEPS = [\n"
        " ('Sync + build', '.', 'true'),\n"
        " ('Pytest (unit + integration)', '.', 'true'),\n"
        " ('before', '.', 'true'),\n"
        " ('Build fno-agents debug binary (for journey tests)', '.', 'true'),\n"
        " ('after-a', '.', 'true'),\n"
        " ('after-b', '.', 'true'),\n"
        "]\n"
    )
    monkeypatch.setenv("SMOKE_REGISTRY_FILE", str(registry))
    monkeypatch.setenv("SMOKE_FAILURE_RECORD", str(tmp_path / "failures.txt"))
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    calls: list[tuple[str, str | None]] = []

    class _Completed:
        returncode = 0

    def run(command, *, cwd, env):
        del cwd
        # The state-canary bracket rides the same subprocess.run. It is not
        # what this test measures, and folding it into the expected list would
        # couple shard partitioning to the canary's presence.
        if any("check-state-canary.sh" in str(part) for part in command):
            return _Completed()
        calls.append((command[-1], env.get("FNO_PYTEST_SHARD")))
        return _Completed()

    monkeypatch.setattr(tc.subprocess, "run", run)

    assert tc._run_smoke(["--shard", "2/4"]) == 0

    assert [command for command, _shard in calls] == ["true", "true", "true", "true"]
    assert [shard for _command, shard in calls] == [None, "2/4", None, None]


@pytest.mark.parametrize("spec", ["5/4", "four/4"])
def test_smoke_shard_invalid_spec_is_refused(spec: str) -> None:
    from fno.test_cmd import _parse_smoke_args

    with pytest.raises(ValueError, match="--shard"):
        _parse_smoke_args(["--shard", spec])


def test_census_green_is_failure_red_slow_killed_pass_empty_is_usage_error(
    tmp_path: Path, monkeypatch
) -> None:
    """The inverted contract the feature exists to enforce: a green-fast deferred
    harness is FAILURE (exit 1); RED / SLOW / killed all pass (exit 0); an empty
    or unparseable set exits 2. Pinned so it cannot silently invert."""
    import fno.test_cmd as tc

    # GREEN: rc 0, fast, not killed -> exit 1.
    monkeypatch.setattr(tc, "_census_entries", lambda: ["g.sh"])
    monkeypatch.setattr(tc, "_run_bounded", lambda *a: (0, 5.0, False))
    assert tc._run_census_deferred([]) == 1

    # RED + SLOW(fast-but-over-tranche) + killed -> all non-green -> exit 0.
    canned = iter([(1, 5.0, False), (0, 999.0, False), (0, 999.0, True)])
    monkeypatch.setattr(tc, "_census_entries", lambda: ["r.sh", "s.sh", "k.sh"])
    monkeypatch.setattr(tc, "_run_bounded", lambda *a: next(canned))
    assert tc._run_census_deferred([]) == 0

    # Empty / unparseable set -> exit 2.
    monkeypatch.setattr(tc, "_census_entries", lambda: [])
    assert tc._run_census_deferred([]) == 2


def test_smoke_discovered_steps_excludes_deferred(tmp_path: Path) -> None:
    """The full-gate consumer (_run_smoke) subtracts _DISCOVERY_DEFERRED too, not
    just select_changed; a dropped clause here would let a drained harness
    re-enter the full gate while every test still passed."""
    import fno.test_cmd as tc

    _write(tmp_path / "tests/healthy.sh", "#!/usr/bin/env bash\n:\n", executable=True)
    _write(tmp_path / "tests/rotten.sh", "#!/usr/bin/env bash\n:\n", executable=True)
    original = tc._DISCOVERY_DEFERRED
    tc._DISCOVERY_DEFERRED = frozenset({"tests/rotten.sh"})
    try:
        rels = [s[0] for s in tc._smoke_discovered_steps(tmp_path, referenced=set())]
    finally:
        tc._DISCOVERY_DEFERRED = original
    assert "tests/healthy.sh" in rels
    assert "tests/rotten.sh" not in rels


def test_smoke_list_output_stays_verbatim(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """`smoke --list` is a machine contract: one name per line, nothing else.
    The scope statement must never join it."""
    reg = tmp_path / "reg.py"
    reg.write_text('STEPS = [("alpha pass", ".", "true"), ("bravo pass", ".", "true")]\n')
    monkeypatch.setenv("SMOKE_REGISTRY_FILE", str(reg))
    monkeypatch.setenv("SMOKE_FAILURE_RECORD", str(tmp_path / "rec.txt"))
    result = CliRunner().invoke(test_command, ["smoke", "--list"])
    assert result.exit_code == 0
    assert result.output == "alpha pass\nbravo pass\n"
