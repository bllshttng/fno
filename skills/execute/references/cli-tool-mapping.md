<!-- style-exception: mechanical command rename preserves pre-existing prose -->
# Cross-CLI Tool Mapping

When this skill references Claude Code tools, use the equivalent on your platform:

## Skill Invocation

| Action | Claude Code | opencode | Codex CLI |
|--------|------------|----------|-----------|
| Load a skill | `Skill` tool with `skill: "fno:fix"` | `skill` tool with `skill({ name: "fix" })` | Open the skill's `SKILL.md` from the skills list |
| Run a slash command | `/fno:fix` | `/fno:fix` | `$fno:fix` (single-quote it on a shell line) |

## Subagent Dispatch

| Action | Claude Code | OpenClaw | Gemini CLI | Codex CLI |
|--------|------------|----------|------------|-----------|
| Spawn subagent | `Agent` tool with prompt + subagent_type | `process` tool with `action: "log"` for background, `action: "run"` for foreground | Sequential main-thread only (no project agents) | `spawn_agent` / project-scoped custom agents in `.codex/agents/` |
| Isolated worktree agent | `Agent` tool with `isolation: "worktree"` | `process` tool targeting worktree directory | Use manual git worktree | Use custom agents for same-repo work; use manual git worktree for isolation |
| Parallel dispatch | Multiple `Agent` calls with `run_in_background: true` | Multiple `process` calls with `action: "log"` | Not supported natively - execute sequentially | Limited - use `spawn_agent` |

## File Operations

| Action | Claude Code | Gemini CLI | Codex CLI |
|--------|------------|------------|-----------|
| Read file | `Read` | `read_file` | `Read` |
| Write file | `Write` | `write_file` | `Write` |
| Edit file | `Edit` | `replace` | `Edit` |
| Run command | `Bash` | `run_shell_command` | `Bash` |
| Search files | `Grep` | `search_file_content` | `Grep` |
| Find files | `Glob` | `glob` | `Glob` |

## Shared Helper CLIs

| Action | Claude Code | Gemini CLI | Codex CLI |
|--------|------------|------------|-----------|
| Backlog task management | `fno backlog ...` | `fno backlog ...` | `fno backlog ...` |

## Session Loop (Stop Hook)

| Action | Claude Code | Gemini CLI | Codex CLI |
|--------|------------|------------|-----------|
| Block exit | `{"decision":"block","reason":"..."}` | `{"decision":"deny","reason":"..."}` | `{"decision":"block","reason":"..."}` |
| Hook event | `Stop` | `AfterAgent` | `Stop` |
| Last output source | Grep transcript JSONL | `prompt_response` in hook input | `last_assistant_message` in hook input |

## Graceful Degradation

If a tool isn't available on your platform:
- **No configured custom agents**: Hooks handle multi-CLI agent adaptation automatically. Execute the wave sequentially if agent dispatch is unavailable.
- **No `Skill` tool**: Read the skill file directly with your file read tool, then follow its instructions.
- **No stop hook**: The autonomous loop won't work. Use `/think` → `/blueprint` → `/execute` as a manual 3-step workflow instead.
