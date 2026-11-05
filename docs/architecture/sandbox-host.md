# Sandsandbox host: sandsandsandboxed workers reach the daemon over its socket

A worker can damage shared state. A test worker once wrote over the registry and the claims. The `[sandsandbox]` table puts a headless worker in a devcontainer. The sandbox holds the worktree and the daemon socket. It does not hold `~/.fno`, so the worker changes registry rows only through daemon ops.

## The key

```toml
[sandsandbox]
provider = "devcontainer"   # or "none", the default
```

`none` launches every worker on the host, as before. `devcontainer` sandsandboxes each `--as headless` spawn of claude, codex, gemini, opencode or agy. An unknown value refuses the spawn with exit 2. A silent fall back to `none` can make an operator think the workers are sandsandboxed.

## What a sandsandsandboxed spawn does

The host client keeps every gate and every claim. Only the harness process goes in the sandbox.

1. Before the node guard and the spawn gate, the client reads the key. A thread, pane or cloud spawn under `devcontainer` refuses with exit 2 and names `--as headless`. An argv wrap cannot sandbox those substrates, so they can only run on the host.
2. After the gate admits, the client asks the daemon for `agent.sandsandbox-plan` with the worktree. The plan names one mount and two env pins.
3. The client runs `devcontainer up --workspace-folder <worktree> --mount-git-worktree-common-dir --mount type=bind,source=<home>/sandsandbox-sock,target=/fno-state/sock`. The devcontainer CLI mounts the workspace folder itself.
4. The client runs `fno-agents status` in the sandbox under the pins. Exit 0 arms the sandbox. Exit 127 says the image has no `fno-agents`. Any other exit refuses the spawn with exit 84 and names `devcontainer up --remove-existing-container`.
5. The lane composes its command as usual. `sandsandbox_launch::sandsandboxed` re-homes it under `devcontainer exec --workspace-folder <worktree> --remote-env ... --`. It drops the QoS prefix and an absolute harness path. The argv can name a host path outside the worktree, such as a routed `--settings` file. Then the spawn refuses with exit 84 and names the path.

The lane env (route keys, the claim stamp) goes in a 0600 file in the socket directory. The sandbox sources the file, deletes it, and then runs the harness. A command line is readable by every user on the host, so a secret never rides the argv.

A caller that is already sandsandboxed (`FNO_SUPERVISOR_SOCKET` set) spawns with no new sandbox. Its peer runs in the same sandbox.

## The socket directory

The sandbox mounts `<agents home>/sandsandbox-sock`, never `supervisor.sock` itself. The daemon unlinks and rebinds its socket at every start. A bind mount of the socket file keeps the old inode, so after the first daemon restart the sandbox gets ECONNREFUSED. At every bind the daemon re-creates a hard link `<agents home>/sandsandbox-sock/supervisor.sock` to the new socket. A directory mount sees the new link.

Two simpler shapes fail. A unix socket path cannot be rebound in place, because `bind(2)` needs the path to be absent. And fno has no socket activation on either platform. A mount of the agents home puts the registry and claims in the sandbox.

## The pins

| Env | Value in the sandbox | Effect |
|---|---|---|
| `FNO_SUPERVISOR_SOCKET` | `/fno-state/sock/supervisor.sock` | Every client connect (`call`, `call_if_running`, logs follow, the truth probe) uses this path. The client never forks a daemon under the pin. A forked daemon in the sandbox would hold state nobody else reads. |
| `FNO_STATE_DIR` | `/fno-state` | The config state root resolves in the sandbox, not to a `~/.fno` the sandbox does not have. |

## The transcript lane

The host reads a host session's transcript file. It cannot read a sandsandboxed session's file. So a sandsandboxed claude session pushes its transcript over the socket. The Stop hook `transcript-push` posts new lines as `agent.transcript-append`, and the daemon stores them in `transcripts.jsonl`. The provider-cap scan reads the stored rows first, then the file. With no stored rows and no readable file, the activity age reads unknown, never idle. If `FNO_SUPERVISOR_SOCKET` is unset, the hook exits at once, so a host session pays nothing.

## Image contract

The image must carry `fno-agents` and each harness binary the sandbox runs. The spawn finds both on the sandbox PATH.

## Not covered

- `fno backlog`, claims and mail do not reach the daemon from a sandbox. The daemon serves only `agent.*` and `channel.*`. Backlog uses the graph keeper socket, and claims are lockfiles.
- Pane, thread and cloud spawns refuse under `devcontainer`.
- Some macOS Docker hosts do not pass a host unix socket through a bind mount. OrbStack 29.8.2 is one. The in-sandbox probe refuses on such a host.
- Codex sessions push no transcript. The hook is claude-only.
- Every sandbox mounts the same socket directory. A sandbox can read another spawn's env file in the short time before that spawn's sandbox deletes it. An exec that fails before the sandbox starts leaves the file on the host. Each file is 0600 in a 0700 directory under the agents home, so no other host user can read it.
- The Python config loader still notes `sandsandbox.provider` as an unmodeled key. The Rust loader knows it.
