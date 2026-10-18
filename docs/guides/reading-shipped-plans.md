# Reading Shipped Plans

This guide is for developers browsing the plans folder or Obsidian vault who
want to answer "did this ship?" without opening ledger.json or running git log.

Since the plan completion stamp landed, every plan that ships through target
stamps its own frontmatter with shipping metadata. The plan file is the
first place to look.

## How to Tell if a Plan Shipped

Open the plan's index file:

- **Folder plan:** open `{plan_dir}/00-INDEX.md`
- **Single-file quick plan:** open the `.md` file directly

Look at the YAML frontmatter at the top:

```yaml
---
status: in_review
shipped_at: 2026-04-15T09:22:10Z
urls: [https://github.com/org/repo/pull/42]
session_ids: [abc123def456]
---
```

| Field | What it tells you |
|-------|------------------|
| `status: in_review` | PR created; for cross-project plans, some repos may still be in flight |
| `status: done` | All PRs created and all expected URLs stamped |
| `shipped_at` | First ship timestamp (UTC ISO 8601). Not updated on re-stamps. |
| `urls: [...]` | Direct links to the PR(s). Multiple URLs for cross-project plans. |
| `session_ids: [...]` | Claude session IDs that contributed stamps. Useful for correlating with ledger.json. |

If the frontmatter has no `status` field (or `status: draft`/`status: ready`),
the plan has not shipped yet.

## COMPLETION.md and scratchpad-archive (older plans only)

A ship no longer writes `COMPLETION.md` or `scratchpad-archive/`. The frontmatter stamp plus git history is the whole durable record. An older plan can still carry either one inside the plan folder. Its `COMPLETION.md` holds one `## Ship N` section per ship event, and the retro harvest still reads its deferred findings. Its `scratchpad-archive/` is an unstructured snapshot of the session's working notes, useful only for forensics.

## The Old `.completed/` Pattern is Gone

Before the plan completion stamp landed, the stop hook dumped session-state files
(`HANDOFF.md`, `SUMMARY.md`, `STATE.md`, `target-state.md`) into a
`.completed/` folder next to the plan. That pattern is removed.

Session-state files are now transient. They are NOT archived. The plan
frontmatter stamp, `COMPLETION.md`, `ledger.json`, and git history are the
durable record.

If you find a `.completed/` folder next to a plan, it is from a run that
predates the plan completion stamp. You can ignore it or remove it - nothing reads it anymore.

## Querying the Plans Folder

Find all shipped plans (replace `<plans-dir>` with wherever your plan folders live - commonly a `plans/` dir in the repo or a vault path reachable via `internal/`):

```bash
grep -rlE 'status: (in_review|shipped|done)' <plans-dir>
```

Find plans that shipped to a specific repo:

```bash
grep -rl 'github.com/org/repo' <plans-dir>
```

Find plans with `status: in_review` but not yet `done` (cross-project in flight):

```bash
grep -rlE 'status: (in_review|shipped)' internal/fno/plans/**/00-INDEX.md 2>/dev/null \
  | xargs grep -L 'status: done'
```

List all PRs from stamped plans (one URL per line):

```bash
grep -rh 'urls:' internal/fno/plans/**/00-INDEX.md 2>/dev/null \
  | sed 's/urls: \[//; s/\]//' | tr ',' '\n' | sed 's/^ //'
```

These work from any shell in the repo. Adjust the base path to match your
plans location (`internal/` is the symlink to the Obsidian vault).

## Stamps Are Missing - Manual Recovery

`fno-agents finalize` stamps the plan at the end of every target run. `fno backlog done` stamps it again at node close. Both can miss, for example after a hard kill, or for a multi-ship plan whose PRs sit on other nodes. Then run the stamp by hand. `--url` repeats:

```bash
fno do plan stamp \
  --plan-path path/to/plan-folder \
  --session-id <session-id> \
  --url https://github.com/org/repo/pull/NNN \
  --expected-url-count 1
```

Use `--dry-run` first to verify the output without writing.

To promote a plan from `in_review` to `done` by hand:

```bash
fno do plan graduate \
  --plan-path path/to/plan-folder \
  --session-id <session-id>
```

`graduate` is a no-op if the URL count has not reached `expected_url_count`,
so it is safe to call even when uncertain whether all repos have shipped.

## Correlating with the Feature Graph

The feature graph at `~/.fno/graph.json` tracks plan nodes by `ab-`
prefixed ID. When a plan ships, `register-task.py` syncs the graph node to
`completed`. The stamp adds `status: done` to the plan file. Both should
agree for any plan that went through the full target pipeline.

If they disagree - for example, graph shows `completed` but the plan
frontmatter has no stamp - the plan was shipped before the plan completion stamp
landed and can be ignored or manually stamped if you want consistent metadata.

## See Also

- `docs/architecture/plan-completion-stamp.md` - how stamps work internally
  (parser, atomic writes, idempotency, invocation points)
- `docs/architecture/megawalk-pipeline.md` - how graduated plans interact
  with the feature graph and roadmap lifecycle
