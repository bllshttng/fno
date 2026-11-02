# Remote sessions in the mux

The sideline can show every machine on the fleet map. A session on another machine opens in your mux as a local pane, live and with input. No client merges two servers. The pane runs ssh, and the host's own build talks to the host's own server. Ssh carries only terminal bytes, so the two machines never share a wire protocol.

## Group by machine

`prefix+W` cycles the agent group, and `machine` is one of its values. The sideline then paints a band for each machine, a band for each project inside it, then the rows. This machine comes first. Each machine band reads `host · ready · 23s`, or `offline` when the fleet map calls the machine stale. The number is the age of its newest beat.

This machine's rows come from the server's layout. Other machines' rows come from `fno agents fleet top --machines`, read off the UI loop every 15 seconds while the axis is on. A failed read keeps the last good bands, so a row does not vanish because the primary blinked. With fewer than two machines the axis paints the workspace tree, and nothing connects to another machine until you open a row.

A remote row is named `name@host`. Peek and mail already route that address.

## Open a row

Click a remote row or press Enter on it. A new tab opens a local pane named `name@host`, and its child is `fno mux serve --link <key> --host <host>`. Opening the row again focuses that pane. The client finds the pane by reading each pane's child argv, so the answer stays true across a client restart with no stored state.

The menu on a remote row offers Open here, Peek and Mail. Stop, kill and retire stay hidden. To stop a session on another machine, open it and act inside it. Stopping the local pane closes the link and leaves the session running on its host.

The link pane is an ordinary pane, so the keeper holds it across a client quit and a server restart. See [pane keeper](pane-keeper.md). It has no registry row, so the fleet map does not publish it as a session on this machine.

## The two kinds of row

The far door, `fno mux serve --link <key>`, resolves the key on the host: session id first, then name.

| The row | What runs on the host | Effect on the host |
|---|---|---|
| Hosted in a pane | A passive client tagged to that one pane | None. It births no workspace and is not counted in the tab's size |
| A paneless thread | `fno-agents attach --inline <name>` in the ssh terminal | None. The host's portals stay as they were |

The server gate is `crates/fno/src/server/links.rs`. A tagged client may send input, mouse and scroll for its own pane and nothing else. The hello carries the pane's child pid, so a pane id reused between the read and the hello is refused. An untagged passive client stays read-only.

Keys carry no author on the wire. Both ends are the same person, so the far door does not redact the pane.

## When the connection drops

`fno-agents attach --dial <host> <key>` supervises ssh on the pane's own terminal. It copies no bytes. It reads the far door's exit code.

| Exit | Meaning | The pane does |
|---|---|---|
| 0 | You left | Closes |
| 2 | Usage, or a host build with no `--link` | Stops and names `fno doctor update` |
| 3 | The session is gone | Stops and says so |
| 4, or ssh 255, or a signal | A lost path | Keeps the last screen and redials |

A redial waits 1 second, then doubles up to 15 seconds. After 180 seconds with no session, the pane stops and says to reopen the row. A session that ran for 30 seconds proves the path, so the next drop starts a fresh budget. Ssh keepalives find a dead path in 45 seconds.

While it redials, the top row reads `[reconnecting to host: retry in 3s, typed keys are dropped]`. The supervisor flushes queued input before each dial, and the far door drops input from its first 300 ms. A key typed during a drop never reaches an agent late.

## Build requirements

Both machines need a build with the link. A host without it answers exit 2, and the pane names the update command. A host server older than the link build refuses the attach and names `fno mux kill-server`. The far door checks this before it sends the hello, so an old server does not read as a network drop.

## Size

The link never resizes the host's pane. See [mux window size](mux-window-size.md#a-remote-link-does-not-resize).

## Test it by hand

A terminal that never answers the image query leaves the client's query thread blocked on stdin. That thread eats the next input and restores cooked mode, so a mouse report waits in the line buffer until a newline arrives. A real terminal answers. A pty harness must answer the query or set `FNO_MUX_IMAGE_PROTOCOL=halfblocks`, or every click reads as a dead row.
