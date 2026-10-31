# Org view

Press prefix+G to open the org view, and press it again to return to the agents list. Prefix+V still cycles Agents, Backlog and Org. The view is the fleet's command center. It shows every machine on the fleet map, the leads on each machine and the live sessions that report to them.

## Shape

The explorer fills the left 34 columns. It lists the fleet, then each machine, then the leads on each machine. A lead is the seat a session reports to, such as the mux lead or the fleet lead. The open lead lists its seats, one per live session, with a status glyph: `●` writing, `◐` parked, `○` quiet and `◌` unknown. Feed and System close the list.

The right pane follows the selection. On the fleet, a machine or a lead it shows one of five tabs over that scope:

- Table: one row per seat, grouped by lead, with its runtime, model, state, age and last turn.
- Recent: the seats with a published last turn, newest first.
- Overview: the machines with their beat age, the scope's shape and state counts, then its leads or seats.
- Graph: one column per lead, one box per seat with its name and model. A lead over six seats wraps into a `↳` column.
- Health: each machine's verdict, then three signals: no status, parked over two hours, and silent over two hours.

A seat shows its page instead: context, current activity, the node it works, its last turn and its seat facts. The facts include its lead and project. Feed lists the open questions to the user. System lists each machine's state, beat, cap, memory and CPU.

Below 80 columns the explorer paints alone, and the bottom bar names `F` for full screen.

## Look

The view wears the panel slots every fno view wears: body text, bold for emphasis and the dim slot for meta. It paints no accent color and no all-caps word. Headers such as Explorer, Feed, System and Current work are bold sentence case. The right pane's title names the selection: Fleet, a machine such as bb16, a lead or a seat. Tabs, column heads and entries use sentence case too.

## Keys

Use `j` and `k` or the arrows to move in the explorer. The cursor opens each lead it reaches. Use `h` and `l` to change tab, or `1` to `5` to pick one. Press `u` or Backspace to go up from a seat to its lead, its machine and the fleet. PageUp and PageDown scroll the right pane. `F` toggles full screen, `?` shows the keys and Esc returns to Agents. `W` opens the web bridge's `/command` page in the browser. With no live bridge, the notice names the `fno mux serve --web` command to start one. Over ssh, a bridge bound to loopback opens nothing, and the notice names `--bind 0.0.0.0`. The tab persists across client restarts.

Enter on the fleet, a machine or a lead moves to the next row, so Enter on a lead opens its first seat. Enter on a seat attaches it. Space peeks, `x` stops and `P` opens the portal picker. A tap selects a row, a tab or a graph box. A second tap on the selected seat acts like Enter.

The bottom bar is the key bar. It leads with the keys that act on the selection, in body text. The breadcrumb (`fleet → bb16 → rowan → scout`) and the read state follow in the dim slot.

## Reads

The view reads `fno-agents top --machines --json` and the backlog model off the UI loop. One read runs at a time, every 60 seconds, with a 30-second bound. The fleet map comes from the shared primary, so the other machine's seats show beside this one's. With no `store.remote_url`, fleet top answers this machine alone, and the bottom bar shows its notice.

A failed read keeps the last good fleet. The bottom bar names the failure and its age. If the primary fails after a good read, the last good fleet stays too. If the first read fails, the right pane names the failure.

Each fleet session carries `parent`, its spawner's session id prefix. A seat with a role is its own lead. Any other seat walks its spawner chain to the first seat with a role, and that seat is its lead. A seat sits on its own machine under that lead's name, so a lead on another machine still names the group. A chain that reaches no seat with a role, or runs past eight steps, puts the seat under `no lead`. A pane with no harness, or a shell, is not a seat. Within a lead, seats with a role list first, then writing, parked, quiet and unknown seats.

## Local and remote seats

A seat joins this mux's registry row only by its full harness session id. A short id or a name never joins. Only a joined seat takes Enter, space, `x` or `P`. A seat on another machine shows `fno agents session attach <short> on <host>` and no action keys. An unjoined seat on this machine shows the same command and says no pane here carries the session.

## Gaps show their basis

A field with no reading names its basis, never a blank. A machine on an old daemon publishes no seat facts, so its seats read `not published` for model, start, last turn and cwd. Health reads `not sampled` while no machine in scope has a fresh verdict, and never a healthy word.

The fleet map publishes no message text. Recent, Table and the seat page show `not published` and the last turn's age in place of the text.

## Snapshots

`fno mux serve --snapshot --view <view>` shoots eight views: `org-table`, `org-recent`, `org-overview`, `org-graph`, `org-health`, `org-seat`, `org-feed` and `org-system`. A fleet map with no seat refuses the shot. `scripts/ops/mux-demo-snapshot.sh` seeds an invented two-machine fleet map for them.
