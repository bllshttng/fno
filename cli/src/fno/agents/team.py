"""``fno agents team``: one read of every role, its scope, its holder, and
whether the registry and the graph agree.

The read is native (crates/fno-agents/src/team_read.rs). With an installed
binary the runtime router execs it before this front runs, so the front keeps
only its signature for --help and refuses without one.
"""
from __future__ import annotations


def register_team_command(app) -> None:
    """Attach the team front to the agents app."""
    import typer

    @app.command("team", hidden=True)
    def cmd_team(
        json_output: bool = typer.Option(
            False, "--json", "-J", help="Emit JSON instead of the table."
        ),
        nodes: bool = typer.Option(
            False,
            "--nodes",
            "-n",
            help=(
                "Fold each role's scope nodes into its row (counts by status, "
                "then the active nodes with worker, PR, session ids). Implies "
                "JSON output."
            ),
        ),
    ) -> None:
        """The whole team: every live role, its scope, its holder, its
        grantor, and whether the registry and the graph agree.

        Exit 0 always: this is a read, and a caller gates on the JSON keys
        (``agree``, ``summary.disagreements``, ``summary.unknowns``, and
        ``conflicts``), not the process status.
        """
        from fno.agents.rust_runtime import refuse_without_binary

        _ = (json_output, nodes)
        refuse_without_binary("team")
