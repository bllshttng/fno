# Mux keybindings: the sideline surfaces

The three sideline-adjacent actions sit on three different keys, and their names do not always match what they do. This table is the map. The in-app `prefix+?` modal is the live authority. It renders the same table the scanner dispatches from.

| Key | Action id | Event | What opens |
|---|---|---|---|
| `prefix+f` | `find` | OpenNav | The navigator: a global goto picker over every workspace, tab, pane, agent and work-queue card. Text-filter by label, pane id, node id, title-slug or workspace. |
| `prefix+w` | `selector` | OpenSelector | The sideline row selector: cursor over the sideline's own rows. |
| `prefix+b` | `toggle-sideline` | TogglePanel | Sideline panel visibility. |
| `prefix+q` | `notifications-bell` | ToggleBell | Questions and fleet announcements in the notifications panel. |
| `prefix+I` | `review-pr` | OpenReview | The PR review as a right split beside the focused chat pane. The chat stays live. |

## Inside the notifications panel

The panel opens from the bell at the far right of the sideline's top row or with `prefix+q`. While open, bare `j`/`k` move, `Enter` opens a question or clears settled questions, `c` clears settled questions, and `q`/`Esc` closes it. Drag the panel's left border to resize it. The dragged width persists across restarts. `w` toggles the wide panel, about three fifths of the terminal, and back. `F` toggles the full-screen panel and back. On the Questions tab, `space` marks the selected question and moves the cursor down. `c` archives the marked questions. With none marked, `c` clears settled questions.

## Resizing splits

Every split has a drag edge and a key. Press the edge and drag. The size holds for the session, and a bare `Esc` mid-drag puts the edge back. The key is the resize chord: the prefix, then `Ctrl+arrow` (or `Ctrl+Opt+arrow`). When a surface below is on screen, the chord moves that surface's edge. Otherwise it resizes the tiled panes.

| Surface | Drag this edge | Key |
|---|---|---|
| Tiled panes | The seam between two panes | Chord, any arrow |
| Sideline | Its right border | Density button |
| Notifications panel | Its left border | `w` toggles wide |
| File viewer panel | Its left border | `-` and `+` |
| Backlog board and detail | The rule between them (needs 100 columns) | Chord, left and right |
| Messages columns | The first cell of the column to the right, for tree and participants | Chord, left and right; the thread column moves its left edge |
| Org explorer and right pane | The rule between them (needs 80 columns) | Chord, left and right |
| Reply dock | Its top border | Chord, up and down |

Each pane keeps a minimum width, so a drag stops short of hiding a pane. A terminal that shrinks re-clamps the held size.

## The global chord: Ctrl+Opt+Left

`Ctrl+Opt+Left` opens the sideline row selector with no prefix, from anywhere. It lands on the focused pane's row, not the top.

Why this chord: plain `Ctrl+arrow` is macOS Mission Control, so it never reaches the terminal. `Ctrl+Opt` is free at the OS level. In xterm encoding it is `ESC[1;7D`. That is the next rung on the modifier ladder the prefix arrows already parse: `1;5` Ctrl resize, `1;2` Shift move. See `esc_chord` in `crates/fno/src/keys.rs`.

The cost, accepted: the sequence is consumed by the mux and never forwarded to the pane. A program inside a pane that binds `Ctrl+Opt+arrow` loses it. `Opt+arrow` alone (word motion) is untouched. That is modifier 3, not 7.

A bare `Esc` is the first byte of the chord, so the mux holds it briefly to see what follows. If nothing follows within 40ms, the Esc forwards to the pane exactly as typed. Esc-to-cancel in vim or fzf never waits on the next keystroke. The chord itself arrives as one read, so the hold never costs a real chord.

Emulators: iTerm2, Ghostty, kitty and WezTerm emit the xterm CSI form. Verify yours before relying on it: run `cat -v`, press `Ctrl+Opt+Left`, expect `^[[1;7D`. If your emulator sends something else, bind what it sends.

## Inside the PR review

`d` on a backlog node's detail opens the diff of the node's newest unmerged PR, full screen. If a seat's node has a PR, `r` on its org page opens the same review. `Esc` goes back to that page.

`prefix+I` on a focused chat pane opens the same review as a right split. It shows that session's node and PR, or `no PR yet`. `prefix+I` again closes it. Inside the split, `I` closes it too. A click on the chat pane gives the keyboard back, and a click on the split takes it again. Drag the split's left edge to resize it, or press `-` and `+` inside it. The width stays between 20 and 80 percent and holds for the session. The file viewer panel drags the same way.

- `j`/`k` or arrows: move one row. `PageUp`/`PageDown`: move one screen. `n`/`p`: next or previous file.
- `c`: comment on the row, or edit its comment. `Enter` stages the comment, and `Esc` drops the input. `x`: remove the row's comment.
- `S`: send every staged comment as one blocking finding on the node. A success clears the drafts, and the notice is the engine's receipt.
- `Esc` (or `I` in the split): close. With unsent comments, the first `Esc` warns and the second closes and drops them.

## Inside the navigator

The overlay owns every keystroke, so bare arrows are free there.

- `Right` or `Enter`: goto the selected row. A thread row attaches its session, a pane row focuses the pane. A row that cannot resolve says so and keeps the overlay open.
- `Left` or `Esc`: back to the pane you came from.
- `Up`/`Down` or `Ctrl-p`/`Ctrl-n`: move the cursor.
- `Tab`/`Shift-Tab`: cycle the state chip.
- `Ctrl-o`: open the row menu on the selected row. An agent row gets the sideline's menu and keys (`x` remove, `p` peek, `m` mail, `s` stop). Tab and workspace rows get their own menus. A plain pane row has none. `Esc` returns to the list. Running an action closes the navigator. Bare letters stay filter text, so the menu is one keypress away.
- Typing: filter by what the row IS. A hit invisible in the label shows the matched token as a suffix.

## Rebinding the prefix form

`w` is left-hand and out of reach. Move the selector off it with `mux.keys` (config only, no code):

```toml
[mux.keys]
selector = ";"
```

The action id is `selector`, not `toggle-sideline`. `;` is a free single-byte chord, right pinky against the default left-hand `ctrl-b` prefix. A collision or unknown action id is refused and reported, never silently ignored.

You can also rebind from the mux. The menu's `keybindings` row opens the table: the prefix line on top, every chord grouped under its section, and an `[ edit keys in <editor> ]` button on the tail. Enter on a chord row runs it. To rebind, take the button. It opens the key config in your `$EDITOR`. If no editor is set, it opens vi. After the editor exits, the mux reloads the keys, and a rejected entry is reported with the resolver's sentence.

## Key presets: tmux and Zellij

`mux.preset` names a set of `mux.keys` rows. Unset means `tmux`, the shipped table. Rows you write in `[mux.keys]` always win over the preset. The setup wizard asks for it under `/fno:setup advanced`.

```toml
[mux]
preset = "zellij"
```

An unknown name keeps the tmux table and reports it.

### Where the shipped table matches tmux

This audit compares the table with `tmux list-keys -T prefix` from tmux 3.8, run on a default config. The prefix is `ctrl+b` in both.

| tmux key | tmux action | Here |
|---|---|---|
| `%` and `"` | split right, split down | Same |
| `x` | kill pane | Same |
| `c`, `n`, `p`, `&`, `,` | new, next, previous, close, rename window | Same |
| `1` to `9` | select window | Same. `0` is not bound |
| `d` | detach | Same |
| `w`, `f`, `?` | window tree, find, list keys | Close: `w` opens the sideline selector, `f` the navigator, `?` the key table |
| `Up`, `Down`, `Left`, `Right` | select pane | Same, plus `h j k l` |
| `Ctrl+arrow` | resize | Same, plus `H J K L` |
| `z` | zoom pane | Differs: `z` cycles the active section. Zoom does not exist yet |
| `o`, `;` | next pane, last pane | Differs: `o` sorts agents. No next-pane or last-pane command |
| `l` | last window | Differs: `l` focuses right. No last-window command |
| `{` and `}` | swap pane | Missing: no swap command |
| `!` | break pane | Missing as a chord |
| `s` | session tree | Differs: `s` toggles the status row |
| `[` and `]` | copy mode, paste | Differs: they jump between command blocks |
| `r` | refresh client | Differs: `r` reruns a block |

The keys marked "Differs" belong to fno actions that have no tmux twin, so the default stays. To get the tmux meaning on a key, rebind it in `[mux.keys]`. The commands that do not exist yet (zoom, next pane, last pane, last window, swap pane) need new server commands. They are tracked as their own work.

### The zellij preset

Zellij is modal. `Ctrl+p` enters pane mode, `Ctrl+t` tab mode, `Ctrl+o` session mode, and each mode has its own letters. This mux has one prefix and one flat key space. The two designs differ, so the preset does not give Zellij parity. It maps what the table can hold. These Zellij keys come from its default `default.kdl`:

| Zellij | Key | Preset row |
|---|---|---|
| pane mode: new pane right | `r` | `split-right = "r"` |
| pane mode: new pane down | `d` | `split-down = "d"` |
| pane mode: close pane | `x` | Same by default |
| pane mode: focus | `h j k l` | Same by default |
| session mode: detach | `d` | `detach = "Q"`: `d` now splits, so detach moves |

The preset also moves `rerun-block` to `u`, because `r` is now a split.

What does not map:

- Tab mode letters (`n` new tab, `x` close tab, `r` rename, `h`/`l` previous and next) reuse the pane mode letters. Tabs keep the tmux keys `c`, `&`, `,`, `p`, `n`.
- Resize and move modes. Resize stays on `H J K L` and `Ctrl+arrow`.
- Fullscreen (`f`), floating panes, stacked panes and pane frames. This mux has no equivalent chord.
- Modes themselves. There is no locked mode and no mode line.
