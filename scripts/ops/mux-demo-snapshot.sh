#!/usr/bin/env bash
# Shoot the mux for a public page without one byte of a real session in it.
#
#   scripts/ops/mux-demo-snapshot.sh --out shot.svg [--theme light] [--format png] [--size 200x60]
#       [--window-size smallest|latest|largest] [--peer 120x40] [--no-fit]
#       [--hold] [--group <axis>]
#
# Builds a throwaway server in a temp state root: its own config, graph,
# agent registry, view prefs and socket dir. It seeds invented backlog nodes,
# leads, mail, feed questions and harness panes, then shoots it with
# `fno mux serve --snapshot --server`, kills the server and deletes the root.
# Every view a paint PR changes renders populated: the command itself refuses an
# empty view render, and the refusal dies here.
set -euo pipefail

FNO="${FNO_BIN:-fno}"
SERVER=demo
# /tmp, not TMPDIR: a socket path must stay under 104 bytes on macOS.
# pwd -P: macOS resolves /tmp to /private/tmp, and HOME must match the real path.
ROOT="$(cd "$(mktemp -d /tmp/fno-demo.XXXXXX)" && pwd -P)"
PANES=()

# The shot runs from $ROOT/code/checkout, so a relative --out would land in
# the throwaway root and die with it. Absolutize against the caller's cwd.
# --window-size and --no-fit are this script's own: the first sets the demo
# server's mux.window_size policy by env (the server inherits it), the second
# shoots as a passive observer so the shot crops instead of resizing the tab.
# --hold arms a daemon reserve on the demo's breaker record, so the status row
# shows the fleet hold. --group <axis> sets the sideline's group-by axis
# (workspace, team, cwd, status, harness).
ARGS=()
FIT=(--fit)
PREV=""
HOLD=""
GROUP=""
for a in "$@"; do
  if [ "$PREV" = "--out" ]; then
    mkdir -p -- "$(dirname -- "$a")"
    ARGS+=("$(cd "$(dirname -- "$a")" && pwd -P)/$(basename -- "$a")")
  elif [ "$PREV" = "--window-size" ]; then
    export FNO_MUX_WINDOW_SIZE="$a"
  elif [ "$PREV" = "--group" ]; then
    GROUP="$a"
  elif [ "$a" = "--window-size" ] || [ "$a" = "--group" ]; then
    :
  elif [ "$a" = "--hold" ]; then
    HOLD=1
  elif [ "$a" = "--no-fit" ]; then
    FIT=()
  else
    ARGS+=("$a")
  fi
  PREV="$a"
done
# bash 3.2 (stock macOS) reads an empty array under set -u as unbound.
set -- ${ARGS[@]+"${ARGS[@]}"}

cleanup() {
  for p in "${PANES[@]+"${PANES[@]}"}"; do
    "$FNO" mux pane kill "$p" --server "$SERVER" >/dev/null 2>&1 || true
  done
  "$FNO" mux kill-server "$SERVER" --end-unkept >/dev/null 2>&1 || true
  # kill-server keeps kept panes by design, so every keeper outlives it. A
  # keeper's --sock and its pane child's argv both sit under ROOT: end them all,
  # then prove none is left before the root goes.
  pkill -TERM -f -- "$ROOT/" 2>/dev/null || true
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -f -- "$ROOT/" >/dev/null || break
    sleep 0.5
  done
  pkill -KILL -f -- "$ROOT/" 2>/dev/null || true
  sleep 0.5
  if pgrep -f -- "$ROOT/" >/dev/null; then
    echo "mux-demo-snapshot: processes under $ROOT survive; root kept:" >&2
    pgrep -fl -- "$ROOT/" >&2
    exit 1
  fi
  if [ -n "${FNO_DEMO_KEEP:-}" ]; then echo "kept $ROOT" >&2; else rm -rf "$ROOT"; fi
}
trap cleanup EXIT

mkdir -p "$ROOT/mux" "$ROOT/agents" "$ROOT/code/checkout" "$ROOT/text" "$ROOT/.fno/claims"
# State under $ROOT/.fno: the mux board reads the graph from $HOME/.fno.
# FNO_DEMO_PY_SOURCE pins the CLI the rust bootstrap provisions, so the
# python leg matches the binary: a PyPI wheel older than the graph.db
# claims store crashes on the migration tombstone the fresh root gets.
{
  printf 'schema_version = 1\nstate_dir = "%s"\n' "$ROOT/.fno"
  # The two seeded teams lead configured projects: a live team's scope
  # compiles only against work.workspaces projects or graph epics, so the
  # payments and search leads name them here.
  printf '%s\n' \
    '[work.workspaces.checkout]' \
    '[[work.workspaces.checkout.projects]]' \
    'name = "payments"' \
    'path = "~/code/checkout"' \
    '[[work.workspaces.checkout.projects]]' \
    'name = "search"' \
    'path = "~/code/checkout"'
  # Invented accounts for the connections view. No credential exists under
  # these paths, so nothing here can log in or bill.
  printf '%s\n' \
    '[accounts]' \
    'active = "studio"' \
    '[[accounts.records]]' \
    'id = "studio"' \
    'name = "Studio"' \
    'harness = "claude"' \
    'auth = "oauth_dir"' \
    'credentials_source = "~/.claude-studio"' \
    'priority = 10' \
    '[[accounts.records]]' \
    'id = "night-shift"' \
    'name = "Night shift"' \
    'harness = "claude"' \
    'auth = "oauth_dir"' \
    'credentials_source = "~/.claude-night"' \
    'priority = 20' \
    '[[accounts.records]]' \
    'id = "codex-lab"' \
    'name = "Codex lab"' \
    'harness = "codex"' \
    'auth = "oauth_dir"' \
    'credentials_source = "~/.codex-lab"' \
    'priority = 30'
  if [ -n "${FNO_DEMO_PY_SOURCE:-}" ]; then
    printf '[dev]\nsource = "%s"\n' "$FNO_DEMO_PY_SOURCE"
  fi
} >"$ROOT/config.toml"
# The bootstrap's dev-pin reader resolves HOME/.fno/config.toml and runs
# before FNO_CONFIG is honored, so the pin has to live at the stock HOME
# path too or the python leg provisions from PyPI and mismatches the binary.
mkdir -p "$ROOT/.fno"
cp "$ROOT/config.toml" "$ROOT/.fno/config.toml"
export FNO_CONFIG="$ROOT/config.toml" FNO_MUX_DIR="$ROOT/mux" FNO_AGENTS_HOME="$ROOT/.fno/agents"
# The org gather shells `fno-agents` from PATH (org-fold), and the fresh
# root provisions the python leg's executables under .local/bin. Without
# this every fold dies with "could not run" and the org views shoot empty.
export PATH="$ROOT/.local/bin:$PATH"
# The org view reads `fno-agents census --workers --machines --json`. The
# demo root has no fleet primary, and fleet top keeps only seats with a live
# pid, so the org views would shoot empty. This shim answers that one call
# with the invented fleet map seeded below, and hands every other call to the
# real binary. The views resolve fno-agents through FNO_AGENTS_BIN first, so
# the shim takes that name and forwards to the binary it named.
REAL_AGENTS="${FNO_AGENTS_BIN:-fno-agents}"
mkdir -p "$ROOT/fleetbin"
cat >"$ROOT/fleetbin/fno-agents" <<SH
#!/bin/sh
if [ "\$*" = "census --workers --machines --json" ]; then exec cat "$ROOT/fleet-top.json"; fi
# Drop this directory wherever it sits in PATH: a process that prepends its
# own directory would otherwise send the call back here forever.
PATH="\$(printf '%s' "\$PATH" | tr ':' '\\n' | grep -vxF "$ROOT/fleetbin" | paste -sd: -)"
export PATH
exec "$REAL_AGENTS" "\$@"
SH
chmod +x "$ROOT/fleetbin/fno-agents"
export PATH="$ROOT/fleetbin:$PATH"
export FNO_AGENTS_BIN="$ROOT/fleetbin/fno-agents"
# The claims legacy lock takes a FILESYSTEM root; a fresh root carries the
# graph.db migration tombstone at .fno/claims, which is not a directory and
# crashes the lock. Give the demo's claims their own directory.
export FNO_CLAIMS_ROOT="$ROOT/claimroot"
# The lock nests .fno/claims under the claims root and needs the full path
# to exist before its tempfile lands there.
mkdir -p "$ROOT/claimroot/.fno/claims"
# HOME too, for every process: the server reads harness rosters under it,
# and a real home would list real sessions in the sideline.
export HOME="$ROOT"
unset FNO_SERVER FNO_SESSION FNO_PANE FNO_OWNER_BIRTH
# Under an owner session the server lives as long as its owner. Left alone,
# the owner is the first short-lived `pane run`, and the server shuts down
# before the shot. This script owns it instead, so it also dies with us.
export FNO_OWNER_PID=$$

# The usage cache the connections view reads first: invented readings, probed
# now, with reset times ahead.
mkdir -p "$ROOT/.fno/state" "$FNO_AGENTS_HOME"
python3 - "$ROOT" "$HOLD" "$GROUP" <<'PY'
import json, sys, time
root, hold, group = sys.argv[1:4]
now = int(time.time())
def win(label, pct, resets_in):
    return {"label": label, "used_pct": pct, "resets_at": now + resets_in}
usage = {
    "studio": {"probed_at": now, "source": "oauth-endpoint",
               "windows": [win("5h", 38.0, 7900), win("weekly", 61.0, 302400)]},
    "night-shift": {"probed_at": now - 240, "source": "oauth-endpoint",
                    "windows": [win("5h", 4.0, 15800), win("weekly", 22.0, 475200)]},
    "codex-lab": {"probed_at": now - 120, "source": "session-log",
                  "windows": [win("5h", 12.0, 9600), win("weekly", 9.0, 518400)]},
}
with open(f"{root}/.fno/state/provider-runtime-state.json", "w") as f:
    json.dump({"schema_version": 2, "usage": usage}, f)
stamp = lambda t: time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t))
if hold:
    with open(f"{root}/.fno/agents/fleet-stop.json", "w") as f:
        json.dump({"version": 1, "state": "stopped", "generation": 1,
                   "changed_at": stamp(now), "changed_by": "machine_watch",
                   "reason": "OBS is running, so the fleet keeps the machine free for stream",
                   "holds": ["spawns", "tests"], "origin": "reserve_auto",
                   "expires_at": stamp(now + 1800)}, f)
if group:
    with open(f"{root}/.fno/agents/mux-view.json", "w") as f:
        json.dump({"agent_group": group}, f)
PY

cd "$ROOT/code/checkout"
# A repo, so every attach resolves the panes' workspace instead of
# minting a second one, and the status row names a branch.
git init -q -b main
# Invented work. The board's lanes come from priority, and its scope from
# the server's project, so these stay unscoped.
IDS=()
file() { # file <priority> <title> : record the id, show the demo's home path
  local id
  id="$("$FNO" backlog idea "$2" -p "$1" --difficulty medium | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
  "$FNO" backlog update "$id" --set cwd='~/code/checkout' >/dev/null
  IDS+=("$id")
}
file p1 "Rate limit the checkout api per key"
file p1 "Retry webhooks with backoff and a dead-letter queue"
file p1 "Cache product search for 60 seconds"
file p1 "Show order status on the receipt page"
file p1 "Page the on-call when p95 latency passes 800ms"
file p2 "Add an audit log for refunds"
file p2 "Split the payments module out of the monolith"
file p2 "Remove the legacy coupon service"
file p3 "Move image resizing to a queue worker"
file p3 "Support Apple Pay on the checkout page"
file p2 "Unify the three checkout config surfaces into one documented schema with a migration guide and a deprecation window"
file p3 "Split the merchant onboarding wizard into three steps with save-and-resume and a progress indicator"

# Real teams: every node carries a scope, and the two seeded leads hold
# those scopes.
for i in "${!IDS[@]}"; do
  if [ $((i % 2)) -eq 0 ]; then s=payments; else s=search; fi
  "$FNO" backlog update "${IDS[$i]}" --set project="$s" >/dev/null
done

# The three panes work the first three nodes: a claim moves each node to In
# Progress, and a session row names the harness session its pane runs.
SIDS=(7d1e2f3a-4b5c-4d6e-8f70-81a2b3c4d5e6 0199a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b ses_3f9a1c2b7e4dA1b2C3d4E5f6g7)
HARNESSES=(claude codex opencode)
# The first backlog command migrates the store and publishes its tombstone AS
# the claims path: a marker file where the lock needs a directory. The
# marker's backup imported at publish, so retiring the marker and
# recreating the directory hands the path back to the legacy lock writer.
claims="$FNO_CLAIMS_ROOT/.fno/claims"
if [ -f "$claims" ]; then
  rm -f "$claims"
  mkdir -p "$claims"
fi
for i in 0 1 2; do
  "$FNO" agents claim acquire "node:${IDS[$i]}" --holder "target-session:${SIDS[$i]}" --ttl 2h --pid-unavailable >/dev/null
  "$FNO" backlog session add "${IDS[$i]}" --phase execute --harness "${HARNESSES[$i]}" --session-id "${SIDS[$i]}" >/dev/null
done
# The selected card carries a plan and a rank. --operator: this script is
# the operator's own tool, writing a graph that dies with the shot.
mkdir -p "$ROOT/code/checkout/plans"
printf -- '---\nstatus: ready\n---\n# Rate limit the checkout api per key\n' >"$ROOT/code/checkout/plans/rate-limit.md"
"$FNO" backlog update "${IDS[0]}" 'plan_path=~/code/checkout/plans/rate-limit.md' >/dev/null
"$FNO" backlog rank "${IDS[0]}" --top --operator >/dev/null
# A PR on the first claimed node, so its pane's row carries one and the
# navigator-pr view has a number to find.
"$FNO" backlog update "${IDS[0]}" pr_url=https://github.com/demo/checkout/pull/412 pr_number=412 >/dev/null

# Pane text: an invented transcript per harness, shown by a process that
# never prints a prompt. The cursor is hidden, so no block reads as a glyph.
E=$'\033'
# Each harness's screen: a transcript on top, its own footer at the bottom.
cat >"$ROOT/text/claude.txt" <<EOF
${E}[2m> rate limit the checkout api per key${E}[0m

${E}[97m⏺${E}[0m Read ${E}[1msrc/checkout/handler.ts${E}[0m, ${E}[1msrc/middleware/limits.ts${E}[0m

${E}[97m⏺${E}[0m The handler has no limit today. I will add a token bucket per api
  key: 60 requests a minute, and a 429 with Retry-After when it empties.

${E}[97m⏺${E}[0m ${E}[1mUpdate${E}[0m(src/middleware/limits.ts)
  ⎿  Added ${E}[32m42${E}[0m lines, removed ${E}[31m3${E}[0m lines

${E}[97m⏺${E}[0m ${E}[1mBash${E}[0m(npm test -- limits)
  ⎿  ${E}[32m14 passed${E}[0m

${E}[97m⏺${E}[0m Opened PR 118: rate limit the checkout api per key.
EOF
cat >"$ROOT/text/claude.foot" <<EOF
${E}[2m────────────────────────────────────────────────────────────${E}[0m
${E}[1m❯${E}[0m
${E}[2m────────────────────────────────────────────────────────────${E}[0m
${E}[2m  Opus 5.5 on main · ~/code/checkout${E}[0m
EOF
cat >"$ROOT/text/codex.txt" <<EOF
${E}[1m›${E}[0m retry webhooks with backoff and a dead-letter queue

${E}[2m•${E}[0m ${E}[1mRan${E}[0m ${E}[36mrg -n "deliver\(" src/webhooks${E}[0m
  ${E}[2m└${E}[0m src/webhooks/send.ts:41: await deliver(event)

${E}[2m•${E}[0m Delivery is one attempt today. A failed call is dropped.

${E}[2m•${E}[0m Plan:
  1. Retry 5 times, doubling from 2s.
  2. Move the event to a dead-letter queue after the last try.
  3. Add a replay command for the queue.

${E}[2m•${E}[0m ${E}[1mEdited${E}[0m src/webhooks/send.ts ${E}[32m(+31${E}[0m ${E}[31m-4)${E}[0m

${E}[2m•${E}[0m Working (2m 14s • ${E}[1mesc${E}[0m to interrupt)
EOF
cat >"$ROOT/text/codex.foot" <<EOF
${E}[1m›${E}[0m ${E}[2mAsk Codex to do anything${E}[0m

${E}[2m  GPT-6-Sol medium · ~/code/checkout${E}[0m
EOF
cat >"$ROOT/text/opencode.txt" <<EOF
${E}[1m┃${E}[0m cache product search for 60 seconds

  Search runs the full query on every keypress.
  I added a 60s cache keyed by the normalized query.

  ${E}[32m✓${E}[0m Edit src/search/index.ts ${E}[32m+18${E}[0m ${E}[31m-2${E}[0m

  Running npm test -- search
EOF
cat >"$ROOT/text/opencode.foot" <<EOF
${E}[1m┃${E}[0m ${E}[2mType a message${E}[0m

${E}[2m  zen · opencode · ~/code/checkout${E}[0m
EOF
cat >"$ROOT/text/pi.txt" <<EOF
${E}[1m> page the on-call when p95 passes 800ms${E}[0m

  Added an alert rule and a runbook link.
${E}[32m  3 files changed${E}[0m
EOF
cat >"$ROOT/text/docs.txt" <<EOF
${E}[1m> document the refund audit log${E}[0m

  Drafting docs/refunds.md from the new schema.
EOF
: >"$ROOT/text/pi.foot"
: >"$ROOT/text/docs.foot"

# Each pane prints its text, hides the cursor, and prints it again on every
# resize, so the sizing attach below lands on a full screen.
show() { # show <name> : the transcript, then the footer on the last rows
  local t="$ROOT/text/$1"
  printf "draw() { printf '\\\\033[?25l\\\\033[2J\\\\033[H'; cat '%s.txt'; n=\$(wc -l < '%s.foot'); r=\$(stty size | cut -d' ' -f1); printf '\\\\033[%%d;1H' \$((r - n)); cat '%s.foot'; }; trap draw WINCH; draw; while :; do sleep 1; done" "$t" "$t" "$t"
}
run() { # run <args...> : start a pane, record its id
  PANES+=("$("$FNO" mux pane run --server "$SERVER" --json "$@" | python3 -c 'import json,sys; print(json.load(sys.stdin)["pane_id"])')")
}

W=(--workspace checkout)
# The first tab stays the active one, so the three-pane tab comes first.
run --cwd "$ROOT/code/checkout" -- sh -c "$(show codex)"
run --cwd "$ROOT/code/checkout" --tab 1 --at "${PANES[0]}" --split right -- sh -c "$(show claude)"
run --cwd "$ROOT/code/checkout" --tab 1 --at "${PANES[0]}" --split down -- sh -c "$(show opencode)"
run --cwd "$ROOT/code/checkout" -- sh -c "$(show pi)"
run --cwd "$ROOT/code/checkout" -- sh -c "$(show docs)"
"$FNO" mux tab rename --server "$SERVER" "${W[@]}" --tab 1 --name agents >/dev/null
"$FNO" mux tab rename --server "$SERVER" "${W[@]}" --tab 2 --name alerts >/dev/null
"$FNO" mux tab rename --server "$SERVER" "${W[@]}" --tab 3 --name docs >/dev/null

# The registry rows that name each pane's harness, model and state. The
# fence directory at agents/registry.json marks the table as the owner: the
# rows live in the graph store's registry table, and the document envelope
# they fold into rides beside them in registry_meta. A seed that writes rows
# without the envelope reads as InvalidData, i.e. no registry at all.
python3 - "$ROOT/.fno/db/graph.db" "$SERVER" "$ROOT/code/checkout" "${SIDS[@]}" "${PANES[@]}" <<'PY'
import json, sys, datetime
db, server, cwd = sys.argv[1:4]
sids = sys.argv[4:7]
ids = [int(p) for p in sys.argv[7:]]
now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
def row(name, harness, model, pane, state, sid=None):
    return {"harness_session_id": sid} | {
        "name": name, "cwd": cwd, "harness": harness, "model": model,
        "status": "live", "liveness": "alive", "liveness_measured_at": now,
        "created_at": now, "last_message_at": now, "substrate": "pane",
        "mux": {"session": server, "pane_id": pane},
        "inside_leg": {"state": state, "seq": 1, "received_at": now},
    }
def thread(name, harness, how, attach=None):
    # idle: a live thread with no report; unmeasured: exited with no proof;
    # exited: a confirmed exit. `attach` marks an attachable bg thread: the
    # row-menu's Split Direction branch renders for exactly that shape.
    exited = how != "idle"
    return {
        "name": name, "cwd": cwd, "harness": harness, "substrate": "thread",
        "status": "exited" if exited else "live",
        "liveness": {"idle": "alive", "unmeasured": "unmeasured", "exited": "dead"}[how],
        "liveness_measured_at": now, "created_at": now, "last_message_at": now,
        "mux": None,
    } | ({} if attach is None else {"attach_id": attach})
agents = [
    row("archer", "codex", "gpt-6-sol", ids[0], "working", sids[1]),
    row("scout", "claude", "opus", ids[1], "done", sids[0]),
    row("reviewer", "opencode", "zen", ids[2], "working", sids[2]),
    # Blocked, so the strip's need-input count and the Input word render.
    row("pager", "pi", "glm-5", ids[3], "blocked"),
    row("scribe", "claude", "sonnet", ids[4], "done"),
    # Paneless threads, so the sideline shows the other states too.
    # Leads dispatch and never host a pane: attachable thread rows.
    thread("rowan", "claude", "idle", attach="demo-lead-1") | {"role_level": 1, "role_scope": "payments"},
    thread("wren", "codex", "idle", attach="demo-lead-2") | {"role_level": 1, "role_scope": "search"},
    thread("planner", "codex", "idle", attach="demo-attach-1"),
    thread("indexer", "claude", "unmeasured"),
    thread("migrator", "opencode", "exited"),
]
import sqlite3
conn = sqlite3.connect(db)
# The registry rides the graph store the commands migrated to .fno/db. Its
# tables are created lazily by the first Rust registry writer, so the seed
# matches that DDL instead of assuming they exist.
if not conn.execute(
    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='nodes')"
).fetchone()[0]:
    sys.exit(f"registry seed: no nodes table in {db}; the live store is missing")
conn.executescript(
    """
    CREATE TABLE IF NOT EXISTS registry (
        identity TEXT PRIMARY KEY,
        ordinal INTEGER NOT NULL,
        payload TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS registry_meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    """
)
records = []
for i, agent in enumerate(agents):
    sid = agent.get("harness_session_id")
    if sid:
        identity = f"session:{agent['harness']}:{sid}"
    else:
        fid = agent.get("fno_id") or f"fno-demo-{agent['name']}"
        agent["fno_id"] = fid
        identity = f"fno:{fid}"
    records.append((identity, i, json.dumps(agent)))
conn.execute("DELETE FROM registry")
conn.executemany(
    "INSERT INTO registry(identity,ordinal,payload) VALUES (?,?,?)", records
)
# The document envelope the reader folds the rows into, plus the imported
# marker the ownership probe checks: without them the read falls back to
# the legacy file and the client serves live panes only.
conn.executemany(
    "INSERT INTO registry_meta(key,value) VALUES (?,?) "
    "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    [
        ("document", json.dumps({"schema_version": 1, "agents": []})),
        ("imported", "1"),
    ],
)
row = conn.execute(
    "SELECT value FROM registry_meta WHERE key='revision'"
).fetchone()
rev = int(row[0]) + 1 if row and str(row[0]).isdigit() else 1
conn.execute(
    "INSERT INTO registry_meta(key,value) VALUES ('revision',?) "
    "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    (str(rev),),
)
conn.commit()
conn.close()
PY

# The fleet map the org view reads: this machine and a second one. Three
# local seats carry the pane sessions above, so they join their registry
# rows. Each seat's parent is its spawner's session id prefix: rowan (the mux
# lead) and wren (the fleet lead) spawned the rest, and reviewer was spawned
# by archer, so it walks up to wren. One remote seat comes from an old daemon,
# publishes no seat facts and reaches no lead.
python3 - "$ROOT/fleet-top.json" "${SIDS[@]}" "${IDS[@]}" <<'PY'
import json, sys, datetime
out = sys.argv[1]
sids = sys.argv[2:5]
ids = sys.argv[5:]
now = datetime.datetime.now(datetime.timezone.utc)
def ts(mins_ago):
    return (now - datetime.timedelta(minutes=mins_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")
MUX_LEAD = "a0d1e2f3-demo-mux-lead"
FLEET_LEAD = "b1e2f3a4-demo-fleet-lead"
def seat(name, harness, model, project, status, mins, sid=None, node=None, role=None, short=None, parent=None):
    return {
        "name": name, "session": short or (sid or name)[:8], "harness": harness, "node": node,
        "status": status, "age_s": mins * 60, "rss_mb": 410.0,
        "parent": parent[:8] if parent else None,
        "session_id": sid, "model": model, "model_basis": "registry",
        "started_at": ts(mins + 95), "last_message_at": ts(mins), "cwd": "~/code/checkout",
        "project": project, "role": role, "context_pct": 38, "parent_at": None,
    }
def machine(mid, host, local, age, sessions):
    return {
        "machine_id": mid, "host": host, "owner": "demo", "role": "worker",
        "beat_at": int(now.timestamp()) - age, "cpu": None, "mem_used": 0.62,
        "mem_pressure": 1, "watts": None, "gpu": None, "verdict": None, "cap": 16,
        "ceiling": 24, "takes_work": True, "fresh": True, "age_s": age, "local": local,
        "sessions": sessions,
    }
local = [
    seat("rowan", "claude", "opus", "payments", "quiet", 12, sid=MUX_LEAD, role="lead", short="demo-ld1"),
    seat("scout", "claude", "opus", "payments", "writing", 1, sid=sids[0], node=ids[0], parent=MUX_LEAD),
    seat("pager", "pi", "glm-5", "payments", "parked", 150, node=ids[4], short="demo-pg1", parent=MUX_LEAD),
    seat("wren", "codex", "gpt-6-sol", "search", "quiet", 30, sid=FLEET_LEAD, role="lead", short="demo-ld2"),
    seat("archer", "codex", "gpt-6-sol", "search", "writing", 2, sid=sids[1], node=ids[1], parent=FLEET_LEAD),
    seat("reviewer", "opencode", "zen", "search", "parked", 9, sid=sids[2], node=ids[2], parent=sids[1]),
]
remote = [
    seat("kestrel", "claude", "sonnet", "payments", "writing", 3, sid="4a1b2c3d-remote-0001", node=ids[6], parent=MUX_LEAD),
    seat("heron", "codex", "gpt-6-sol", "payments", "quiet", 44, sid="5b2c3d4e-remote-0002", node=ids[8], parent=FLEET_LEAD),
    seat("ibis", "opencode", "zen", "search", "writing", 5, sid="6c3d4e5f-remote-0003", node=ids[3], parent=MUX_LEAD),
    {"name": "plover", "session": "7d4e5f60", "harness": "claude", "node": None,
     "status": "unknown", "age_s": None, "rss_mb": None, "parent": None},
]
fleet = {"fresh_s": 300, "machines": [
    machine("demo-bb16", "bb16", True, 4, local),
    machine("demo-imac2d2", "imac2d2", False, 21, remote),
]}
open(out, "w").write(json.dumps(fleet))
PY

# Mail and feed events. The bell, the Messages tab and the activity feed read
# stores the board fixture never wrote, so those views shot empty. Both are
# append-only JSONL with a documented row shape, so the seed writes them
# directly. The projections resolve the stores under the config state dir:
# <state>/chats/<chat>/messages.jsonl and <state>/questions.jsonl.
python3 - "$ROOT" <<'PY'
import json, os, sys, datetime
root = sys.argv[1]
now = datetime.datetime.now(datetime.timezone.utc)

def ts(mins_ago):
    return (now - datetime.timedelta(minutes=mins_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")

# Chats store: from/to join the registry rows above, so threads resolve to
# seeded agents. The fleet: broadcast lands on the bell's Announcements tab.
chats = f"{root}/.fno/chats"

def msg(i, mins_ago, frm, to, body):
    return {"type": "message", "kind": "send", "v": 1, "id": f"fmail-demo-{i}",
            "ts": ts(mins_ago), "thread": f"chat-demo-{i}", "from": frm, "to": to,
            "body": body}

def write(chat, rows):
    os.makedirs(f"{chats}/{chat}", exist_ok=True)
    with open(f"{chats}/{chat}/messages.jsonl", "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")

write("chat-demo-review", [
    msg(1, 9, "rowan", "scout", "Rate-limit review pass today? PR 118 is up."),
    msg(2, 6, "scout", "rowan", "Landed: 60 req/min per key, the 429 carries Retry-After."),
])
write("chat-demo-scope", [
    msg(3, 21, "wren", "archer", "Webhook retry landed behind a flag; the dead-letter queue drains nightly."),
])
# Broadcasts: an announce row lands on the bell (fleet-incident) or the #fno
# channel (a lead's team row). A plain send to fleet: never does, and a pair
# row it is not, so the demo seeds real announce rows.
write("chat-demo-broadcast", [
    {"type": "message", "kind": "announce", "v": 1, "id": "fmail-demo-5",
     "ts": ts(2), "from": "fno/fleet-incident", "to": "fleet:fno",
     "meta": {"scope": "fno"},
     "body": "Test hold lifted at noon; CI covers the fleet."},
    {"type": "message", "kind": "announce", "v": 1, "id": "fmail-demo-6",
     "ts": ts(2), "from": "rowan", "to": "fleet:fno",
     "meta": {"scope": "fno"},
     "body": "Demo lanes reseeded; workers start on the top three cards after the shot."},
])

# Questions journal: the feed's questions leg (operator_question,
# operator_decision envelopes). The first question carries one screenshot,
# so the question page shows its screens row.
import struct, zlib
def png(path, w, h, rgb):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))
os.makedirs(f"{root}/screens", exist_ok=True)
shot = f"{root}/screens/q-demo-a1-1-bell.png"
png(shot, 160, 90, (40, 120, 200))
qs = [
    {"ts": ts(4), "type": "operator_question", "source": "target",
     "data": {"question_id": "q-demo-a1", "question": "Hold the 18:00 demo until the rate-limit shot lands?",
              "images": [shot]}},
    {"ts": ts(14), "type": "operator_question", "source": "target",
     "data": {"question_id": "q-demo-b2", "question": "Keep the demo root's mail store after the shot run?"}},
    {"ts": ts(26), "type": "operator_decision", "source": "target",
     "data": {"decision_id": "d-demo-c3", "decision": "Ship the seeded shots behind the paint gate"}},
]
with open(f"{root}/.fno/questions.jsonl", "w") as f:
    for q in qs:
        f.write(json.dumps(q) + "\n")
PY

sleep 6

# The server reads the registry only while a viewer is attached, and the
# first attach ends before the rows join. A warm-up shot starts that read.
"$FNO" mux serve --snapshot --server "$SERVER" --size 200x56 --fit --squad checkout --out "$ROOT/warm.svg" >/dev/null
sleep 3

# The flags after ours win. HOME makes the status row read ~/code/checkout.
"$FNO" mux serve --snapshot --server "$SERVER" --size 200x56 ${FIT[@]+"${FIT[@]}"} --squad checkout "$@"
