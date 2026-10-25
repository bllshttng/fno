# fno agents — MCP channel + sidecar

> **Partly retired.** The Python ask legs this page's send ladder names (`ask_followup`, `ask_followup_via_mcp`, `MCPChannelSendError` in `harnesses/claude.py`) were ported to the Rust runtime and deleted (the ask-adapter port, 2026-09-02). `mcp_channel_reachable` survives for the spawn surface. The page stays as the design record of the demotion ladder it once drove.

The MCP channel backend gives `fno agents mail ask` a second send path that rides on Anthropic's sanctioned `claude/channel` capability instead of the reverse-engineered `messagingSocketPath` Unix-domain socket. Both backends are supported indefinitely; the dispatcher prefers MCP when available and falls back to the socket on probe failure. The user-visible reply is identical regardless of which backend delivered the message — only the wire transport differs.

Parent: [fno-agents-registry-and-dispatch.md](fno-agents-registry-and-dispatch.md). The socket path it complements: [fno-agents-followup.md](fno-agents-followup.md).

## Two artifacts, two lifecycle owners

```
+--------------------+        +-----------------+        +--------------+
|   fno agents mail ask   |        | footnote side- |        |  channel_    |
|     (CLI caller)   |--Unix->|  car daemon     |--Unix->|  server      |
|                    |  sock  |  (per-user)     |  push  |  (per-       |
|                    |        |                 | -down  |   session,   |
|                    |        |                 |  conn  |   CC-owned)  |
+--------------------+        +-----------------+        +--------------+
                                                                |
                                                                | stdio
                                                                v
                                                       +---------------+
                                                       | Claude Code   |
                                                       | session       |
                                                       +---------------+
```

Both the per-user sidecar daemon and the per-session channel-server child ship in `crates/fno-agents/src/mcp/`, but they have **different lifecycle owners**:

- **Sidecar (`mcp/sidecar.rs`)** is fno-owned. Lazy-start / lazy-exit. The bind path resolves to `$XDG_RUNTIME_DIR/fno/sidecar.sock` if set, else the state dir's `sidecar.sock` (`$FNO_SIDECAR_SOCKET` overrides both). Single-leader via socket-bind exclusivity. Idle-exits after 30 minutes with zero registered channels.
- **Channel server (`mcp/server.rs`)** is CC-owned. CC spawns the child as a stdio subprocess from an MCP config entry naming the fno-agents-worker binary (`fno-agents-worker --mcp-exec serve --session-id <id> --channel-name <name>`). fno has no `start`/`stop` for it. The plugin tree itself carries no `.mcp.json` today: an operator wiring the channel adds that config entry by hand.

The sidecar exists because CC spawns one channel server per session and they don't share pipes. Without a per-user rendezvous, external `fno agents mail ask` pokes from sibling processes cannot reach the right channel server. The sidecar holds an in-memory `session_id -> writer` map and routes pokes to the registered child.

## Routing decision tree

For a follow-up against an existing claude agent (the read-path lives in `dispatch.py:_followup`):

```
1. Read AgentEntry under with_agent_lock_and_entry.
2. If entry.provider != "claude" -> existing socket path (no MCP).
3. If entry.mcp_channel_id is None -> socket path.
4. Else:
   a. Probe via mcp_channel_reachable(mcp_channel_id, timeout=250ms).
   b. True  -> ask_followup_via_mcp(...). emit agent_followup_done(backend="mcp").
   c. False -> ask_followup(...). emit mcp_channel_demoted_to_socket(reason="channel_not_registered").
   d. Raises ReachabilityProbeError ->
        ask_followup(...). emit mcp_channel_unreachable(reason="mcp_channel_disconnected").
   e. probe True but send raises MCPChannelSendError ->
        ask_followup(...). emit mcp_channel_demoted_to_socket(reason="send_failed_post_probe:<...>").
```

The `backend` field on `agent_followup_done` (`"mcp"`, `"socket"`, or `"socket_after_mcp_demote"`) gives forensic analytics a clean way to slice send-path outcomes.

## Modules and their boundaries

```
crates/fno-agents/src/mcp/
├── mod.rs             # the worker's --mcp-exec door: send | status |
│                      # serve | sidecar sub-ops
├── channel.rs         # WIRE FORMAT - single source of truth.
│                      # build + validate + drift-diff, pinned by the
│                      # characterization tests in this module.
├── server.rs          # stdio MCP child. Declares experimental
│                      # claude/channel capability. Forwards inbound
│                      # pokes (received from sidecar) as
│                      # notifications/claude/channel on stdout.
├── sidecar.rs         # Unix-socket daemon. Single-leader. Holds
│                      # session_id -> channel-writer map.
│                      # stale-socket recovery bounded to two
│                      # attempts around the bind.
└── client.rs          # sync Unix-socket client. Lazy-starts the
                       # sidecar (`--mcp-exec sidecar`) if missing.
                       # The daemon's push_to_channel delivery leg
                       # answers through it in-process.
```

## Wire format pin

The `notifications/claude/channel` envelope shape comes from Claude Code's channels reference (§Notification format). We mirror it byte-for-byte:

```json
{
  "jsonrpc": "2.0",
  "method": "notifications/claude/channel",
  "params": {
    "content": "<UTF-8 body>",
    "meta": {"from_name": "...", "session_id": "...", "...": "..."}
  }
}
```

`meta` keys must match `^[A-Za-z0-9_]+$` because CC silently drops non-identifier keys (hyphens, dots, spaces). The builder (`build_channel_notification`) refuses to emit them so the operator sees a loud build error instead of a silently-stripped attribute.

The pin lives in the characterization tests in `crates/fno-agents/src/mcp/channel.rs` (goldens captured from the Python leg before deletion): build shape, every validate reason string, and the drift diff. A CC research-preview API drift FAILS LOUDLY in CI before reaching production callers. The old `MCP_SMOKE=1` smoke script and its fixture died with the Python leg.

## mcp_channel_id

`mcp_channel_id` is populated from the agent's `claude_short_id` (1:1 mapping) so the sidecar routes by claude's native session id without an id-translation layer. The field type (`Optional[str]`) permits swapping to a server-generated UUIDv4 without a registry-schema bump.

## Operational notes

- Sidecar logs land under `$XDG_STATE_HOME/fno/sidecar.log` (default `~/.local/state/fno/sidecar.log`). The lazy-start appends there.
- The sidecar state file (`sidecar-state.json`) is flushed on SIGTERM but not on SIGKILL. Channel servers re-register on next session boot; the registry's `mcp_channel_id` is the persistent source of truth.
- macOS AF_UNIX path limit is 104 chars. Long `$HOME` values are rare in practice, but the operator-visible error (`AF_UNIX path too long`) is transparent.
- The verb answers natively: `fno agents mcp send --session-id <id>` (envelope on stdin, exit 0/1/2) execs the worker's `--mcp-exec send` lane. The reachability probe rides `--mcp-exec status`. Run the sidecar by hand with `fno-agents-worker --mcp-exec sidecar`.
