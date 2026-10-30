#!/usr/bin/env bash
# Cross-invocation TTL cache for the worktree sweep's machine-wide cwd
# snapshot. The bash twin of live_cwds_cached in
# crates/fno-agents/src/cargo_build_dirs.rs: that one serves one
# `lsof -a -d cwd` read per process for CWD_CACHE_TTL (30s); this one serves
# one read per MACHINE for the same 30 seconds, because each sweep
# invocation is a new process and the payload crosses the boundary as a
# file, not a mutex.
#
# The cached bytes are the raw `lsof -a -d cwd -Fpn` stdout, byte for byte,
# so every consumer feeds the cache through the same parser it already runs
# on a live read and no second parse shape exists to keep honest.
#
# Safety bound (the Rust side's argument, unchanged): a process that enters
# a tree inside the TTL window reads as absent for at most 30 seconds - far
# inside the 30-minute reap grace and the claim, lock, liveness and
# occupancy gates that hold each tree on their own.
#
# Best-effort by contract: a failed load falls through to the live read, a
# failed store skips the cache, and neither failure may cost the caller
# anything beyond the read it was already going to make. Store is called
# only after a successful read, so an unreadable lsof never poisons the
# cache. FNO_CWD_CACHE_DIR overrides the directory so tests pin their own.

# How long one machine-wide read is served. Keep in step with the Rust
# CWD_CACHE_TTL: 30s inside the 30m reap grace.
WT_CWD_CACHE_TTL=30

wt_cwd_cache_file() {
    printf '%s\n' "${FNO_CWD_CACHE_DIR:-$HOME/.fno/cache}/cwd-snapshot.lsof"
}

# Print the cached payload on stdout, rc 0, when a fresh cache file exists;
# rc 1 when missing, stale (including a future mtime: clock skew), or
# unreadable. The caller falls through to the live read on rc 1.
wt_cwd_cache_load() {
    local file now mtime age
    file="$(wt_cwd_cache_file)"
    [[ -f "$file" ]] || return 1
    now=$(date +%s)
    # GNU stat first: on BSD `stat -c` fails, and on GNU `stat -f` means
    # FILESYSTEM status, which SUCCEEDS and prints a "File:" block (see
    # hook-budget.sh for the same trap).
    mtime=$(stat -c %Y "$file" 2>/dev/null || stat -f %m "$file" 2>/dev/null || printf 0)
    age=$((now - mtime))
    ((age >= 0 && age < WT_CWD_CACHE_TTL)) || return 1
    cat "$file" || return 1
}

# Read the raw payload on stdin and land it on the cache file via tmp-plus-
# mv in the cache's own directory, so a concurrent loader never sees a
# partial file. Any failure returns rc 1 and stores nothing.
wt_cwd_cache_store() {
    local file dir tmp
    file="$(wt_cwd_cache_file)"
    dir="${file%/*}"
    mkdir -p "$dir" 2>/dev/null || return 1
    tmp="$dir/.cwd-snapshot.tmp.$$"
    if ! cat > "$tmp"; then
        rm -f "$tmp" 2>/dev/null
        return 1
    fi
    if ! mv -f "$tmp" "$file" 2>/dev/null; then
        rm -f "$tmp" 2>/dev/null
        return 1
    fi
}
