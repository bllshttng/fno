# footnote session protocol, v1

This is the contract between an orchestrator and a harness. The orchestrator launches the harness with a launch spec. The harness writes a session record that the orchestrator reads back: liveness, cost, outcome and the harness's verdicts on its own claims. fno is the reference orchestrator and fnh (the `footnote` binary) is the reference harness. A harness need not be either.

The session record is the interchange format. A harness can store its session in any form, a database included. It must write or export this record. The machine-readable form is [footnote-records.schema.json](footnote-records.schema.json), beside this file. The Rust types and the reader live in `crates/fno-agents/src/footnote_transcript.rs`.

The spec has four parts: the launch spec, the session record, the lifecycle records, and the verdict types.

## 1. Launch spec, v2

The orchestrator writes one `LaunchSpec` to the harness's stdin. Its `v` is 2. The harness refuses any other `v` with exit 2 and an install command, so a stale binary never runs a newer spec.

| field | meaning |
|---|---|
| `v` | 2 |
| `mode` | `create` or `resume` |
| `fno_id` | the session id; the orchestrator mints it |
| `session_dir`, `cwd` | where the session lives and runs |
| `model`, `message`, `timeout_secs` | the run |
| `node`, `plan_path` | the backlog work, if any |
| `cost_cap_usd`, `wall_cap_minutes` | the caps |
| `plugin_root`, `parent_session_id`, `permission_mode` | the context |
| `price_cache`, `context_window` | the cost and compaction inputs |
| `endpoint` | `base_url`, `key`, `bearer`, `wire`, `provider_id`, `route` |

v2 adds `permission_mode`: `default`, `acceptEdits`, `plan`, `dontAsk` or `bypassPermissions`, from the capability row's `permission_modes`. Null runs the harness's own default. A resume sends the mode the registry row recorded at spawn. The key rides stdin, so it never reaches argv or the environment.

## 2. Session record, v1

### Files

Every session file carries the session id. Under `~/.fno/sessions/<project-slug>/` a session is a record file beside a sidecar dir. The slug is the canonical checkout's space slug, or `_none` outside a repo.

- `<fno_id>.jsonl` is the record. It is append-only, mode 0600, and each line is fsynced. Child transcripts land inside the sidecar dir as `<child_id>.jsonl`.
- `<fno_id>/` is the sidecar dir. It refuses a second create: an id names one session, never a reused one.
- `<fno_id>/<fno_id>.lock` holds an exclusive `flock` for the life of the one writer.
- `<fno_id>/<fno_id>.index.db` holds one content-free row per line (`seq`, `record_id`, `offset`, `len`, `turn`, `tool_call_uid`). Its event_store journal is `<fno_id>/<fno_id>.index.jsonl`. When an index write fails, the record stays the truth.
- `<fno_id>/<tool_call_uid>.out` holds a tool output over 8 KiB, whole.
- `<fno_id>/<fno_id>.diag.log` holds diagnostics, rotated to `<fno_id>.diag.log.1`. The API key is redacted. It is never part of the record.

### Envelope

`{"v": 1, "seq": n, "id": "<uuid>", "ts": "<RFC3339 ms>", "session_id": "<fno_id>", "type": "<type>", "data": {...}}`, plus `"ignorable": true` on a log-only record.

`seq` starts at 0 and rises by one per line. `id` is a fresh v4 UUID, never a hash. A reader refuses an unknown `type` that is not `ignorable`. A new type does not raise `v`. If resume never needs a record, the writer marks it `ignorable`. The mark tells an older reader that it can skip the line. It does not mean the type is unspecified: every ignorable type below is part of the spec.

### Types

| type | data |
|---|---|
| `header` | `session_id`, `cwd`, `harness`, `commit`, `branch`, `fno_version`, `parent_session_id`, `forked_from` (reserved, null), `node`, `plugin_root`, `model`, `wire`, `system_prompt`, `hook_sources` (`[{path, kind, status, error}]`; kind `user`, `project`, `local` or `plugin`; status `absent`, `loaded` or `broken`), `permission_mode`, `permission_mode_source` (`launch_spec`, `settings:<path>`, `built_in`), `permission_sources` (`[{path, status, allow, deny, ask}]`, rule counts per file), `agent` (child sessions only: `{name, path, tools, model_requested}`), `mcp_servers` (one row per configured MCP server, see `mcp_servers`) |
| `turn_context` | `turn`, `model`, `cwd`, `spent_usd`, `wall_secs`, `cost_cap_usd`, `wall_cap_secs`, `permission_mode` |
| `user_input` | `text`, `origin` (`operator`, `hook`, `skill`, `agent`, `footnotes`, `mail`). Origin `mail` also carries `sender_session_id`, `message_id` and `reply_to`. |
| `model_request` | `turn`, `provider_id`, `endpoint_host`, `wire`, `route`, `requested_model`, `message_count`, `estimated_input_tokens`, `body_sha256`. Never the body, key or headers. |
| `model_attempt` | ignorable. `attempt`, `status`, `error_class`, `detail`, `latency_ms`, `usage`, `cost_usd`. `cost_usd` appears only when the attempt carries `usage`. Null means the price is unknown. |
| `model_response` | `turn`, `provider_response_id`, `reported_model`, `content` (Anthropic blocks), `stop_reason`, `latency_ms`, `attempts` |
| `usage` | `turn`, `provider_response_id`, `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `cost_usd` |
| `tool_call` | `turn`, `tool_call_uid` (the harness's), `provider_tool_call_id` (the wire's), `name`, `raw_input`, `input`, `parse_error` |
| `effect_decision` | `tool_call_uid`, `effect_class`, `effect_capable`, `verdict`, `source` (`parse_error`, `hook_load`, `deny_rule`, `hook`, `agent_tools`, `allow_rule`, `mode`, `effect_class`, `default`), `rule`, `reason`, `hook_decision` (`allow`, `ask` or null), `mode`, `settings_path`, `principal` |
| `tool_start` | `tool_call_uid` |
| `tool_result` | `tool_call_uid`, `disposition` (`none`, `applied`, `unknown`), `is_error`, `model_text`, `size`, `sha256`, `spill_path`. A Task result also carries `child_session_id`, `child_transcript` and `child_cost_usd`. |
| `hook_decision` | `event`, `source` (SessionStart only: `startup`, `resume`, `compact`), `verdict`, `rule`, `reason`, `context` |
| `hook_run` | ignorable. One failed or timed-out observer run: `event`, `command`, `source`, `exit`, `duration_ms`, `error` |
| `compaction` | `summary`, `tokens_before`, `tokens_after`, `trigger`, `retained_tail`, `messages`, `plan_path`, `plan_anchor`, `plan_anchor_sha256`, `plan_anchor_missing`, `replaced_seq_range` |
| `mcp_servers` | ignorable. One at session start and one per resume when servers are configured: `servers` (`[{name, scope, path, transport, host, status, error, server_name, server_version, protocol_version, tools, skipped}]`) and `tools` (`[{name, description, input_schema}]`, named `mcp__<server>__<tool>`). A row holds no args, env, header or full URL. |
| `child_session` | ignorable. One per Task child, written before the child runs: `tool_call_uid`, `child_session_id`, `child_transcript` |
| `heartbeat` | ignorable. See part 3. |
| `terminal` | See part 3. |
| `footnotes` | ignorable. See part 4. |
| `footnotes_post` | ignorable. See part 4. |

`model_text` is exactly what the model saw, at most 30,000 bytes. If the output was longer, it also names the spill path. Storing it keeps one rule true: the next request rebuilds from the record alone, byte for byte, and its sha256 equals `body_sha256`.

### Resume

Resume reads from the last `compaction` record forward. For a `tool_call` with no `tool_result`:

- no `tool_start`: the call never ran, so it runs now, through PreToolUse again.
- `tool_start` on Read, Glob, Grep or Skill: it runs again.
- `tool_start` on anything else: it never runs again. A `tool_result` with disposition `unknown` tells the model.

## 3. Lifecycle records

These let an orchestrator read liveness and cost as facts instead of guessing from a pid or a file's mtime.

- `heartbeat` (ignorable): `state` (`working`, `waiting_permission`, `idle`), `turn`, `spent_usd`, `wall_secs`, `pid`. A harness writes one every 30 s while its process lives, idle included. It also writes one on each state change. A reader counts a session alive while its newest heartbeat is under 90 s old. A quiet record proves nothing either way: silence never proves a session dead.
- `terminal`: `state` (`done`, `budget`, `refused`, `interrupted`, `error`), `reason`, `turns`, `tool_calls`, `input_tokens`, `output_tokens`, `cost_usd`. One per run. The totals cover the whole session record, so after a resume the newest `terminal` holds the session's spend so far.

fno reads them this way. When a footnote child exits, fno writes the newest `terminal` to its ledger as one entry per session. The node's cost rolls up from there. The entry's `termination_reason` maps the terminal state. `done` reads `Done`, `budget` reads `Budget`, and `interrupted` reads `Interrupted`. `refused` and `error` read `Aborted`. A merged PR never stamps onto a `Budget`, `Interrupted` or `Aborted` entry. A fresh heartbeat is a positive liveness marker for a footnote row.

## 4. Verdict types

A harness can check each claim in its final reply against its own records. fnh does. The rules that decide a verdict belong to the harness. The spec names only the words and the fields.

- `footnotes` (ignorable): one per ended run. Its fields follow.
  - `gist`, `ending` (`done`, or the terminal state of a run that did not end done), `head_sha` (null outside git).
  - `claims`: one `{n, text, verdict, reason_code, check, footnotes}` per claim.
  - Counts: `claims_total`, `claims_accepted`, `claims_missing`, `claims_rejected`, `claims_unverifiable`, `claims_stale`, and `checks` (`{passed, failed, not_run}`).
  - Run notes: `next`, `first_draft`, `signals`, `tests_touched`, `via` (`footnotes_tool`, `text` or `loop`), `reasked`, `attester_session_id`, `reviewer`.
- A footnote is `{mark, source, verdict, reason_code, record_id, seq, kind, label}`. If the source names no record, `kind` is null. Otherwise it is `diff`, `run`, `read`, `turn`, `pr` or `tool`.
- A footnote `verdict` is `accepted`, `missing`, `rejected`, `unverifiable` or `stale`.
- If every footnote of a claim is accepted, the claim is `accepted`. Otherwise it takes the worst of `rejected`, `missing`, `unverifiable` and `stale`. A check the harness did not run reads `not_run` and stays out of `claims_total`. `reason_code` names the rule that forced a verdict, for example `self_written` or `no_observation`.
- `footnotes_post` (ignorable): one per attempt to post the verdicts to a pull request. It carries `status` (`posted`, `failed` or `skipped`) and `reason`. A post also carries `action` (`created` or `updated`), `comment_id`, `pr`, `url` and `head_sha`.

A verdict counts as evidence only at the commit it names. A `footnotes` record whose `head_sha` is not the pull request's head is stale.
