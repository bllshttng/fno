"""Tests for the LazyTypeGroup lazy-import refactor.

Acceptance criteria:
  AC3-EDGE: fno --help doesn't import any sub-app body
  AC3-FR: no functional regression in existing commands
  AC1-ERR: misconfigured lazy entry fails loud
  AC2-HP: fno --help lists all current subcommands
"""
from __future__ import annotations

import importlib
import subprocess
import sys
from pathlib import Path

import pytest


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _run_py(code: str, timeout: int = 30) -> subprocess.CompletedProcess[str]:
    """Run a Python snippet in a fresh subprocess, returning the result."""
    return subprocess.run(
        [sys.executable, "-c", code],
        capture_output=True,
        text=True,
        timeout=timeout,
    )


# ---------------------------------------------------------------------------
# AC3-EDGE: fno --help doesn't import sub-app module bodies
# ---------------------------------------------------------------------------

# Modules that must NOT appear in sys.modules after `fno --help`.
# These are the heaviest transitive imports that the lazy refactor defers.
_FORBIDDEN_AFTER_HELP = [
    "fno.adapters.providers.cli",
    "fno.graph.cli",
    "fno.events.cli",
    "fno.agent",
]

_CHECK_CODE_HELP = """\
import sys
from fno import cli
from typer.testing import CliRunner
CliRunner().invoke(cli.app, ["--help"])
forbidden = {forbidden!r}
found = [m for m in forbidden if m in sys.modules]
if found:
    print("FOUND:", ",".join(found), file=sys.stderr)
sys.exit(len(found))
""".format(forbidden=_FORBIDDEN_AFTER_HELP)


def test_fno_help_does_not_import_sub_app_modules():
    """AC3-EDGE: sys.modules after `fno --help` excludes sub-app bodies."""
    result = _run_py(_CHECK_CODE_HELP)
    assert result.returncode == 0, (
        f"Sub-app modules imported after fno --help:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# AC3-HP: fno paths state-dir does NOT import heavy sub-apps
# ---------------------------------------------------------------------------

_CHECK_CODE_PATHS = """\
import sys
from fno import cli
from typer.testing import CliRunner
CliRunner().invoke(cli.app, ["paths", "state-dir"])
found = [m for m in ["fno.adapters.providers.cli"] if m in sys.modules]
if found:
    print("FOUND:", ",".join(found), file=sys.stderr)
sys.exit(len(found))
"""


def test_fno_paths_does_not_import_heavy_subapps():
    """AC3-HP: `fno paths state-dir` only loads the paths sub-app."""
    result = _run_py(_CHECK_CODE_PATHS)
    assert result.returncode == 0, (
        f"heavy sub-app imported during `fno paths state-dir`:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# AC2-HP: fno --help lists all subcommands (no regression in command surface)
# ---------------------------------------------------------------------------

# Stable subcommands that must appear in `fno --help` after the refactor.
# The curated top-level menu (x-71b6 In-N-Out tiering). `fno --help` advertises
# only these; everything else is hidden but invocable, listed by `fno help --all`.
_ADVERTISED_SUBCOMMANDS = [
    "help",
    "backlog",
    "agents",
    "config",
    "whoami",
    "doctor",
]

# A sample of the hidden tier - present under `help --all`, absent from `--help`.
# Distinctive names only: short commands (pr/cost/state) are substrings of ordinary
# help prose, so a raw substring leak-check on them is unreliable. The old
# sample (evals/carveout/scoreboard/stub-manifest) was all moved spellings,
# which render nowhere after d-26002be8; do is unusable for the leak-check
# (substring of "doctor"), so the hidden REAL roots worth pinning are these.
_HIDDEN_SUBCOMMANDS = [
    "inbox",
]


def _strip_ansi(text: str) -> str:
    import re
    return re.sub(r"\x1b\[[0-9;]*m", "", text)


def test_fno_help_lists_only_advertised_menu():
    """AC1-HP: fno --help advertises the curated menu and hides the rest."""
    from fno.cli import app
    from typer.testing import CliRunner

    runner = CliRunner()
    result = runner.invoke(app, ["--help"])
    assert result.exit_code == 0, f"fno --help failed: {result.output}"
    plain = _strip_ansi(result.output)
    for cmd in _ADVERTISED_SUBCOMMANDS:
        assert cmd in plain, f"advertised {cmd!r} missing from fno --help"
    for cmd in _HIDDEN_SUBCOMMANDS:
        assert cmd not in plain, f"hidden {cmd!r} leaked into fno --help"


# ---------------------------------------------------------------------------
# Regression: group structure preserved for single-command sub-apps
# ---------------------------------------------------------------------------

def test_single_command_subapp_group_shape_preserved(monkeypatch):
    """Single-command Typer sub-apps must NOT collapse to a TyperCommand.

    Without `get_group_from_info`, a Typer app with exactly one registered
    command would collapse via `typer.main.get_command()` into a bare
    TyperCommand, turning `fno <group> <sub> <args>` into `fno <group>
    <args>` and breaking every nested call.

    Regression test for the fix in `_LazyStub._load_real`. The scratch
    module stands in for a real one-command sub-app registered through the
    same lazy map the root app uses; `monkeypatch.setitem(sys.modules, ...)`
    is what makes its fake import path resolvable.
    """
    import sys
    import types

    import typer
    from typer.testing import CliRunner

    from fno._lazy_group import make_lazy_group_cls

    scratch = types.ModuleType("_scratch_single_command_app")
    single = typer.Typer()

    @single.command("resolve")
    def resolve(
        plan_path: str = typer.Option(None, "--plan-path", help="design doc path"),
    ) -> None:
        typer.echo("resolved")

    scratch.single_app = single
    monkeypatch.setitem(sys.modules, scratch.__name__, scratch)

    lazy_map = {"thing": ("_scratch_single_command_app:single_app", "scratch")}
    test_app = typer.Typer(cls=make_lazy_group_cls(lazy_map))

    @test_app.callback()
    def _cb() -> None: ...

    runner = CliRunner()
    # Invoking the help for the sub-command proves the group structure is
    # intact: a collapsed app would not recognise 'resolve' as a subcommand.
    result = runner.invoke(test_app, ["thing", "resolve", "--help"])
    assert result.exit_code == 0, f"thing resolve --help failed: {result.output}"
    assert "--plan-path" in result.output, (
        "Expected --plan-path option in the single-command app's sub-help; "
        f"got: {result.output}"
    )


# ---------------------------------------------------------------------------
# Whole-app build gate: every command group must load via get_command
# ---------------------------------------------------------------------------


def _load_every_command(group, ctx, path):
    """Force-build every command under a click group, recursing into groups.

    Lazy entries resolve to _LazyStub placeholders whose module imports only
    on _load_real(); the walk forces it so a bad parameter annotation fails
    here, at build time, instead of at the operator's first invocation.
    """
    import click

    from fno._lazy_group import _LazyStub

    for name in group.list_commands(ctx):
        cmd = group.get_command(ctx, name)
        if isinstance(cmd, _LazyStub):
            cmd = cmd._load_real()
        assert cmd is not None, f"{path} {name}: get_command returned None"
        if isinstance(cmd, click.Group):
            _load_every_command(cmd, ctx, f"{path} {name}")


def test_every_command_group_builds_via_get_command():
    """The whole fno app must build with typer.main.get_command.

    fno config plugin install crashed only when its command was BUILT (typer
    0.27 rejects a click.Context parameter annotation with 'Type not yet
    supported'), and the lazy registry hid that from every --help path. One
    walk forcing every group to load closes that gap.
    """
    import click as click_mod

    import typer.main

    from fno.cli import app

    root = typer.main.get_command(app)
    ctx = click_mod.Context(root, info_name="fno")
    _load_every_command(root, ctx, "fno")


# ---------------------------------------------------------------------------
# config <-> graph import cycle: broken by the fno.config_io leaf (x-7fdd).
# Guards the invariant that both packages import at module scope in EITHER
# order without ImportError, and that the leaf holds no back-edge.
# ---------------------------------------------------------------------------

def test_config_first_import_does_not_freeze_graph_path_to_fallback(tmp_path):
    """config's graph._constants import stays function-local: a top-level one makes
    `import fno.config` eagerly load the graph package during config's partial init,
    which freezes store.read_graph's GRAPH_JSON default to the ~/.fno fallback and
    silently ignores a configured state_dir. Regression guard: with a state_dir
    override, config-first import must still resolve the graph anchor."""
    import os

    cfg = tmp_path / "config.toml"
    state_dir = tmp_path / "state"
    # The fresh state dir holds no store yet, so the resolution ladder
    # answers the db/ spelling.
    graph_json = state_dir / "db" / "graph.json"
    cfg.write_text(f'state_dir = "{state_dir}"\n')

    code = (
        "import fno.config, fno.graph, inspect\n"  # config-first (the risky order)
        "import fno.graph.store as store\n"
        "d = inspect.signature(store.read_graph_strict).parameters['path'].default\n"
        "print(str(d))\n"
    )
    result = subprocess.run(
        [sys.executable, "-c", code],
        capture_output=True, text=True, timeout=60,
        env={**os.environ, "FNO_CONFIG": str(cfg)},
    )
    assert result.returncode == 0, result.stderr
    assert str(graph_json) in result.stdout, (
        f"read_graph default froze to the fallback, not the configured path:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# AC5-ERR / AC6-EDGE: a missing fno submodule names the reinstall window
# ---------------------------------------------------------------------------

def test_fno_module_import_failure_names_reinstall_window():
    """AC5-ERR: imports happen at invocation time, so `uv tool install --reinstall`
    replaces the package underneath a running process and any not-yet-imported
    subcommand fails for the length of the install. The operator-facing message must
    name both candidate causes (reinstall in flight, stale install) and the fix for
    each, not just the transient one."""
    from fno._lazy_group import make_lazy_group_cls
    import typer
    from typer.testing import CliRunner

    bad_cls = make_lazy_group_cls({"bad": "fno.does_not_exist_module_xyz:cli"})
    # Mirror the real app's error-path config (see fno.cli.app), so this exercises
    # the rendering the operator actually gets.
    test_app = typer.Typer(
        cls=bad_cls,
        no_args_is_help=True,
        pretty_exceptions_enable=False,
        rich_markup_mode=None,
    )

    @test_app.callback()
    def _cb() -> None:
        pass

    result = CliRunner().invoke(test_app, ["bad"])
    assert result.exit_code != 0
    combined = (result.output or "") + (result.stderr if hasattr(result, "stderr") else "")
    # Collapse whitespace: any renderer is free to wrap the message across lines.
    flat = " ".join(combined.replace("│", " ").split())
    assert "reinstalled underneath the running process" in flat, combined
    assert "fno doctor" in flat, combined


# ---------------------------------------------------------------------------
# The bounded wait: absent-then-present, the budget cap, the spent cap
# ---------------------------------------------------------------------------

# A parent whose import costs nothing: find_spec on a dotted name imports the
# parent first, so every fake-absent name below hangs directly off `fno`.


def test_appears_on_disk_succeeds_once_the_module_lands_within_the_budget(monkeypatch):
    """Absent on the first look, present on the third: the import wins the race
    the single-look guard used to lose. This is the reinstall window, shrunk
    but still real, being closed instead of conceded."""
    import importlib.util
    import time as time_mod
    from types import SimpleNamespace

    import fno

    monkeypatch.setattr("fno._recheck_budget_spent", False)
    monkeypatch.setattr(time_mod, "sleep", lambda s: None)
    looks = {"n": 0}
    real_find_spec = importlib.util.find_spec

    def flaky(name):
        if name == "fno._mid_reinstall":
            looks["n"] += 1
            if looks["n"] >= 3:
                return SimpleNamespace(loader=object())
            return None
        return real_find_spec(name)

    monkeypatch.setattr(importlib.util, "find_spec", flaky)

    assert fno._module_appears_on_disk("fno._mid_reinstall") is True
    assert looks["n"] == 3, "answered the moment the module appeared, not at budget end"


def test_appears_on_disk_spends_the_full_budget_before_answering_absent(monkeypatch):
    """Absent for longer than the budget: False after exactly the budget's
    polls, and the once-per-process cap marked spent."""
    import time as time_mod

    import fno

    monkeypatch.setattr("fno._recheck_budget_spent", False)
    slept: list[float] = []
    monkeypatch.setattr(time_mod, "sleep", slept.append)

    assert fno._module_appears_on_disk("fno._never_shipped") is False
    assert len(slept) == fno._VERIFY_ATTEMPTS, slept
    assert fno._recheck_budget_spent is True, "exhaustion must arm the spent cap"


def _counting_import(monkeypatch, target: str, results: list):
    """Patch import_module so calls to `target` pop from `results`; count the calls."""
    import importlib as _il
    from fno import _lazy_group as lg

    calls: list[str] = []
    real = _il.import_module

    def fake(name, package=None):
        if name == target:
            calls.append(name)
            outcome = results.pop(0)
            if isinstance(outcome, BaseException):
                raise outcome
            return outcome
        return real(name, package)

    monkeypatch.setattr(lg.importlib, "import_module", fake)
    return calls


def test_lazy_import_retries_once_when_the_tree_changed_underneath(monkeypatch):
    """AC8-WIN: a reinstall can replace the package between process start and the
    lazy import. When the missing module is present again on re-check, the command
    must succeed rather than surface a transient failure to the operator."""
    from fno import _lazy_group as lg
    from fno._lazy_group import make_lazy_group_cls
    import typer
    from typer.testing import CliRunner

    real_target = importlib.import_module("fno.graph.cli")
    calls = _counting_import(
        monkeypatch,
        "fno.graph.cli",
        [ModuleNotFoundError("gone", name="fno.graph._mid_reinstall"), real_target],
    )
    monkeypatch.setattr(lg, "_module_appears_on_disk", lambda name: True)

    app = typer.Typer(
        cls=make_lazy_group_cls({"backlog": "fno.graph.cli:cli"}),
        no_args_is_help=True,
        pretty_exceptions_enable=False,
        rich_markup_mode=None,
    )

    @app.callback()
    def _cb() -> None:
        pass

    result = CliRunner().invoke(app, ["backlog", "--help"])
    assert len(calls) == 2, f"expected exactly one retry, got {len(calls)} imports"
    assert result.exit_code == 0, result.output


def test_lazy_import_does_not_retry_when_the_module_is_really_missing(monkeypatch):
    """AC8-WIN, the half that keeps this honest: a genuinely stale or broken install
    answers 'absent' on re-check, so it is NOT retried and still fails with the
    message it fails with today. Without this the retry would be a blind sleep."""
    from fno import _lazy_group as lg
    from fno._lazy_group import make_lazy_group_cls
    import typer
    from typer.testing import CliRunner

    calls = _counting_import(
        monkeypatch,
        "fno.graph.cli",
        [ModuleNotFoundError("gone", name="fno.graph._really_gone")],
    )
    monkeypatch.setattr(lg, "_module_appears_on_disk", lambda name: False)

    app = typer.Typer(
        cls=make_lazy_group_cls({"backlog": "fno.graph.cli:cli"}),
        no_args_is_help=True,
        pretty_exceptions_enable=False,
        rich_markup_mode=None,
    )

    @app.callback()
    def _cb() -> None:
        pass

    result = CliRunner().invoke(app, ["backlog", "--help"])
    assert len(calls) == 1, f"a missing module must not be retried; got {len(calls)}"
    assert result.exit_code != 0
    combined = (result.output or "") + (result.stderr if hasattr(result, "stderr") else "")
    flat = " ".join(combined.replace("│", " ").split())
    assert "fno doctor" in flat, combined


# ---------------------------------------------------------------------------
# AC8-HOOK: the same verify-then-retry, for the imports the lazy group cannot see
# ---------------------------------------------------------------------------


def _installed_finder():
    """The one guard instance `import fno` appended to sys.meta_path."""
    import fno

    found = [f for f in sys.meta_path if isinstance(f, fno._ReinstallWindowFinder)]
    assert len(found) == 1, f"expected exactly one guard, got {len(found)}"
    # Behind PathFinder, so it is consulted only after every normal finder has
    # already answered "no such module". Anywhere earlier and it would pay its
    # re-check on imports that were about to succeed. Asserted as "after
    # PathFinder" rather than "last", because any library imported later is free
    # to append a finder of its own and that would not break this one.
    # Found by name, not by identity: a test that spies on
    # `importlib.machinery.PathFinder` must not also move this assertion.
    path_finder = max(
        i for i, f in enumerate(sys.meta_path) if getattr(f, "__name__", "") == "PathFinder"
    )
    assert sys.meta_path.index(found[0]) > path_finder
    return found[0]


def _spy_path_finder(monkeypatch, seen: list, spec="SPEC"):
    """Replace PathFinder so the retry lookup is countable rather than inferred."""
    import importlib.machinery

    class _Spy:
        @staticmethod
        def find_spec(name, path=None, target=None):
            seen.append(name)
            return spec

    monkeypatch.setattr(importlib.machinery, "PathFinder", _Spy)


def test_import_hook_retries_a_module_that_is_on_disk_now(monkeypatch):
    """A tree that is whole again gets one more lookup, and the import proceeds.

    This is the same disk-recheck-then-retry-once `_load_real` does, reaching the
    ~2000 function-level `from fno. ...` imports inside command bodies that the
    lazy group never sees -- `fno agents truth` -> `fno.agents.session_truth`
    among them.
    """
    import fno
    import fno._lazy_group

    seen: list[str] = []
    finder = _installed_finder()
    # The gate is the SHARED helper, the same one `_load_real` consults. Asserted
    # by identity rather than left to the patch below: `_lazy_group` binds the
    # function by VALUE at import time, so monkeypatching `fno` reaches the
    # finder only. An inlined second copy of the on-disk check in `_load_real`
    # would sail past the patch, and this line is what refuses it.
    assert fno._lazy_group._module_appears_on_disk is fno._module_appears_on_disk
    checked: list[str] = []
    monkeypatch.setattr(fno, "_module_appears_on_disk", lambda name: checked.append(name) or True)
    _spy_path_finder(monkeypatch, seen)

    spec = finder.find_spec("fno.agents.session_truth", None, None)

    assert spec == "SPEC"
    assert checked == ["fno.agents.session_truth"], "the shared re-check is the gate"
    assert seen == ["fno.agents.session_truth"], "retried exactly once, no loop"


def test_namespace_refuser_blocks_the_poison_before_it_caches(monkeypatch, tmp_path):
    """End-to-end through the real import machinery: a subpackage directory
    without its __init__.py mid-swap must NOT cache an empty namespace module.
    PathFinder answers namespace specs BEFORE the last-resort guard is ever
    consulted, so a finder ahead of PathFinder has to refuse the shape; the
    same import retried after the swap lands on the real package."""
    import sys as _sys

    import fno

    # The refuser gates on the fno prefix; aim it at a synthetic top name so
    # the test drives the real meta_path ordering without touching real fno.
    monkeypatch.setattr(fno, "_is_fno_module", lambda n: n.startswith("zzns."))
    refuser = fno._FnoNamespaceRefuser()
    path_finder_at = next(
        i for i, f in enumerate(_sys.meta_path) if getattr(f, "__name__", "") == "PathFinder"
    )
    _sys.meta_path.insert(path_finder_at, refuser)
    pkg = tmp_path / "zzns"
    pkg.mkdir()
    (pkg / "__init__.py").write_text("x = 1\n", encoding="utf-8")
    (pkg / "claims").mkdir()  # the mid-swap shape: dir sans __init__.py
    _sys.path.insert(0, str(tmp_path))
    try:
        import zzns  # the real parent imports fine

        assert zzns.x == 1
        with pytest.raises(ModuleNotFoundError) as excinfo:
            import zzns.claims

        assert "part of fno itself" in str(excinfo.value), str(excinfo.value)
        assert "zzns.claims" not in _sys.modules, "the empty namespace must not cache"

        (pkg / "claims" / "__init__.py").write_text("y = 2\n", encoding="utf-8")
        import zzns.claims  # the retry after the swap lands

        assert zzns.claims.y == 2
    finally:
        _sys.path.remove(str(tmp_path))
        _sys.meta_path.remove(refuser)
        for name in [m for m in list(_sys.modules) if m.startswith("zzns")]:
            _sys.modules.pop(name, None)


def test_absent_fno_submodule_names_both_causes_end_to_end():
    """The acceptance shape, through a real import in a real process: a function-level
    `from fno. ...` for a module that is not there says why, twice over."""
    proc = _run_py(
        "import fno\n"
        "try:\n"
        "    from fno.agents.no_such_module import thing\n"
        "except ModuleNotFoundError as exc:\n"
        "    print(exc)\n"
    )
    assert proc.returncode == 0, proc.stderr
    assert "reinstalled underneath the running process" in proc.stdout, proc.stdout
    assert "fno doctor update" in proc.stdout


# ---------------------------------------------------------------------------
# AC-ENTRY: the console entrypoint carries the hint the import layer drops
# ---------------------------------------------------------------------------

# A live specimen of 2026-09-04, verbatim shape.
_FROMLIST_SWALLOW = (
    "cannot import name '_subprocess_util' from 'fno'",
    "fno",
)


def _run_main_with_raising_app(exc_type: str, msg: str, name: str) -> subprocess.CompletedProcess[str]:
    """Call `main()` through a stubbed app raising `exc_type(msg, name=name)`."""
    return _run_py(
        "import fno.cli\n"
        f"def boom(): raise {exc_type}({msg!r}, name={name!r})\n"
        "fno.cli.app = boom\n"
        "try:\n"
        "    fno.cli.main()\n"
        "except ImportError as e:\n"
        "    print('MSG', e)\n"
    )


def test_entrypoint_carries_reinstall_hint_on_fromlist_swallow():
    """Specimen 1: `from fno import _subprocess_util` mid-reinstall. The finder is
    never consulted, so the operator saw a bare ImportError. `main()` is the one
    layer that can read it."""
    msg, name = _FROMLIST_SWALLOW
    proc = _run_main_with_raising_app("ImportError", msg, name)
    assert proc.returncode == 0, proc.stderr
    assert msg in proc.stdout, proc.stdout
    assert "is part of fno itself" in proc.stdout, proc.stdout
    assert "fno doctor update" in proc.stdout, proc.stdout


def test_fno_py_entrypoint_is_main():
    """The console script must name `main`, not `app`: the shim is regenerated
    from this line on every reinstall, and a bare `app` target is what left the
    hint undelivered."""
    pyproject = (Path(__file__).resolve().parents[1] / "pyproject.toml").read_text(
        encoding="utf-8"
    )
    assert 'fno-py = "fno.cli:main"' in pyproject, pyproject


def test_plain_click_action_help_exits_clean():
    """A plain-click action's --help behind the lazy group (a
    lazy stub) prints its help and exits 0. The
    plain-click Exit used to escape typer's vendored-click main as a
    traceback ending in click.exceptions.Exit: 0."""
    from typer.testing import CliRunner

    from fno import cli as fno_cli

    runner = CliRunner()
    result = runner.invoke(fno_cli.app, ["doctor", "test", "--help"])
    assert result.exit_code == 0, result.output
    assert "Usage" in result.output
    assert result.exception is None or isinstance(result.exception, SystemExit)
