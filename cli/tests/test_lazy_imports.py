"""Tests for the LazyTypeGroup lazy-import refactor.

Acceptance criteria:
  AC3-FR: no functional regression in existing commands
  AC1-ERR: misconfigured lazy entry fails loud
  AC2-HP: fno --help lists all current subcommands
"""
from __future__ import annotations

import importlib

import pytest


# ---------------------------------------------------------------------------
# AC2-HP: fno --help lists all subcommands (no regression in command surface)
# ---------------------------------------------------------------------------

# Stable subcommands that must appear in `fno --help` after the refactor.
# The curated top-level menu (x-71b6 In-N-Out tiering). `fno --help` advertises
# only these; everything else is hidden but invocable, listed by `fno help --all`.
_ADVERTISED_SUBCOMMANDS = [
    "help",
    "backlog",
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
    "pr-watch",
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
# AC8-WIN: one retry when the tree changed underneath the import
# ---------------------------------------------------------------------------


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

    real_target = importlib.import_module("fno.do_cli")
    calls = _counting_import(
        monkeypatch,
        "fno.do_cli",
        [ModuleNotFoundError("gone", name="fno.do_cli._mid_reinstall"), real_target],
    )
    monkeypatch.setattr(lg, "_module_appears_on_disk", lambda name: True)

    app = typer.Typer(
        cls=make_lazy_group_cls({"do": "fno.do_cli:do_app"}),
        no_args_is_help=True,
        pretty_exceptions_enable=False,
        rich_markup_mode=None,
    )

    @app.callback()
    def _cb() -> None:
        pass

    result = CliRunner().invoke(app, ["do", "--help"])
    assert len(calls) == 2, f"expected exactly one retry, got {len(calls)} imports"
    assert result.exit_code == 0, result.output


# ---------------------------------------------------------------------------
# AC8-HOOK: a mid-swap subpackage never caches as an empty namespace
# ---------------------------------------------------------------------------


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
