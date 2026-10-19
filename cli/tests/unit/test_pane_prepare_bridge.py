"""The mux verb's renderer bridge, asserted as a contract.

`fno mux pane send` (Rust) shells to the paired fno-agents binary's `mail
pane-prepare` renderer for every non-raw send, and FAILS CLOSED when that call
cannot run. Fail-closed is right, and it is also why this needs a test: rename
the command or the flag and every existing test still passes while non-raw pane
sends refuse at runtime, quietly, because a refusal is what the design asks for
when the hop is unavailable.

So this reads the argv the Rust source actually builds and asserts the renderer
accepts it. The Python leaf of the same name is a forward to that same binary,
so its declared options are the Python-side mirror of the same contract. The
renderer's gate and refusal behavior itself is pinned in Rust (the
mail_pane_prepare parity goldens); nothing here re-tests it through the
forward.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.mail.cli import mail_app

REPO = Path(__file__).resolve().parents[3]
# The bridge lives in its own module since the pane-prepare port; read it there.
BRIDGE_SRC = REPO / "crates" / "fno" / "src" / "mux_cli" / "pane_prepare.rs"


def _bridge_argv() -> list[str]:
    """The literal argv `prepare_pane_bytes` passes to the child."""
    src = BRIDGE_SRC.read_text(encoding="utf-8")
    block = re.search(
        r"fn prepare_pane_bytes.*?\.args\(\[(.*?)\]\)", src, re.S
    )
    assert block, "prepare_pane_bytes no longer builds a literal .args([...])"
    return re.findall(r'"([^"]+)"', block.group(1))


def _declared_options() -> set[str]:
    """Every option string `agents mail pane-prepare` declares."""
    from typer.main import get_command

    group = get_command(mail_app)
    prepare_cmd = group.commands["pane-prepare"]  # type: ignore[attr-defined]
    opts: set[str] = set()
    for param in prepare_cmd.params:
        opts.update(getattr(param, "opts", []) or [])
        opts.update(getattr(param, "secondary_opts", []) or [])
    assert opts, "pane-prepare declares no options; the introspection broke"
    return opts


def test_the_rust_bridge_names_a_command_this_cli_has():
    argv = _bridge_argv()
    # The renderer dispatch shape: the paired binary takes `mail`, then the verb.
    assert argv[:2] == ["mail", "pane-prepare"], argv

    result = CliRunner().invoke(mail_app, ["pane-prepare", "--help"])
    assert result.exit_code == 0, (
        f"the Rust bridge renders through `fno-agents mail pane-prepare`, whose "
        f"Python forward no longer accepts its own flags: {result.output}"
    )


@pytest.mark.parametrize("flag", ["--session-id"])
def test_the_rust_bridge_flags_still_exist(flag):
    """Each flag the bridge passes must be one the command still takes.

    `--session-id` in particular was renamed once already, with `--session`
    kept as a hidden deprecated alias. A second rename that forgot the Rust
    caller would refuse every non-raw send and no test would say so.
    """
    assert flag in _bridge_argv(), f"{flag} is no longer in the bridge argv"
    # Introspect the parameters, never the rendered help. Typer draws help in a
    # Rich box that wraps to the terminal width, so a narrower CI terminal
    # splits a long flag across lines and a substring search reports it missing.
    # The parameter list is what the command actually accepts, and it does not
    # depend on how wide the screen is.
    assert flag in _declared_options(), (
        f"the pane-prepare forward no longer accepts {flag}; the Rust bridge passes it"
    )
