#!/usr/bin/env bash
# fno hook: PreToolUse - shared Bash guard dispatcher
# PreToolUse: run the ordered Bash guard chain with one hook command.
# Guard policy stays in the Rust hook module and existing Python guards.
#
# Hot path: this fires on every Bash tool call, so the happy path starts
# three processes (bash, the root subshell, the fno-agents binary). stdin
# rides the read builtin, PATH resolution never forks, and the binary's
# stderr inherits ours instead of an mktemp/cat/rm round trip. A failing
# candidate's diagnostics now surface on the fallback path instead of being
# swallowed; the allow verdict and the relayed stdout contract are unchanged.
stdin=
IFS= read -r -d '' stdin || true
root="$(cd -- "${0%/*}/.." && pwd)"
bins=()
[[ -n "${FNO_AGENTS_BIN:-}" ]] && bins+=("$FNO_AGENTS_BIN")
[[ -n "${FNO_AGENTS_FRONT:-}" ]] && bins+=("$FNO_AGENTS_FRONT")
IFS=: read -ra path_dirs <<<"$PATH"
for dir in "${path_dirs[@]}"; do
    # An empty PATH segment means the cwd, as command -v read it.
    dir="${dir:-.}"
    [[ -x "$dir/fno-agents" ]] && { bins+=("$dir/fno-agents"); break; }
done
bins+=("$PWD"/crates/fno-agents/target/release/fno-agents \
    "$PWD"/crates/fno-agents/target/debug/fno-agents)
for bin in "${bins[@]}"; do
    [[ -x "$bin" ]] || continue
    if out="$(FNO_REPO_ROOT="$root" "$bin" hook pretooluse-bash <<<"$stdin")"; then
        printf '%s\n' "$out"
        exit 0
    fi
done
echo "pretooluse-bash: no fno-agents could answer the hook entry; allowing" >&2
printf '%s\n' '{}'
