# Workspace folders

A workspace owns the rows under its folder. The folder is the workspace's origin, stored as `origins` in `squads.json`.

## How a row finds its workspace

A worker with a pane sits in the workspace that holds the pane. A thread, a headless worker, or a daemon row has no pane. It joins a workspace by its cwd alone, through `Session::find_by_cwd` in `crates/fno/src/squad.rs`.

- The row's cwd decides. It matches an origin that equals it or sits above it.
- The longest matching origin wins. On an equal length a named workspace beats an unnamed squad, because naming a workspace is a deliberate claim. Between equals the first one wins.
- A row that matches nothing renders under `~ elsewhere`.

A workspace with no origin matches nothing. Its thread workers all render under `~ elsewhere`, and the workspace shows empty.

## Giving a workspace a folder

A new workspace takes the folder it was made from. The sideline `+` button sends no origin, so the server uses the cwd of the workspace the sender is viewing. That is also where the first shell opens.

To change the folder of an open workspace, run:

```
fno mux workspace origin add <name> <path>
fno mux workspace origin remove <name> <path>
```

The path becomes absolute. `~` expands to HOME. `.`, `..` and a trailing slash fold away. No symlink resolves, because a row matches an origin by the cwd string its harness recorded.

Every mux server that holds the name applies the edit and writes it to the store before it replies. The write changes only the stored origins, so a second server's newer members and layout survive. If the write fails, the edit reverts and the verb reports the error. A workspace can hold several origins. Add and remove are idempotent. Removing the last origin leaves the workspace with no folder.

## Seeing the state

The sideline header shows the folder after the workspace name, with HOME as `~`. A workspace with no origin shows `(no folder)`. The text takes only the room the row counts leave over, so a narrow sideline hides it.

The composer's project picker lists only workspaces that have a folder.

When a name is taken, the refusal says what holds it: `name taken: footnote is stored, no folder`. A stored row with no open workspace shows nothing in the sideline, so the old bare `name taken` gave no way to find the cause.

## Dismissing a workspace

Deleting a workspace removes its stored row, so the name is free again. The remove adopts the store's current generation first. A prune or a second server can move that generation after this server last wrote. Without the adoption the remove lost the generation check, wrote nothing, and the deleted workspace kept its name. Topology writes still lose to a newer row. Only the operator's explicit dismissal skips the check.
