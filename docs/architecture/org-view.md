# Org view

Press prefix+G to open the org view, and press it again to return to the agents list. Prefix+V still cycles Agents, Backlog and Org. The view is the fleet's command center. It shows every machine on the fleet map, each machine's projects and the live sessions in them.

## Shape

The explorer fills the left 34 columns. It lists the fleet, then each machine, then each machine's pods. A pod is one project on one machine. The open pod lists its seats, one per live session, with a status glyph: `●` writing, `◐` parked, `○` quiet and `◌` unknown. FEED and SYSTEM close the list.

The right pane follows the selection. On the fleet, a machine or a pod it shows one of five tabs over that scope:

- TABLE: one row per seat, grouped by pod, with its runtime, model, state, age and last turn.
- RECENT: the seats with a published last turn, newest first.
- OVERVIEW: the machines with their beat age, the scope's shape and state counts, then its pods or seats.
- GRAPH: one column per pod, one box per seat with its name and model. A pod over six seats wraps into a `↳` column.
- HEALTH: each machine's verdict, then three signals: no status, parked over two hours, and silent over two hours.

A seat shows its page instead: context, current activity, the node it works, its last turn and its seat facts. FEED lists the open questions to the user. SYSTEM lists each machine's state, beat, cap, memory and CPU.

Below 80 columns the explorer paints alone, and the bottom bar names `F` for full screen.

## Keys

Use `j` and `k` or the arrows to move in the explorer. The cursor opens each pod it reaches. Use `h` and `l` to change tab, or `1` to `5` to pick one. Press `u` or Backspace to go up from a seat to its pod, its machine and the fleet. PageUp and PageDown scroll the right pane. `F` toggles full screen, `?` shows the keys and Esc returns to Agents. The tab persists across client restarts.

Enter on the fleet, a machine or a pod moves to the next row, so Enter on a pod opens its first seat. Enter on a seat attaches it. Space peeks, `x` stops and `P` opens the portal picker. A tap selects a row, a tab or a graph box. A second tap on the selected seat acts like Enter.

The bottom bar shows the breadcrumb (`topology · fleet → bb16 → fno → seat`) and only the keys that act on the selection.

## Reads

The view reads `fno-agents top --machines --json` and the backlog model off the UI loop. One read runs at a time, every 60 seconds, with a 30-second bound. The fleet map comes from the shared primary, so the other machine's seats show beside this one's. With no `store.remote_url`, fleet top answers this machine alone, and the bottom bar shows its notice.

A failed read keeps the last good fleet. The bottom bar names the failure and its age. If the primary fails after a good read, the last good fleet stays too. If the first read fails, the right pane names the failure.

A seat's pod is its published project. With no published project, the pod is its node's graph project, else `unassigned`. A pane with no harness, or a shell, is not a seat. Within a pod, seats with a role list first, then writing, parked, quiet and unknown seats.

## Local and remote seats

A seat joins this mux's registry row only by its full harness session id. A short id or a name never joins. Only a joined seat takes Enter, space, `x` or `P`. A seat on another machine shows `fno agents session attach <short> on <host>` and no action keys. An unjoined seat on this machine shows the same command and says no pane here carries the session.

## Gaps show their basis

A field with no reading names its basis, never a blank. A machine on an old daemon publishes no seat facts, so its seats read `not published` for model, start, last turn and cwd. HEALTH reads `NOT SAMPLED` while no machine in scope has a fresh verdict, and never a healthy word.

The fleet map publishes no message text. RECENT, TABLE and the seat page show `not published` and the last turn's age in place of the text.

## Snapshots

`fno mux serve --snapshot --view <view>` shoots eight views: `org-table`, `org-recent`, `org-overview`, `org-graph`, `org-health`, `org-seat`, `org-feed` and `org-system`. A fleet map with no seat refuses the shot. `scripts/ops/mux-demo-snapshot.sh` seeds an invented two-machine fleet map for them.
