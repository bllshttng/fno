# `logs` goldens

Frozen bytes for `logs_parity.rs`, captured from the Python leg
(`cli/src/fno/agents/read.py`) before this PR deleted it. The port protocol's
step 2: freeze the old leg's observable behavior, delete it, and let the
frozen bytes become the contract.

## Recipe

From the repo root of a checkout whose `cli/src` still carries the Python
leg (a fresh worktree reuses the canonical checkout's venv):

```bash
PYTHONPATH="$PWD/cli/src" cli/.venv/bin/python \
    crates/fno-agents/tests/golden/logs/capture.py
```

The script builds one throwaway root per case (registry seed, the empty
harness stores, a PATH shim with the fake `claude` the case needs), runs
`FNO_AGENTS_RUNTIME=python python -m fno.cli agents logs ...` against it, and
freezes `<slug>.out`, `<slug>.err` and `<slug>.exit` after the normalization
pipeline: drop the five setup-noise prefixes, skip the events.emit
multi-line warning (a top-level `Error: ` closes the skip), rstrip, scrub the
temp root to `ROOT`.

## Survivor bytes

One golden carries the survivor's bytes, not the deleted leg's:
`unknown_name`. The registry resolver became session-id-first and its
not-found message changed shape, so the deleted leg printed
`agent not found: ghost` where the surviving resolver prints the
accepted-forms line. `capture.py` records the same override in its
`SURVIVOR_BYTES` table (it still runs the Python leg to prove the fixture,
then freezes the survivor's bytes), so re-running the recipe reproduces the
frozen files exactly.

## Case inventory

| Golden | Decision it freezes |
|---|---|
| `unknown_name` | not-found names the accepted forms (exit 13) |
| `claude_passthrough` | claude output is raw passthrough, exit mirrors claude |
| `claude_tail_3` | `--tail N` slices the last N lines client-side |
| `tail_zero` | `--tail 0` emits nothing, exit 0 |
| `negative_tail` | `--tail -5` refuses with exit 2 |
| `claude_non_zero_exit_propagates` | claude's non-zero exit propagates |
| `claude_json_warning` | `--json` on a claude row warns and passes through |
| `claude_row_with_no_short_id` | missing short id is exit 1, not name-miss 13 |
| `codex_one_shot_tail_2_by_name` | codex tee tail by name |
| `codex_one_shot_tail_2_by_session_head` | codex tee tail by session-id token |
| `codex_row_with_no_log_file` | log_path set, file absent: honest message, exit 13 |
| `codex_no_log_path_recorded` | log_path empty: `(no log_path recorded)`, exit 13 |
