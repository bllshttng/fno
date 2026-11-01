# Autonomy mode

`autonomy.mode` is one choice over three action classes: unattended restarts, merges and law writes. The default is `balanced`, so a fresh install behaves as the fleet did before the key existed, with one addition: a session's law write records the user's words.

```toml
[autonomy]
mode = "ask"
allow = ["restart"]
```

`fno agents admit autonomy status` prints the resolved mode. `--json` prints the `policy` block.

## The matrix

| Mode | restart | merge | law |
|---|---|---|---|
| `ask`, class not in `allow` | hold and report | held | needs `--quote` |
| `ask`, class in `allow` | auto | `auto_merge` config decides | auto |
| `balanced` | auto | `auto_merge` config decides | needs `--quote` |
| `yolo` | auto | authorized unless config sets an explicit no | auto |

`allow` applies to `ask` only.

## What no mode changes

These gates hold in every mode, `yolo` included:

- `autonomy.enabled = false` and `recovery.enabled = false`
- CI and review
- the auto-merge posture floor, merge freezes and `--no-merge`
- an explicit `auto_merge.enabled = false` or `auto_merge.grant = "none"`
- operator-only law rows, waiver subjects and merge-grant subjects

A malformed mode, or a config file that cannot be read or parsed, reads `ask`. A broken config can never widen authority.

Commands a person runs are not autonomous actions. `boot_revival` rerun and an operator-granted merge keep their own gates.

## Law and the quote

Process ancestry cannot tell a user's `/fno:law` from an agent's own Bash call. So where the mode asks, `fno inbox law set` and `fno inbox law restore` refuse a session's write with no `--quote "<the user's words>"`, and exit 3. The quote records the ask on the row. The row stays `chat_attested`. A person at a terminal records as `operator` and passes no quote.

## The owner

`crates/fno-agents/src/autonomy_policy.rs` is the one owner of the matrix. Every door reads it: `agents_config::auto_merge_enabled` and `auto_merge_grant_dispatches` for merge, `boot_revival::revive_unattended`, `worker_wake`, `recovery_sweep` and `watchdog` for restart, and `law_match::autonomy_ask_gate` for law. `init-target-state.sh` takes its manifest snapshot from the status command, and the merge door rechecks live.
