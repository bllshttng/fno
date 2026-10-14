# Launch a claude worker

Seat a claude worker with claude's own line. No fno translation layer sits between you and claude, and fno still gates, records and adopts every launch.

```bash
cd <repo-root> && claude --bg --name <name> --model sonnet --effort high --permission-mode auto '/fno:target <node>'
```

`--bg` hands the session to claude's own supervisor and returns at once. It prints a short id. The session hosts no pane. Launch from the repo root: `/fno:target` cold-starts its own worktree off `origin/main`.

## A routed lane: glm through zai

A route rides a `--settings` file, never your shell env. Ask spawn for the exact line, then run what it prints:

```bash
fno agents spawn --name <name> '/fno:target <node>' -P zai -m 'glm-5.3-flash[1m]' --effort xhigh --permission-mode bypassPermissions --print
```

The print writes the lane's settings file and launches nothing. The line it prints reads `claude --bg --settings <file> --model 'glm-5.3-flash[1m]' ...`. Drop `--print` to have spawn run the same line for you.

To pin an account, prefix the line with its config dir: `CLAUDE_CONFIG_DIR=<account dir> claude --bg ...`.

## What fno does around the line

- **Gate.** The PreToolUse Bash hook reads the line and asks the spawn gate. A refusal denies the Bash call and names the reason and the remedy.
- **Record.** The same hook writes a launch record. It names your session as parent, plus the node and the name.
- **Adopt.** claude's SessionStart hook reports the new session. fno claims the record and mints a registry row with that name, parent and node.

fno translates none of claude's flags. The flag tables below come from `claude --help` itself.

## Get the result back

- `fno agents list` shows the row once SessionStart has fired, usually within seconds.
- `fno agents peek <name>` reads its latest turn. `fno agents mail send <name> "<text>"` reaches it.
- claude's own views work too: `claude agents --json`, `claude logs <id>`, `claude attach <id>`.
- The worker's `/fno:target` loop opens the PR. If a covered PR settles, the lead's settle arm mails you.

## Pitfalls

- Only a `--bg` line is adopted. A foreground `claude "<prompt>"` is a person's session and gets no row.
- A native launch moves no node claim. If you hold the node's claim, release it first, or use spawn, which hands it to the child.
- A native launch sets no first-check deadline. Peek once after a few minutes.
- A `--name` that a live row already holds takes a suffix: `<name>-2`.
- Stop a worker with `fno agents stop <name>`. Never run a bare `claude rm <id>`: it deletes the session behind the fno row. `fno agents rm <name>` removes both in order.

Back to [the harness choice table](../SKILL.md#launch-a-worker).
