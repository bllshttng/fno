from __future__ import annotations

import click
from click.testing import CliRunner

from fno._lazy_group import collapse_click_group


def _group(seen: list[tuple[str, str]]) -> click.Group:
    @click.group()
    def group() -> None:
        pass

    @group.command("kept")
    @click.option("--value", required=True)
    def kept(value: str) -> None:
        seen.append(("kept", value))
        click.echo(f"kept={value}")

    @group.command("folded")
    @click.option("--value", required=True)
    def folded(value: str) -> None:
        seen.append(("folded", value))
        click.echo(f"folded={value}")

    return group


def test_t1_action_is_an_argument_that_reaches_the_original_typed_command():
    seen: list[tuple[str, str]] = []
    original = _group(seen)
    collapsed = collapse_click_group(original, keep={"kept"})

    result = CliRunner().invoke(collapsed, ["folded", "--value", "yes"])

    assert result.exit_code == 0, result.output
    assert result.output == "folded=yes\n"
    assert seen == [("folded", "yes")]


def test_keep_action_remains_a_registered_leaf():
    seen: list[tuple[str, str]] = []
    collapsed = collapse_click_group(_group(seen), keep={"kept"})
    ctx = click.Context(collapsed)

    assert collapsed.list_commands(ctx) == ["kept"]
    result = CliRunner().invoke(collapsed, ["kept", "--value", "yes"])
    assert result.exit_code == 0, result.output
    assert seen == [("kept", "yes")]
