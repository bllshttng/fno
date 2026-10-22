# Mux keybindings: the sideline surfaces

The three sideline-adjacent actions sit on three different keys, and their names do not always match what they do. This table is the map. The in-app `prefix+?` modal is the live authority. It renders the same table the scanner dispatches from.

| Key | Action id | Event | What opens |
|---|---|---|---|
| `prefix+f` | `find` | OpenNav | The navigator: a global goto picker over every workspace, tab, pane, agent and work-queue card. Text-filter by label, pane id, node id, title-slug or workspace. |
| `prefix+w` | `selector` | OpenSelector | The sideline row selector: cursor over the sideline's own rows. |
| `prefix+b` | `toggle-sideline` | TogglePanel | Sideline panel visibility. |
| `prefix+q` | `notifications-bell` | ToggleBell | Questions and fleet announcements in the notifications panel. |

## Inside the notifications panel

The panel opens from the bell at the far right of the sideline's top row or with `prefix+q`. While open, bare `j`/`k` move, `Enter` opens a question or clears settled questions, `c` clears settled questions, and `q`/`Esc` closes it. Drag the panel's left border to resize it. The dragged width persists across restarts. `w` toggles the wide panel, about three fifths of the terminal, and back. `F` toggles the full-screen panel and back. On the Questions tab, `space` marks the selected question and moves the cursor down. `c` archives the marked questions. With none marked, `c` clears settled questions.

## The global chord: Ctrl+Opt+Left

`Ctrl+Opt+Left` opens the sideline row selector with no prefix, from anywhere. It lands on the focused pane's row, not the top.

Why this chord: plain `Ctrl+arrow` is macOS Mission Control, so it never reaches the terminal. `Ctrl+Opt` is free at the OS level. In xterm encoding it is `ESC[1;7D`. That is the next rung on the modifier ladder the prefix arrows already parse: `1;5` Ctrl resize, `1;2` Shift move. See `esc_chord` in `crates/fno/src/keys.rs`.

The cost, accepted: the sequence is consumed by the mux and never forwarded to the pane. A program inside a pane that binds `Ctrl+Opt+arrow` loses it. `Opt+arrow` alone (word motion) is untouched. That is modifier 3, not 7.

A bare `Esc` is the first byte of the chord, so the mux holds it briefly to see what follows. If nothing follows within 40ms, the Esc forwards to the pane exactly as typed. Esc-to-cancel in vim or fzf never waits on the next keystroke. The chord itself arrives as one read, so the hold never costs a real chord.

Emulators: iTerm2, Ghostty, kitty and WezTerm emit the xterm CSI form. Verify yours before relying on it: run `cat -v`, press `Ctrl+Opt+Left`, expect `^[[1;7D`. If your emulator sends something else, bind what it sends.

## Inside the navigator

The overlay owns every keystroke, so bare arrows are free there.

- `Right` or `Enter`: goto the selected row. A thread row attaches its session, a pane row focuses the pane. A row that cannot resolve says so and keeps the overlay open.
- `Left` or `Esc`: back to the pane you came from.
- `Up`/`Down` or `Ctrl-p`/`Ctrl-n`: move the cursor.
- `Tab`/`Shift-Tab`: cycle the state chip.
- `Ctrl-o`: open the row menu on the selected row. Agent rows get the sideline's menu, with the same keys inside it (`x` remove, `p` peek, `m` mail, `s` stop, `P` portal, and the rest). Tab and workspace rows get their own menus. A plain pane row has none. `Esc` returns to the list. `Remove` and `Stop` keep the navigator open; every other action closes it. Bare letters stay filter text, so the menu is one keypress away.
- Typing: filter by what the row IS. A hit invisible in the label shows the matched token as a suffix.

## Rebinding the prefix form

`w` is left-hand and out of reach. Move the selector off it with `mux.keys` (config only, no code):

```toml
[mux.keys]
selector = ";"
```

The action id is `selector`, not `toggle-sideline`. `;` is a free single-byte chord, right pinky against the default left-hand `ctrl-b` prefix. A collision or unknown action id is refused and reported, never silently ignored.

You can also rebind from the mux. The menu's `keybindings` row opens the table: the prefix line on top, every chord grouped under its section, and an `[ edit keys in <editor> ]` button on the tail. Enter on a chord row runs it. To rebind, take the button. It opens the key config in your `$EDITOR`. If no editor is set, it opens vi. After the editor exits, the mux reloads the keys, and a rejected entry is reported with the resolver's sentence.
