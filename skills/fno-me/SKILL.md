---
name: fno-me
description: Join this session to the Footnote agent mesh so peers can reach it.
metadata:
  internal: false
  requires:
    binaries:
      - "fno >= 0.3.1"
---

# fno-me

Register THIS session in the fno roster so peer sessions can discover and message
it. A session a human started by hand (not one `fno agents spawn` created) has no
roster row until you run this. Afterward a peer reaches you with
`fno agents mail send <your-handle>` - live if you are running, durable-queued and
drained at your next SessionStart if not.

Run it, then report the resulting handle back to the user:

```bash
fno agents registry register
```

It resolves this session's ambient harness identity and writes an `idle` row named
by the canonical bare short-id handle (e.g. `3ad1f42d`) - the same string this
session self-stamps and drains, and the same one `resume` / `attach` / `peek`
take, so delivery is coherent and a missed send is recoverable by the same id. Idempotent
(safe to re-run) and needs no arguments; pass `--name <name>` only to override the
derived handle. Exit 3 means the session has no addressable harness identity
(nothing to register) - report that rather than inventing a handle.

This joins only THIS session. A session also joins on its own at its first fno skill or verb. A `/fno:` or `$fno:` prompt joins it, and so do an `fno:*` Skill call and an `fno` command it runs. A joined session gets a registry row, a spawn-gate slot and the using-fno preamble. An unjoined one gets none of them. The join writes the marker `~/.fno/agents/sessions/<harness>/<session-id>/joined`. A hook's own fno call never joins, because every hook runs with `FNO_HOOK_CONTEXT=1`. To auto-join every hand-started session in scope, set `agents.auto_register_sessions = true` in config.

That knob joins sessions at SessionStart only, so a session already running, or one another skill tells to join (lead, on titling), still invokes this. This is a skill, not a command. The codex plugin manifest and the agy bundle ship `skills/` only, and a skill is the surface an agent can invoke itself.

## Known Limitations and Deferred Work

- See [LIMITATIONS.md](LIMITATIONS.md).
