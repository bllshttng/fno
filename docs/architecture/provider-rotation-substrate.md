---
created: 2026-05-04
status: accepted
---

# Provider Rotation Substrate

## Overview

The provider rotation substrate (Spec 1 of 4) gives footnote a typed model for "which CLI + which credentials" a subprocess dispatch should use, and a pure helper that returns the env dict for that combination. It ships data + dispatch primitives only. Specs 2 through 4 layer reactive failover, per-agent routing, and proactive round-robin on top of this substrate.

Daily-driver reliability across Jason's accounts: 2x Claude Max OAuth, 2x Gemini Pro OAuth, plus Anthropic API credits via OpenClaw, Hermes, and Codex. Manual switching between accounts replaces the external account-swap step previously run between sessions today.

## Architecture

```mermaid
graph TD
    YAML["~/.fno/config.toml<br/>config.accounts.records[]"] --> LOADER["loader.py<br/>load_providers()"]
    LOADER --> PR["ProviderRecord<br/>(Pydantic v2)"]
    LOADER --> PC["ProvidersConfig<br/>records + active"]

    PC --> CLI_CMD["cli.py<br/>fno config accounts list/show/add/use/remove"]
    PR --> STAGE["staging.py<br/>stage(record)"]
    STAGE --> FS["~/.fno/providers/&lt;id&gt;/<br/>&lt;cli-dir&gt; -> credentials_source"]

    PC --> DISPATCH["dispatch.py<br/>dispatch_env(provider_id)"]
    FS --> DISPATCH
    DISPATCH --> ENV["dict[str, str]<br/>CLAUDE_CONFIG_DIR= or HOME= or env vars"]

    ENV --> SUBPROC["subprocess.run(env=...)"]

    LEDGER["cost.update(provider_id=..., account_id=...)"] --> JSON["~/.fno/ledger.json<br/>entries with provider_id + account_id"]
    REGTASK["register-task.py<br/>(reads target-state.md)"] --> LEDGER
```

The substrate has four layers, each shipped in its own atomic commit:

1. Config schema (`model.py`, `loader.py`) - typed records read from config.toml with project-local-over-global precedence.
2. CLI surface (`cli.py`, `cli/src/fno/cli.py` registration) - `fno config accounts` Typer sub-app for human-driven management.
3. Credential staging + dispatch (`staging.py`, `dispatch.py`) - per-account directory with symlink to canonical OAuth dir, plus a pure dispatch_env helper.
4. Ledger attribution (`cost.py`, `register-task.py`, `session-cost.py`) - cost entries gain `provider_id` and `account_id` fields tagged at write time.

## Locked Decisions

These are non-negotiable for Specs 2 through 4. They are recorded here so future specs do not relitigate them.

| # | Decision | Why |
|---|----------|-----|
| 1 | Substrate-first sequencing; consumers ship as separate specs | Lets the substrate stabilize and be dogfooded manually before automation depends on it |
| 2 | Hybrid auth strategy: each record declares `auth: oauth_dir` or `auth: api_key` | OAuth-on-disk and env-var-keys have different staging needs; one-size validation cannot cover both |
| 3 | footnote owns provider config; no runtime dependency on an external account-swap tool | that class of tool operates between sessions; footnote is the session, so provider state must live inside the substrate |
| 4 | Dispatch-level switching only; orchestrator-level failover delegated to existing `run-target-loop.sh` | Keeps the substrate purely about "what env do I pass to this subprocess"; failover policy is a Spec 2 concern |
| 5 | Layered routing precedence (agent > phase > default) - schema slot reserved here, consumers in Specs 3-4 | The YAML key shape is fixed now so Spec 3 does not have to migrate user configs |
| 6 | Conservative failure detection (Specs 2-4 territory; substrate just declares the data shape) | Substrate cannot know what counts as failure; only the consumer that dispatched the work does |
| 7 | Mode-aware end-of-queue (Spec 2 territory) | Same reasoning as #6 |
| 8 | OAuth symlink, not copy or fswatch | Anthropic's OAuth refresh writes back to the canonical dir; the symlink resolves through, so isolation comes from per-id separation, not from filesystem-level copies |
| 9 | HOME override for gemini/codex/openclaw/hermes | These CLIs lack a clean `*_CONFIG_DIR` env var; setting `HOME=<root>/<id>/home/` is uniform across them |
| 10 | `${KEYCHAIN:...}` resolution via the macOS `security` CLI | No third-party keychain libraries; Linux/Windows support deferred |

## Module Boundaries

The substrate is Rust: `crates/fno-agents/src/accounts/`. The Python package that first held it (`cli/src/fno/adapters/providers/`) is deleted, and its goldens are the contract now.

| Module | Responsibility | Was |
|--------|---------------|-----|
| `records.rs` | Record validation, the layered read, combos, the quota block | `model.py`, `loader.py` |
| `mod.rs` and one module per command | `fno config accounts` (every spelling routes here through `crates/fno/src/accounts_route.rs`) | `cli.py` |
| `staging.rs` | The staged dir layout | `staging.py` |
| `env.rs` | The launch env and env references | `dispatch.py` |
| `error_class.rs` | The error taxonomy; `spawn-overlay accounts error-class` serves it to Python | `error_taxonomy.py` |

`cli/src/fno/cost.py` gains two optional keyword-only parameters (`provider_id`, `account_id`); the entry only includes the keys when non-None. `scripts/metrics/register-task.py` reads them from `target-state.md` frontmatter and propagates. `scripts/metrics/session-cost.py` aggregates by `entry.get("provider_id", "unattributed")` so old-format ledger entries land in the unattributed bucket.

## Concurrency Guarantees

`dispatch_env` is a pure function: no caching, no global state, no I/O beyond reading config.toml. AC03.3 verifies this with a `concurrent.futures.ThreadPoolExecutor` running 100 calls per provider across 8 workers, asserting that no result for provider A ever leaks values from provider B.

`cost.update` uses the existing `filelock` pattern; AC04.6 covers the concurrent-write case. The substrate inherits the atomic-write guarantee already validated in the broader CLI test suite.

## What This Substrate Does NOT Do

This is the explicit list of capabilities deferred to later specs. Reading this section before reaching for the substrate prevents accidental scope expansion.

- Reactive failover (Spec 2) - if a dispatch fails with rate-limit/quota, the substrate has no opinion about retrying with another provider.
- Per-agent routing (Spec 3) - sigma-review subagents cannot yet ask the substrate "give me a non-Claude provider for this review".
- Per-phase pinning (Spec 4) - the YAML key shape is reserved but the lookup logic is not implemented.
- Proactive round-robin (Spec 4) - no rotation cursor, no usage tracking.
- Linux/Windows keychain support - `${KEYCHAIN:...}` only resolves on macOS via the `security` CLI.
- Provider availability detection - a provider that has been quota-exhausted looks identical to a healthy one until dispatch.

## Failure Modes Inventory

The plan's design doc enumerates failure modes per category. The substrate addresses these:

| Failure mode | Where addressed | Phase |
|--------------|-----------------|-------|
| Empty provider list at config load | Returns ProvidersConfig(records=[], active=None) without error; rejected only at dispatch time | 01 |
| Auth declaration mismatched with credentials | model_validator raises with literal phrase `auth_strategy_mismatch` | 01 |
| Active record id not in records list | loader raises ProviderConfigError with `active_record_not_found` | 01 |
| Duplicate record ids in YAML | ProvidersConfig model_validator rejects with `duplicate_record_ids` | 01 (added during sigma-review) |
| Missing credentials_source path | stage() raises ProviderStagingError with the path | 03 |
| Existing symlink with wrong target | stage() raises (idempotent only on matching target) | 03 |
| Unstaged provider at dispatch | dispatch_env raises ProviderUnavailableError, distinct from ProviderNotFoundError | 03 |
| Unresolvable env reference | resolve_env_value raises ProviderUnavailableError naming the reference | 03 |
| Concurrent dispatch contamination | dispatch_env is pure, ThreadPoolExecutor-tested | 03 |
| Concurrent ledger write | cost.update uses filelock, atomic temp-file write | 04 |
| Save against corrupt config.toml | save_providers reads strict, raises ProviderConfigError instead of silently overwriting unrelated keys | 01 (added during sigma-review) |
| Tilde in credentials_source not expanded | field_validator on ProviderRecord.credentials_source calls .expanduser() | 01 (added during sigma-review) |

## Tests

Every accounts contract is a Rust characterization over goldens captured from the deleted Python legs. The `*_golden_driver.py` beside each test is its capture record.

| Test | Scope | Owner |
|------|-------|-------|
| `crates/fno-agents/tests/accounts_records_parity.rs` | Record validation, refusal headlines, layered read, `$FNO_CONFIG`, combos, quota, effective active | `records.rs` |
| `crates/fno-agents/tests/accounts_show_parity.rs` | `show` text and `--print-binding` | `show.rs` |
| `crates/fno-agents/tests/accounts_env_parity.rs` | Launch env per auth arm, env references, staging check, identity refusal, the `--account` overlay lanes | `env.rs`, `staging.rs` |
| `crates/fno-agents/tests/accounts_launch_parity.rs` | The credentials compose and its decision, the state-root seal, the `--dispatch-account` answer, the dead worker's account | `credentials.rs`, `launch.rs` |
| `crates/fno-agents/tests/accounts_error_class_parity.rs` | Error classes, cooldown rules, reset stamps, the recorded 429 corpus | `error_class.rs` |
| `cli/src/fno/test_cost.py` | tagged write, untagged write, mixed-schema ledger, concurrent atomic write | cost.py |
| `tests/test_register_task_provider_attribution.sh` | register-task.py reads target-state.md, propagates fields | register-task.py |

## Migration Path for Spec 2

When reactive failover lands, the integration point is `dispatch_env` plus a new function `next_provider(after_failed: ProviderRecord, queue: list[ProviderRecord]) -> ProviderRecord | None` that the orchestrator (e.g., `loop.py`) calls between phase dispatches. The substrate's `priority: int` field on each record is the failover-queue ordering hint reserved for that purpose.

The substrate already commits the data shape Spec 2 will need:
- `priority` for ordering
- `tags: list[str]` for cost-class filtering (Spec 4)
- Distinct `ProviderUnavailableError` for "skip and try next" vs `ProviderNotFoundError` for "stop, programming error"

Spec 2 should NOT extend ProviderRecord with new fields; if more state is needed, attach it to a separate "rotation cursor" file under `~/.fno/providers-state.json` so the substrate stays reusable.

## See Also

- User-facing reference: `docs/provider-rotation.md`
- CLAUDE.md "Provider Rotation Substrate" section for the contributor-facing surface summary
