# Launch an opencode worker

Plain opencode is opencode 2.x on every fleet machine. The `opencodev2` wrapper and its side-by-side `~/.opencode-v2` tree are retired. One binary reads the default XDG roots. 2.x keeps its credentials in the data root's `opencode.db` (`credential` table), not `auth.json`. Pick the host by who watches.

An unattended teammate runs on fno's opencode serve lane. Spawn starts one shared `opencode serve` and drives the first turn with a detached writer:

```bash
fno agents spawn -H opencode -m zai-coding-plan/glm-5.3-flash --name <name> --node <node> '/fno:target <node>'
```

The serve lane is measured on opencode 2.2.2 and 2.0.19. The worker spawned, ran its first turn, drained mail at the turn boundary, and replied through `fno agents mail reply`. The manual v2 line stays unmeasured. The 2.x help lists `--server <url>` in place of `--attach`. Whether `run` expands a leading `/fno:target` is unmeasured, so prefer spawn.

## The payload

The payload starts with `/fno:`. `-m` takes opencode's own `provider/model` id (`zai-coding-plan/glm-5.3-flash`). A bare name refuses at the spawn door. opencode declares no efforts, so leave `--effort` off. The one permission mode is `auto`. `--add-dir` is rejected.

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
- `spells its model provider/model`: the launch passed a bare model name like `glm-5.3-flash`. Pass the provider-qualified id, e.g. `zai-coding-plan/glm-5.3-flash`.

Back to [the harness choice table](../SKILL.md#launch-a-worker).
