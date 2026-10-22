from __future__ import annotations

from pathlib import Path

import pytest
import typer
import typer.main
# click's runner, not typer's: the live `fno doctor lint` resolves to a bare
# TyperCommand (see _live_lint_command), and typer.testing.CliRunner only
# accepts a Typer app.
from click.testing import CliRunner

from fno import paths
from fno.lint_cli import lint


def _live_lint_command():
    """The `fno doctor lint` command in the exact shape the live CLI resolves.

    `lint` is a plain-function registry entry, so `_lazy_group` wraps it in a
    one-command Typer and takes `typer.main.get_command`, which collapses to a
    bare command. Rebuilding it the same way here keeps the argv these tests
    pass (`["flock-pattern", ...]`) identical to what a user types, rather than
    testing a group shape that no longer exists.
    """
    sub = typer.Typer(add_completion=False)
    sub.command(name="lint")(lint)
    return typer.main.get_command(sub)


app = _live_lint_command()

runner = CliRunner()


def _write_provider(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")


def test_provider_stderr_merge_lint_flags_unjustified_merge(tmp_path: Path) -> None:
    providers = tmp_path / "providers"
    _write_provider(
        providers / "bad.py",
        """
import subprocess


def _run_bad():
    return subprocess.Popen(["bad"], stderr=subprocess.STDOUT)
""",
    )

    result = runner.invoke(
        app,
        ["provider-stderr-merge", "--providers-dir", str(providers)],
    )

    assert result.exit_code == 1
    assert "bad.py" in result.stderr
    assert "requires nearby" in result.stderr


def test_plan_filenames_lint_names_mismatched_claim(tmp_path: Path, monkeypatch) -> None:
    plans = tmp_path / "plans"
    plans.mkdir()
    (plans / "20260915-example-ab-aaaa1111.md").write_text(
        "---\nclaims: ab-bbbb2222\n---\n# Example\n", encoding="utf-8"
    )
    (plans / "20260915-example-ab-bbbb2222.md").write_text(
        "---\nclaims: ab-bbbb2222\n---\n# Matching\n", encoding="utf-8"
    )
    (plans / "20260915-example-births-dead.md").write_text(
        "---\nclaims: ab-cdca1234\n---\n# Id-less\n", encoding="utf-8"
    )
    monkeypatch.setattr(paths, "plans_content_dir", lambda: plans)

    result = runner.invoke(app, ["plan-filenames"])

    assert result.exit_code == 1
    assert "20260915-example-ab-aaaa1111.md" in result.stdout
    assert "ab-aaaa1111" in result.stdout
    assert "ab-bbbb2222" in result.stdout
    assert "20260915-example-ab-bbbb2222.md" not in result.stdout
    assert "births-dead.md" not in result.stdout


def test_registry_lint_reports_all_three_buckets_and_exits_nonzero(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`fno doctor lint registry` (x-7bcd) end-to-end: only registry.py's own
    write-time validation had test coverage before this. Nothing exercised
    the CLI wrapper itself (the CHECKS entry, the --json flag, the three
    output buckets), so a break here would have gone unnoticed."""
    import fno.agents.registry as reg

    ok_log = tmp_path / "ok.log"
    ok_log.write_text("", encoding="utf-8")
    rows = [
        reg.AgentEntry(name="ghost", cwd="/tmp/x", log_path="", harness="claude"),
        reg.AgentEntry(
            name="stale", cwd="/tmp/x", log_path=str(tmp_path / "gone.log"), harness="claude"
        ),
        reg.AgentEntry(name="live", cwd="/tmp/x", log_path=str(ok_log), harness="claude"),
    ]
    monkeypatch.setattr(reg, "load_registry", lambda *a, **k: rows)

    result = runner.invoke(app, ["registry"])

    assert result.exit_code == 1
    assert "no handle recorded: ghost" in result.stdout
    assert "recorded but unresolvable: stale (recorded: log_path)" in result.stdout
    assert "live" not in result.stdout
    assert (
        "fno doctor lint registry: 1 no handle recorded, 1 recorded but unresolvable, 1 ok, 3 total"
        in result.stdout
    )


def test_spawn_paths_lint_rejects_spawn_flag_after_other_options(tmp_path: Path) -> None:
    source = tmp_path / "cli" / "src" / "fno" / "new_spawn.py"
    source.parent.mkdir(parents=True)
    source.write_text(
        'cmd = ["claude", "--model", "sonnet", "--print", "prompt"]\n',
        encoding="utf-8",
    )

    from fno.lint_cli import _spawn_shape_violations

    violations = _spawn_shape_violations(tmp_path)
    assert len(violations) == 1
    assert "new_spawn.py:1" in violations[0]
    assert "--print" in violations[0]


# --------------------------------------------------------------------------- #
# flock-pattern: conform + degrade (ab-fd017698)
# --------------------------------------------------------------------------- #
def test_flock_pattern_degrades_when_script_absent(tmp_path: Path, monkeypatch) -> None:
    """US1 (AC1-HP/ERR/UI/EDGE/FR): with the lint script absent (a no-script env),
    the command exits 2 with an actionable stderr message - never bash's 127 and
    never a Python traceback."""
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))  # empty dir -> no script
    result = runner.invoke(app, ["flock-pattern"])

    assert result.exit_code == 2  # exit 2, not 127, not 0
    assert "flock-pattern" in result.stderr
    assert "lint scripts" in result.stderr  # names what is missing
    assert "Traceback" not in (result.stderr + result.stdout)


def test_every_check_parameter_is_declared_on_the_dispatcher() -> None:
    """Every option a check accepts must exist on `fno doctor lint`.

    The eight subcommands each carried their own options; the collapse turned
    those into options on one command, dispatched by signature. A check parameter
    with no matching option on the dispatcher is UNREACHABLE - the flag is
    rejected as unknown before any check runs.

    That is exactly what shipped: `style` declares `--surface/--stdin/--files/
    --diff-base` and the first pass wired none of them, so `fno doctor lint style
    --surface markdown` died with "No such option" in a CI job four steps
    removed from the change. This compares the two sets directly, so the next
    check with a new option fails here instead.
    """
    import inspect

    import fno.lint_cli as L

    declared = set(inspect.signature(L.lint).parameters) - {"check"}
    for name, fn_name in L.CHECKS.items():
        accepted = set(inspect.signature(getattr(L, fn_name)).parameters)
        missing = accepted - declared
        assert not missing, (
            f"`fno doctor lint {name}` accepts {sorted(missing)}, which `fno doctor lint` does "
            f"not declare, so those flags are unreachable. Add them to lint()."
        )


# ---------------------------------------------------------------------------
# state-roots (x-3d21 R4/R5): every rule ships with a control that FIRES.
#
# A zero from a lint proves nothing on its own. A positive control validates
# the TOOL, not the TARGET, and a green control aimed at the wrong symbol
# still reads as proof - so each control below names the symbol the rule
# actually matches, and the two real-tree cases assert against the live
# specimens rather than a fixture the detector was shaped around.
# ---------------------------------------------------------------------------


def _real_repo_root() -> Path:
    import fno.lint_cli as lint_cli

    return Path(lint_cli.__file__).resolve().parents[3]


def test_state_roots_rule_a_fires_on_the_combined_literal(tmp_path: Path) -> None:
    """`".fno/graph.json"` as ONE literal, in both languages.

    The control the first cut of this rule did not have. Every other fixture
    here writes the SEPARATED form (`/ ".fno" / "graph.json"`), so a filename
    pattern that rejected the slash the `.fno/` token supplies passed all of
    them while matching none of the combined form - a green control aimed at
    the wrong symbol. It hid a live production site in `spawn_gate.rs`.
    """
    py = tmp_path / "cli" / "src" / "fno" / "new_writer.py"
    py.parent.mkdir(parents=True)
    py.write_text('p = root / ".fno/ledger.json"\n', encoding="utf-8")
    rs = tmp_path / "crates" / "fno-agents" / "src" / "new_writer.rs"
    rs.parent.mkdir(parents=True)
    rs.write_text('let p = root.join(".fno/claims");\n', encoding="utf-8")

    from fno.lint_cli import _state_root_path_violations

    hit = {(rel, key) for rel, key, _ in _state_root_path_violations(tmp_path)}

    assert ("cli/src/fno/new_writer.py", "ledger.json") in hit
    assert ("crates/fno-agents/src/new_writer.rs", "claims") in hit


def test_state_roots_rule_b_fires_on_a_zero_arg_cache_over_a_resolver(
    tmp_path: Path,
) -> None:
    source = tmp_path / "cli" / "src" / "fno" / "new_reader.py"
    source.parent.mkdir(parents=True)
    source.write_text(
        "from functools import lru_cache\n"
        "\n"
        "@lru_cache(maxsize=1)\n"
        "def _cached_graph():\n"
        "    return state_dir()\n",
        encoding="utf-8",
    )

    from fno.lint_cli import _zero_arg_root_cache_violations

    violations = _zero_arg_root_cache_violations(tmp_path)

    assert len(violations) == 1
    rel, symbol, message = violations[0]
    assert (rel, symbol) == ("cli/src/fno/new_reader.py", "_cached_graph")
    # The refusal must TEACH THE REMEDY, not only name the offence.
    assert "_cached_graph_at(root: Path)" in message


def test_state_roots_baseline_covers_the_whole_live_census() -> None:
    """The shipped tree is clean: every live finding is baselined, none is stale.

    Both set differences are empty when the scan finds NOTHING, so the two
    assertions alone pass vacuously - the absence-only shape this repo's own
    pitfall corpus forbids. The positive markers come first: the scan must
    reach both languages and both rules before its emptiness means anything.
    """
    from fno.lint_cli import _read_state_roots_baseline, _state_roots_findings

    root = _real_repo_root()
    findings = set(_state_roots_findings(root))
    baseline = _read_state_roots_baseline(root)

    assert len(baseline) > 20, "the ratchet lost its census"
    assert any(rule == "A" and rel.endswith(".rs") for rule, rel, _ in findings)
    assert any(rule == "A" and rel.endswith(".py") for rule, rel, _ in findings)
    # The four x-3d21 specimens are keyed on their declaration; rule B's live
    # census is empty and its ratchet stays armed via the synthetic-specimen
    # control above.
    assert not any(rule == "B" for rule, rel, _ in findings)

    assert findings - baseline == set()
    assert baseline - findings == set()


def test_state_roots_gate_fails_when_a_baselined_site_was_drained(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The load-bearing half: a drained exemption cannot become permanent."""
    import fno.lint_cli as lint_cli

    (tmp_path / "cli" / "src" / "fno").mkdir(parents=True)
    baseline = tmp_path / "scripts" / "ci"
    baseline.mkdir(parents=True)
    (baseline / "state-roots-baseline.txt").write_text(
        "A\tcli/src/fno/gone.py\tgraph.json\tunowned\tdrained already\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(lint_cli, "_repo_root", lambda: tmp_path)

    result = runner.invoke(app, ["state-roots"])

    assert result.exit_code == 1
    assert "fixed, remove these baseline lines" in result.output
    assert "cli/src/fno/gone.py" in result.output


def test_unknown_check_refusal_lists_internal_refs() -> None:
    """The refusal derives from CHECKS, so the new name is discoverable there."""
    result = runner.invoke(app, ["definitely-not-a-check"])
    assert result.exit_code == 2
    assert "internal-refs" in result.output
