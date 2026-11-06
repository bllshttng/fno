#!/usr/bin/env bash

codex_resolve_skills_root() {
  local root_dir="$1"
  local requested_root="${2:-}"
  local recorded_root_file="${3:-}"

  if [[ -n "$requested_root" ]]; then
    printf '%s\n' "$requested_root"
    return 0
  fi

  if [[ -n "$recorded_root_file" && -f "$recorded_root_file" ]]; then
    local recorded
    recorded="$(head -n 1 "$recorded_root_file" 2>/dev/null || true)"
    if [[ -n "$recorded" ]]; then
      printf '%s\n' "$recorded"
      return 0
    fi
  fi

  printf '%s/.agents/skills\n' "$root_dir"
}

codex_existing_parent_dir() {
  local path="$1"

  while [[ ! -e "$path" && "$path" != "/" ]]; do
    path="$(dirname -- "$path")"
  done

  if [[ -d "$path" ]]; then
    printf '%s\n' "$path"
  else
    dirname -- "$path"
  fi
}

codex_dir_is_writable_or_creatable() {
  local path="$1"

  if [[ -d "$path" ]]; then
    [[ -w "$path" ]]
    return
  fi

  local parent
  parent="$(codex_existing_parent_dir "$path")"
  [[ -d "$parent" && -w "$parent" ]]
}

codex_dir_supports_mutation() {
  local path="$1"

  if [[ -d "$path" ]]; then
    local probe="$path/.codex-write-probe.$$"
    if ! touch "$probe" 2>/dev/null; then
      return 1
    fi
    rm -f "$probe" 2>/dev/null || true
    return 0
  fi

  local parent
  parent="$(codex_existing_parent_dir "$path")"
  local probe_dir="$parent/.codex-mkdir-probe.$$"
  if ! mkdir "$probe_dir" 2>/dev/null; then
    return 1
  fi
  rmdir "$probe_dir" 2>/dev/null || true
  return 0
}

# The one skill-discovery engine: `fno-agents plugin-install --codex-skills`
# (crates/fno-agents/src/codex_skills.rs). The checkout's own build wins over
# PATH. A bootstrap can run before fno is installed, and an installed
# fno-agents can predate the engine. Never call an old binary to find out: it
# reads the engine's words as an install request. The engine's usage line is
# compiled into the binary, so a byte search answers with no side effect.
# Either gap reads as no engine: nothing is curated, the probe reads no
# plugin, and setup links the development aliases.
codex_skill_discovery() {
  local root="$1"; shift
  local bin="" candidate
  for candidate in "$root/crates/fno-agents/target/release/fno-agents" \
                   "$root/crates/fno-agents/target/debug/fno-agents"; do
    if [[ -x "$candidate" ]]; then bin="$candidate"; break; fi
  done
  [[ -n "$bin" ]] || bin="$(command -v fno-agents 2>/dev/null || true)"
  if [[ -n "$bin" ]] && grep -qaF -- '--codex-skills probe' "$bin"; then
    "$bin" plugin-install --codex-skills "$@"
    return
  elif [[ -n "$bin" ]]; then
    echo "Codex: the installed fno-agents predates skill discovery; run fno doctor update" >&2
  else
    echo "Codex: fno-agents not installed; skill discovery reads no plugin" >&2
  fi
  if [[ "$1" == "probe" ]]; then echo "plugin="; fi
  return 0
}
