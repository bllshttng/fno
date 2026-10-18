# Launch a pi worker

pi has no paneless background start of its own. Pick the host by who watches.

An unattended teammate runs on fno's keeper-backed thread lane:

```bash
fno agents spawn -H pi --name <name> --node <node> '/fno:target <node>'
```

To watch the worker, run pi's own line in a keeper pane. Every word after `--` reaches pi verbatim:

```bash
fno mux pane run --cwd <repo-root> -- pi --session-id <uuid> --model zai/glm-5.3:high '/skill:target <node>'
```

## The payload

On spawn, write `/fno:target`. The seed renders it as `/skill:target`, the form pi registers from its skills directory. On the native line, write `/skill:target` yourself. pi raises no permission prompts.

## What fno does around the line

- **Gate.** The PreToolUse Bash hook reads the line and asks the spawn gate.
- **Record.** The hook writes a launch record with your session as the parent.
- **Adopt.** fno's pi extension (`footnote.ts`) reports at `before_agent_start`, and fno mints the row. No pi-code install is needed.

The loop closes through the same `footnote.ts` in the pi agent dir. If that file is absent or differs from the packaged copy, a `/fno:target` spawn refuses and names the path. Run `fno config plugin install pi`, then launch again.

## Get the result back

- `fno agents list` and `fno agents peek <name>` read the row and its latest turn.
- `pi --session <id>`, run from the session's own cwd, rejoins it by hand.

## Known refusals

- The spawn `--resume` fork door is retired. Revive the same row by name with `fno agents resume <name>`.
- An unmeasured substrate. Spawn names it and stops.

Back to [the harness choice table](../SKILL.md#launch-a-worker).
