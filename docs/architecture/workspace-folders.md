# Workspace folders

A workspace owns the rows under its folder. The folder is the workspace's origin, stored as `origins` in `squads.json`.

## How a row finds its workspace

A worker spawned into a tab sits in the workspace that holds its pane. Every other row asks one resolver, `thread_workspace` in `crates/fno/src/server/thread_workspace.rs`. That covers a thread, a headless worker, a daemon row, and a row that a portal seat or an Open Here viewer only shows.

The resolver tries six rungs in order. The first rung that answers wins.

1. A pin. The operator moved this session into a workspace by hand (see Moving a session).
2. The recorded member. The workspace stored the row as a member.
3. The spawner. The row's parent session lives in a workspace, and the child joins it. A parent that a portal seat or an Open Here viewer shows does not count. It answers through its own chain.
4. The raw cwd. The row's cwd equals an origin or sits under one. The longest origin wins. On an equal length a named workspace beats an unnamed squad, because naming a workspace is a deliberate claim. Between equals the first one wins.
5. The project root. The mux server resolves the canonical repo root of the cwd once per cwd, through the git common dir. A linked worktree outside the origin lands in the workspace that owns its main checkout.
6. The project name. Exactly one named workspace has a name equal to the `[project] id` in `<root>/.fno/config.toml`. With no id, the name equals the last segment of the root. Two or more matches answer nothing, so the resolver never guesses.

A row that no rung answers renders under `~ elsewhere`. A workspace with no origin still claims rows through rungs 1, 2 and 6.

No rung reads focus, a portal seat, or an Open Here viewer. A tap on a row never changes its workspace. Opening a row in a portal from another workspace, and retuning that portal later, leave the row where it was.

## Giving a workspace a folder

A new workspace takes the folder you pick. The `+` overlay lists the project roots of the live rows. While you type, the folder follows the name: a folder whose last segment equals the name is chosen. The Up and Down keys step through the folders and the default, and a pick stops the name from changing it. The footer shows the folder that Enter sends. The default is the cwd of the workspace you are viewing, which is also where the first shell opens.

A workspace made by recruiting rows takes the one project root those rows share. Rows from mixed roots make a workspace with no folder.

The section menu of a named workspace has Add folder. It opens a numbered list of the same live-row folders, and the digit adds one folder to the workspace. It is the same edit as `fno mux workspace origin add`. An older mux server shows `update the mux server to move a session` and receives nothing.

To change the folder of an open workspace, run:

```
fno mux workspace origin add <name> <path>
fno mux workspace origin remove <name> <path>
```

The path becomes absolute. `~` expands to HOME. `.`, `..` and a trailing slash fold away. No symlink resolves, because a row matches an origin by the cwd string its harness recorded.

Every mux server that holds the name applies the edit and writes it to the store before it replies. The write changes only the stored origins, so a second server's newer members and layout survive. If the write fails, the edit reverts and the verb reports the error. A workspace can hold several origins. Add and remove are idempotent. Removing the last origin leaves the workspace with no folder.

## Moving a session

The row menu has Move to workspace for every row when another workspace exists. A row with its own pane moves that pane. A paneless row, or a row that a portal seat shows, moves by pin. The server stores the pin on the destination's row in `squads.json`, keyed by harness and harness session id. It never keys a pin on the row name.

The store write comes first. If it fails, the live pins do not change and you see `workspace move was not stored: <error>`. A topology write keeps the pins of the row it rewrites. A row without a harness session id says `this row has no session id to pin`.

A pin whose workspace is not open reads as absent, and the row falls to the next rung. Pins stay in the store until their workspace is removed. An older `fno` build that rewrites `squads.json` drops them.

## Seeing the state

The sideline header shows the folder after the workspace name, with HOME as `~`. A workspace with no origin shows `(no folder)`. The text takes only the room the row counts leave over, so a narrow sideline hides it.

The composer's project picker lists only workspaces that have a folder.

When a name is taken, the refusal says what holds it: `name taken: footnote is stored, no folder`. A stored row with no open workspace shows nothing in the sideline, so the old bare `name taken` gave no way to find the cause.

## Dismissing a workspace

Deleting a workspace removes its stored row, so the name is free again. The remove adopts the store's current generation first. A prune or a second server can move that generation after this server last wrote. Without the adoption the remove lost the generation check, wrote nothing, and the deleted workspace kept its name. Topology writes still lose to a newer row. Only the operator's explicit dismissal skips the check.
