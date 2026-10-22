# Mux window size

Several windows can attach to one mux. One pty has one size, so every window that views the same tab shares one content area. The key `mux.window_size` picks which window decides it.

| Value | The tab takes |
|---|---|
| `latest` (default) | the size of the window that last attached, sent input, or resized |
| `smallest` | the smallest rows and the smallest columns over all viewers |
| `largest` | the largest rows and the largest columns over all viewers |

## Set it

Add `window_size` under `[mux]` in `config.toml`. The client reads it when it starts a server and passes it as `FNO_MUX_WINDOW_SIZE`. The server never reads config. A change needs `fno mux kill-server`. An unknown value reads `latest` and logs one line.

```toml
[mux]
window_size = "smallest"
```

The key has no Python registry row yet, so `fno config set mux.window_size` refuses it. Edit the file.

## What counts as activity

Under `latest`, a window becomes the size owner when it attaches, when it sends a resize with new dimensions, or when it sends input, a command, a mouse event, a search, a block action, an answer, a dispatch or an agent launch. Read-only observers never count. The peek refresh timer sends a command with no person behind it, so it never counts. A control connection (`fno mux pane send`, which the mail pane lane uses) is not a window, so a mail delivery cannot move the size.

## A smaller window crops

A window smaller than the tab's area shows the top-left part of the tab. Panes clip at the edge of that window's content area, nothing pans or scrolls, and no letterbox filler paints. The cursor stays at the visible edge. The window gets its full fit back on its next touch, because the touch makes it the latest window.

A larger window under `smallest` letterboxes: the cells beyond the tab's area show dim dots.

Switching between two windows re-tiles the tab and signals every pane on each switch. There is no debounce, as in tmux.
