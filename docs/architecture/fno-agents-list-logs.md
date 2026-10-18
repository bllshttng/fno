# fno agents — list + logs read surface

Two read verbs sit on top of the registry substrate: `fno agents list` (registry roster + live status) and `fno agents logs <name>` (per-agent transcript tail). The `fno-agents` Rust client serves both. `list` comes from the daemon's `agent.list` projection, and `render_list_table` in `crates/fno-agents/src/bin/client.rs` draws the table. `logs` is `run_logs` in `crates/fno-agents/src/client_verbs.rs`: claude passthrough, codex tee tail, and `logs --follow` over the daemon's WebSocket `agent.logs` transport. The MODEL and PR cells come from `crates/fno-agents/src/list_row.rs`. The Python list lane is deleted, and the Python logs leg is deleted too. On a checkout with no installed `fno-agents` binary the verb refuses with exit 127 instead of a second Python answer. Both verbs are pure-read and never mutate the registry. This doc covers the design. The how-to lives in the [list/logs user guide](../guides/fno-agents-list-logs.md).

Parent: [fno-agents-registry-and-dispatch.md](fno-agents-registry-and-dispatch.md). Sibling: [fno-agents-followup.md](fno-agents-followup.md).

## Audience: LLM orchestrator first

The primary caller is an LLM orchestrator. `list --json` returns the fleet shape, `logs --json` returns recent assistant output as JSON-Lines (codex/gemini only — Claude is raw passthrough for now). Human callers get a table for `list` and raw passthrough for `logs` when stdout is a TTY. JSON is the load-bearing contract; the human format is a convenience adapter on top.

## Module layout

```
cli/src/fno/agents/
├── cli.py                  ← Typer wiring; the `logs` verb is a refusal body
├── format.py               ← serialize_entry (row shape for the field-coverage lint)
└── harnesses/
    └── claude.py           ← claude_agents_json shellout
```

`format.py` stays a pure-read module: it never mutates the registry, never flips `status` based on inferred live state, never emits events. Status mutations belong to the dedicated write verbs (`stop`, `rm`, `reconcile`). The logs read has one owner: `run_logs` in `crates/fno-agents/src/client_verbs.rs`.

## Three axes, two views

The registry's view (`status`, mutated by the follow-up and reconcile paths) and Claude's supervisor view (`live_status`, ephemeral) are deliberately kept on separate axes:

| Axis | Source | Values | Owner |
|---|---|---|---|
| `status` | `~/.fno/agents/registry.json` | `live`, `orphaned` | fno |
| `live_status` | shell-out to `claude agents --json` | `Working`, `Needs input`, `Idle`, `Done`, `null` | claude supervisor |

Those four values are the OUTPUT vocabulary, not the wire vocabulary.
Which key and which spelling a claude build emits has changed twice, so the provider reads both `id`/`short_id` and `state`/`status` and maps every observed spelling (`working`, `busy`, `blocked`, `idle`, `done`) onto the table above.
`state` outranks `status` when a row carries both: current builds emit them in different vocabularies with disagreeing values, and `state` is the field the binary's own header count renders.
A spelling not in the input map passes through unchanged with a drift warning, which is what keeps the NEXT rename loud instead of silent.

Both fields appear on every JSON entry. `live_status` is `null` for non-Claude entries and for Claude entries when the shellout fails or omits the entry. Conflating them would lose information — an `orphaned` registry entry whose `live_status` is `null` because claude reports it doesn't exist is a different story than a `live` entry whose `live_status` is `Idle` because the supervisor is between jobs.

## Logs branch by provider

```
run_logs(rest, home)                        crates/fno-agents/src/client_verbs.rs
   │
   ├─ read_registry_entries(); resolve_entry_with_heal (session-id-first)
   │    └─ unresolved → accepted-forms message, exit 13
   ├─ harness == "claude":
   │    ├─ --json → WARN and fall back to raw passthrough
   │    ├─ short_id missing on entry → data-drift message, exit 1
   │    └─ `claude logs <short_id>` subprocess
   │         ├─ follow=False → capture, tail slice in-process
   │         └─ follow=True  → inherited stdio passthrough; SIGINT ignored in
   │                            the parent and reset to default in the child,
   │                            exit mapped clean by follow_exit_code
   ├─ codex / gemini:
   │    ├─ log_path empty or file absent → honest "no log file" message, exit 13
   │    ├─ one-shot tail block (slice last N lines)
   │    └─ if --follow → the daemon's agent.logs WebSocket stream
   └─ bad --tail / extra positional → exit 2
```

### Streaming vs capture

Claude `--follow` is an inherited-stdio passthrough. The child prints to the operator's terminal directly. The parent ignores SIGINT for the duration of the wait, so it survives to map the child's status. Ctrl-C is a clean stop (exit 0) on every provider.

### Follow-stream end frames

When a followed log file is truncated, rotated or removed, `stream_follow` in `crates/fno-agents/src/logs.rs` ends the stream with a structured end frame:

- the file disappears (`stat` returns ENOENT) — atomic-rename rotation followed by old unlink
- the inode changes — atomic-rename rotation where the new file is being written
- size shrinks below the read offset — truncate-in-place rotation

Without these checks the stream hangs silently on every common log-rotation strategy. The operator gets a real diagnostic. Re-running `logs --follow` attaches to the new file. The tests in `crates/fno-agents/src/logs.rs` prove the end frames. The one-shot bytes the Rust client prints are frozen in `crates/fno-agents/tests/golden/logs`.

## JSON output contract

```json
{
  "agents": [
    {
      "name": "worker-frontend",
      "harness": "claude",
      "short_id": "abc123",
      "cwd": "/Users/foo/code/proj",
      "created_at": "2026-05-20T17:00:00Z",
      "last_message_at": "2026-05-20T17:30:12Z",
      "status": "live",
      "live_status": "Working",
      "observed_model": { "kind": "observed", "model": "glm-5.2", "samples": 300 },
      "model": "glm-5.2",
      "model_basis": "requested",
      "pr": null,
      "pr_basis": "no-node",
      "log_path": "/Users/foo/.fno/agents/worker-frontend/output.jsonl"
    }
  ],
  "count": 1,
  "filters_applied": { "cwd": null, "provider": null, "status": null, "progress": null },
  "fields_omitted": [],
  "schema_version": 2
}
```

The schema version is owned by the Rust client and is intentionally distinct from the registry's `SCHEMA_VERSION`. The CLI output and the storage substrate evolve on independent cadences.

All entries have the same key set regardless of harness. Consumers that grep for keys can rely on `short_id == null` for non-Claude entries rather than checking for key presence.

`harness` names the CLI. `observed_model` names the model that answered, derived from each worker's transcript by `session_truth.observed_model`. Rust reads it from the existing `fno agents truth --json` probe. Row-level `model` rides with `model_basis`, so a stored request never reads as what answered. `pr` rides with `pr_basis` for the same reason: a null PR always names why it is null. `fields_omitted` stays in the envelope and is empty.

## Format selection

| `--json` | TTY | Output |
|---|---|---|
| true  | true  | JSON |
| true  | false | JSON |
| false | true  | human table |
| false | false | JSON |

The JSON-when-non-TTY default aligns with the orchestrator-first audience: a piped or subprocess-captured invocation gets parseable output without an explicit flag.

## Drift defenses

The CLI's `AgentStatusFilter` Typer enum exposes the family-1 read verdicts `live`, `orphaned`, and `unknown`.
It intentionally does not mirror `registry.KNOWN_STATUSES`: stored lifecycle metadata and rendered transcript liveness answer different questions.

The `KNOWN_LIVE_STATUSES` allowlist in `harnesses/claude.py` catches the orthogonal drift case — claude shipping a new sentinel like `"Reflecting"`. A rename it has already made, such as `"Working"` to `"working"`, is not drift. `_LIVE_STATUS_INPUT` maps every observed spelling onto the output vocabulary, and only an UNMAPPED value warns. The value still passes through, because we do not fail closed on an unknown enum from an external CLI. A forensic warning lands on stderr instead, so operators see the change rather than getting silently-stale table values.

## Known gaps

- `logs --json` for Claude entries would require parsing claude's log format; the verb currently emits a `WARN:`-prefixed gap message and continues with raw passthrough.
- codex / gemini `logs` paths require tee'd JSONL files written by those providers' create paths; where absent, the verb emits the honest "no log file at" stderr and exits 13.

## Test surface

The list row contract is `list_row_key_set_matches_shared_contract` in `crates/fno-agents/src/daemon_tests.rs`, against `schemas/agents-list-row.json`. The table tests are the `render_list_table_*` tests in `crates/fno-agents/src/client_tests.rs`, and the cell rules are tested in `crates/fno-agents/src/list_row.rs`. `logs` coverage lives in `crates/fno-agents/tests/golden/logs` (frozen one-shot bytes, with the capture recipe) and the tests module in `crates/fno-agents/src/logs.rs` (follow-stream end frames).
