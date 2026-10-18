# Launch an opencode worker

opencode has no paneless background start of its own that fno has measured on v2. Pick the host by who watches.

An unattended teammate runs on fno's opencode serve lane. Spawn starts one shared `opencode serve` and drives the first turn with a detached writer:

```bash
fno agents spawn -H opencode -m glm-5.3-flash --name <name> --node <node> '/fno:target <node>'
```

The native line fno measured on opencode 1.18.33 is:

```bash
opencode run --attach <serve-url> --title <name> -m zai/glm-5.3 --command fno:target <node>
```

opencode v2 removed `--attach` and `--command` from `run`. It added `--server <url>`, `--title` and `--auto`. Whether v2 `run` expands a leading `/fno:target` in the message is unmeasured. Read the flag tables below before you type a v2 line, and prefer spawn until a v2 launch is measured.

## The payload

The payload starts with `/fno:`. opencode declares no efforts, so leave `--effort` off. The one permission mode is `auto`. `--add-dir` is rejected.

## What fno does around the line

- **Gate.** The PreToolUse Bash hook reads the line and asks the spawn gate.
- **Record.** The hook writes a launch record. It reads the name from `--title`.
- **Adopt.** fno's opencode plugin reports `session.created`, and fno mints the row.

The loop closes through a JS extension fno installs, not a shell hook. A `/fno:target` payload therefore needs a current install. The loop gate checks it before the spawn.

## Get the result back

- `fno agents list` and `fno agents peek <name>` read the row and its latest turn.
- `opencode session list --format json` lists sessions on the serve.

## Known refusals

`closes its loop through a fno-installed extension that is absent or stale`. The text in parentheses names the cause:

- `opencode binary was not found`: fno looks at `FNO_OPENCODE_BIN`, then `~/.opencode/bin/opencode`, then `opencode` on PATH. Set the variable or install the binary.
- `installed X vs source Y`: the extension is behind the plugin source. Run `fno config plugin install opencode`.
- `the opencode contract changed since install`: opencode crossed a major version after the install. Run the same install command.

Back to [the harness choice table](../SKILL.md#launch-a-worker).
