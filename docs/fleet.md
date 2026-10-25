# Fleet: run several agents at once

## Is this page for you?

You ship with fno on one project and now want several workers at once, maybe on several harnesses. This page links each fleet task to its guide: spawn, watch, route, and recover.

Not for: one agent on one feature. That is [workflows](workflows.md).

## Start here

Spawn a worker on another harness and ask it a question:

```bash
fno agents spawn "find why tests/test_login.py is flaky" --name helper -H codex
fno agents mail ask helper "what did you find?"
```

For a one-off question, `--once` runs the worker headless, prints the reply, and tears it down. The [agents quickstart](guides/agents-quickstart.md) walks through both.

To dispatch every ready backlog node as a background worker: `/fno:target bg --all-ready`.

## Spawn and message

- [Spawn a peer](guides/fno-agents-spawn.md), and the [spawn flag reference](guides/agents-spawn-flags.md).
- [Send an async message](guides/fno-agents-send.md), or [ask a running agent](guides/fno-agents-ask-followup.md).
- [List agents and tail their logs](guides/fno-agents-list-logs.md).
- [Stop, remove and reconcile agents](guides/fno-agents-stop-rm-reconcile.md).
- [Message between projects](guides/cross-project-inbox.md).
- Seat a worker on one harness: [claude](harnesses/launch/launch-claude.md), [codex](harnesses/launch/launch-codex.md), [opencode](harnesses/launch/launch-opencode.md), [agy](harnesses/launch/launch-agy.md), [pi](harnesses/launch/launch-pi.md).

## Watch

- [Mux keys](mux-keys.md) and [mux themes](mux-themes.md).
- [Org view](architecture/org-view.md): one key press, the whole fleet.
- [Watch panes from a browser](guides/web-view.md).
- [Open a file from a pane](architecture/mux-open-files.md).
- [Open delivered mail in Claude Code](architecture/claude-mail-pane.md).
- [Send run events to a chat channel](status-fanout.md).
- [How questions reach you away from the terminal](architecture/attention-items.md).

## Route

- [The five routing axes](architecture/axis-vocabulary.md): harness, provider, model, effort, account. Never confuse them.
- [Per-stage model routing](architecture/role-based-model-routing.md).
- [Accounts, failover and combos](provider-rotation.md).
- [Qualify a model lane](architecture/lane-qualification.md).
- [Managed Claude slot changes](architecture/managed-claude-slot-cutover.md).
- [Session lanes](architecture/thread-lanes.md), [portals](architecture/portals.md), and [cloud workers](architecture/cloud-substrate.md).

## Scale

- [How many lanes the machine can take](architecture/resource-meter.md), and the [fleet load report](guides/fleet-load.md).
- [The tenured lead role](architecture/lead.md) and the [fleet FAQ](fleet-faq.md).
- [One fleet over several machines](architecture/multiplayer.md), and its [two-machine test](guides/multiplayer-test.md).
- [Scheduled loops](loops.md).

## Recover

- [Bring workers back after a reboot](architecture/workspace-restore.md).
- [What survives a mux reconnect and a restart](architecture/mux-restart-recovery.md), and [why panes outlive the mux server](architecture/pane-keeper.md).
- [Which background process is which](architecture/background-processes.md).
- [Why a session row is still on the roster](reaping-faq.md).
- [Move the fleet to another computer](architecture/fleet-cutover.md).

## Reference

- [Every agents command per harness](harness-command-matrix.md).
- [Status event wire contract](protocol/a2a-status-events.md) and [telemetry schema](reference/otel-schema.md).
