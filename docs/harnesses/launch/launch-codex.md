# Launch a codex worker

codex has no paneless background start of its own. Pick the host by who watches.

An unattended teammate runs on fno's codex thread lane. Spawn hosts it on the codex app-server and keeps no pane:

```bash
fno agents spawn -H codex -m gpt-6-sol --effort high -Y --name <name> --node <node> '$fno:target <node>'
```

To watch the worker, run codex's own line in a keeper pane. fno places the pane. Every word after `--` reaches codex verbatim:

```bash
fno mux pane run --cwd <repo-root> -- codex -m gpt-6-sol -a never -s workspace-write -c model_reasoning_effort=high '$fno:target <node>'
```

## The payload

The payload starts with `$fno:`, never `/`. Single-quote it. Inside double quotes the shell expands `$fno` to nothing, and the worker reads the rest as prose. Spawn refuses both bad shapes and names the fix.

## What fno does around the line

- **Gate.** The PreToolUse Bash hook reads either line and asks the spawn gate. A refusal denies the Bash call and names the remedy.
- **Record.** The hook writes a launch record with your session as the parent. For a pane run it parses the codex argv after `--`.
- **Adopt.** codex's SessionStart hook (`hooks/codex-hooks.json`) reports the session, and fno mints its row.

On spawn, `-Y` maps to codex `--dangerously-bypass-approvals-and-sandbox`. On the native line, write codex's own flag. Models: `gpt-6-astra`, `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`.

## Get the result back

- `fno agents list` and `fno agents peek <name>` read the row and its latest turn.
- `fno agents mail send <name> "<text>"` reaches it. A busy worker leaves mail in the composer, so peek after every send.
- `codex resume <id> --remote unix://` rejoins a session by hand.

## Known refusals

- The footnote plugin is `missing` or `wrong-channel` in the codex home. Spawn refuses a command-shaped seed. Run `fno config plugin install codex`, then launch again.
- `--permission-mode` on a thread or headless spawn. Use `-Y`, or run the native line in a pane.
- `--agent` is rejected. codex reaches sub-agents through its own project agents.

Back to [the harness choice table](../SKILL.md#launch-a-worker).
