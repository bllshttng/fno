# Mux key reference

Generated from `default_bindings` and `preset_rows` in `crates/fno/src/keys.rs`. Do not edit by hand: run `FNO_REGEN_KEY_DOCS=1 cargo test -p fno key_reference` to rewrite it.

Each row is one action id. Put the id in `~/.fno/keybindings.json` with a key string to rebind it. The file is one flat object, and `"preset"` is its one reserved key. The keys below come after the prefix.

| Action id | Built-in | tmux | zellij | Meaning |
|---|---|---|---|---|
| `split-right` | `%` | `%` | `r` | split right |
| `split-down` | `"` | `"` | `d` | split down |
| `split-up` | unbound | unbound | unbound | split up |
| `split-left` | unbound | unbound | unbound | split left |
| `focus-left` | `h` | `h` | `h` | focus left |
| `focus-down` | `j` | `j` | `j` | focus down |
| `focus-up` | `k` | `k` | `k` | focus up |
| `focus-right` | `l` | `l` | `l` | focus right |
| `resize-left` | `H` | `H` | `H` | resize left |
| `resize-down` | `J` | `J` | `J` | resize down |
| `resize-up` | `K` | `K` | `K` | resize up |
| `resize-right` | `L` | `L` | `L` | resize right |
| `close-pane` | `x` | `x` | `x` | close pane |
| `diff-pane` | `D` | `D` | `D` | toggle git diff pane |
| `new-tab` | `c` | `c` | `c` | new tab |
| `new-workspace` | `N` | `N` | `N` | new workspace |
| `next-tab` | `n` | `n` | `n` | next tab |
| `prev-tab` | `p` | `p` | `p` | prev tab |
| `close-tab` | `&` | `&` | `&` | close tab |
| `rename-tab` | `,` | `,` | `,` | rename tab |
| `cycle-section` | `z` | `z` | `z` | cycle active section |
| `move-tab-left` | `<` | `<` | `<` | move tab left |
| `move-tab-right` | `>` | `>` | `>` | move tab right |
| `move-tab-to` | `#` | `#` | `#` | move tab to an index |
| `prev-block` | `[` | `[` | `[` | jump prev block |
| `next-block` | `]` | `]` | `]` | jump next block |
| `select-block` | `v` | `v` | `v` | select block |
| `copy-selection` | `y` | `y` | `y` | copy selection |
| `redraw-pane` | `R` | `R` | `R` | ask the pane to repaint |
| `rerun-block` | `r` | `r` | `u` | rerun block |
| `search` | `/` | `/` | `/` | search scrollback |
| `find` | `f` | `f` | `f` | find: goto workspace/tab/pane/agent |
| `selector` | `w` | `w` | `w` | sideline row selector |
| `answers` | `a` | `a` | `a` | answer queue |
| `open-messages` | `M` | `M` | `M` | open Messages (mail) |
| `connections` | `A` | `A` | `A` | connections (accounts, combos) |
| `feed` | `e` | `e` | `e` | activity feed |
| `review-pr` | `I` | `I` | `I` | PR review beside the pane |
| `feedfocus` | `E` | `E` | `E` | focus the activity feed (arrows, enter) |
| `sideline-menu` | `P` | `P` | `P` | sideline menu (palette) |
| `org` | `C` | `C` | `C` | the org block (load, census, lanes) |
| `org-view` | `G` | `G` | `G` | HQ: org view (fleet, machines, leads, seats) |
| `notifications-bell` | `q` | `q` | `q` | open notifications for questions and announcements |
| `open-backlog-board` | `O` | `O` | `O` | open the backlog board |
| `cycle-sideline-view` | `V` | `V` | `V` | cycle sideline view (agents, backlog, org) |
| `toggle-sideline` | `b` | `b` | `b` | toggle sideline |
| `toggle-composer` | `i` | `i` | `i` | new-agent composer |
| `full-sideline` | `F` | `F` | `F` | full-screen sideline |
| `cycle-density` | `B` | `B` | `B` | cycle sideline density |
| `sort-agents` | `o` | `o` | `o` | sort table columns |
| `cycle-agent-group` | `W` | `W` | `W` | group agents by workspace, lead, cwd, status, harness, or none |
| `toggle-card-head` | `Y` | `Y` | `Y` | show or hide the card header row |
| `toggle-status` | `s` | `s` | `s` | toggle status |
| `settings` | `S` | `S` | `S` | settings (theme, toggles, prefix) |
| `sweep-threads` | `T` | `T` | `T` | sweep threads (close, reap) |
| `lift-hold` | `U` | `U` | `U` | lift the fleet hold (reserve, overload) |
| `share-focused-pane` | `X` | `X` | `X` | share the focused pane (experimental) |
| `show-pane-ids` | `\` | `\` | `\` | hold to show pane ids |
| `show-keys` | `?` | `?` | `?` | this key table |
| `grab-work` | `g` | `g` | `g` | grab work (dispatch next ready) |
| `detach` | `d` | `d` | `Q` | detach |
