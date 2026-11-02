# Remote sessions in the mux

The sideline can show every machine on the fleet map. A session on another machine opens in your mux as a local pane, live and with input. No client merges two servers. The pane runs ssh, and the host's own build talks to the host's own server. Ssh carries only terminal bytes, so the two machines never share a wire protocol.

## The machine layer

The layer is an experimental feature. Turn it on in menu > settings > experimental > `other machines`. It is off by default. While it is off, the mux reads no fleet map, and the sideline paints exactly as before.

While it is on, each machine gets a band at the top of the tree, and the active agent group paints inside each band. `prefix+W` still cycles workspace, lead, cwd, status and harness. It now picks the grouping under every machine. This machine comes first. Each machine band reads `host · ready · 23s`. A machine the fleet map calls stale reads `offline`. The number is the age of its newest beat.

This machine's band holds the ordinary tree from the server's layout. Other machines' rows come from `fno agents fleet top --machines`. The mux reads it off the UI loop every 15 seconds while the toggle is on. A failed read keeps the last good bands, so a row does not vanish because the primary blinked. A machine with no beat yet still gets its own band, which reads `offline`. With fewer than two machines there is no layer. Nothing connects to another machine until you open a row.

The fleet map carries less than the layout, so a far row joins its band by the nearest fact it has:

| Group | A far row joins |
|---|---|
| workspace | its project. The fleet map carries no workspace |
| lead | the first seat up its parent chain that holds a role |
| cwd | the last part of its cwd |
| status | Working while it writes, else Completed. The fleet map has no needs-input state |
| harness | its harness |

The Created and Modified sorts read a far row's start and last-message times, so recency orders far rows too. A pin is a keep mark on the machine that runs the session. The fleet map does not carry it, so a far row never sorts as pinned, and `ctrl+t` on a far row refuses.

A remote row is named `name@host`. Peek and mail already route that address. Its card paints the bare name, and its third line reads `remote · host · state` with the context percent after it. The state is `working` while the seat writes, `idle 3m` with the time since its last message, or `offline 2m` once its machine's beat is stale.

## Open a row

Click a remote row or press Enter on it. A new tab opens a local pane named `name@host`, and its child is `fno mux serve --link <key> --host <host>`. Opening the row again focuses that pane. The client finds the pane by reading each pane's child argv, so the answer stays true across a client restart with no stored state.

The menu on a remote row offers Open here, Peek and Mail. Stop, kill and retire stay hidden. To stop a session on another machine, open it and act inside it. Stopping the local pane closes the link and leaves the session running on its host.

The link pane is an ordinary pane, so the keeper holds it across a client quit and a server restart. See [pane keeper](pane-keeper.md). It has no registry row, so the fleet map does not publish it as a session on this machine. It still lists in this machine's band as a pane named `name@host`.

## The two kinds of row

The far door, `fno mux serve --link <key>`, resolves the key on the host: session id first, then name.

| The row | What runs on the host | Effect on the host |
|---|---|---|
| Hosted in a pane | A passive client tagged to that one pane | None. It births no workspace and is not counted in the tab's size |
| A paneless thread | `fno-agents attach --inline <name>` in the ssh terminal | None. The host's portals stay as they were |

The server gate is `crates/fno/src/server/links.rs`. A tagged client can send input, mouse and scroll for its own pane and nothing else. The hello carries the pane's child pid, so a pane id reused between the read and the hello is refused. An untagged passive client stays read-only.

Keys carry no author on the wire. Both ends are the same person, so the far door does not redact the pane.

## A dropped connection

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

Both machines need a build with the link. A host without it answers exit 2, and the pane names the update command. A host server older than the link build cannot take the role, and the far door names `fno mux kill-server`. The far door checks this before it sends the hello, so an old server does not read as a network drop.

## Size

The link never resizes the host's pane. See [mux window size](mux-window-size.md#a-remote-link-does-not-resize).

## Test it by hand

A terminal that never answers the image query leaves the client's query thread blocked on stdin. That thread eats the next input and restores cooked mode, so a mouse report waits in the line buffer until a newline arrives. A real terminal answers. A pty harness must answer the query or set `FNO_MUX_IMAGE_PROTOCOL=halfblocks`, or every click reads as a dead row.
