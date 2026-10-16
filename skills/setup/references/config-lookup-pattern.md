# Config Lookup Pattern for Skills

Every skill that needs a configurable value must use this pattern.

## For Paths

```markdown
## Step 0: Read Configuration

Resolve the plan save path with the verb, never a hand-assembled tier walk:
`fno do plan path --slug "<slug>"` (run from the node's project root).

The verb walks the one plans_path chain: `.claude/settings.local.json`
`plansDirectory` → `.claude/settings.json` → `plans_dir` in `.fno/config.toml`
→ `~/.fno/config.toml`. If unconfigured or `fno` is unavailable, ask the user
or suggest `/fno:setup`.
```

## For Optional Features

```markdown
## Step 0: Check if Feature Enabled

Read from config.toml:
- `config.docs.how_to_guides` — if absent or false, skip this step
- `config.linear.enabled` — if absent, Linear integration is disabled
```

## For Roles/Lists

```markdown
## Step 0: Read Roles

Read from config.toml → `config.docs.roles`
If absent or empty: skip role-based generation
```

## Anti-Patterns

| Bad | Good |
|-----|------|
| `Save to internal/web/plans/` | `Save to {plans.full_path}/` |
| `ls internal/shared/howto/admin/` | `ls {docs.how_to_path}/{role}/` |
| `git commit -m "feat(RR-XXX)"` | `git commit -m "feat({linear.team}-XXX)"` (if Linear enabled) |
| `roles: [admin, staff, parent]` | `roles: {config.docs.roles}` |
