#!/usr/bin/env bash
# Two-machine review-attestation proof over a throwaway primary.
#
# The bug: an attestation lived only in the reviewing machine's journals,
# so a merge from a peer machine read review_coverage_uncovered and paid a
# second review round; three PRs hit it on 2026-10-13. The fix mirrors
# every committed review_attestation to the shared primary at commit and
# adds a primary source to every gate read, with the same mirror dedup the
# global log already applies.
#
# This is the throwaway-primary drill from docs/guides/multiplayer-test.md:
# one sqld primary in a service container, two simulated machines (fresh
# HOMEs, the guide's own rehearsal pattern), never the live store. CI is
# the judge; no local cargo/pytest.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
FNO_BIN="${FNO_BIN:?set FNO_BIN}"
FNO_AGENTS_BIN="${FNO_AGENTS_BIN:?set FNO_AGENTS_BIN}"
PRIMARY_URL="${PRIMARY_URL:-http://127.0.0.1:18080}"

fail() { echo "FAIL: $*" >&2; exit 1; }

# Two simulated machines: fresh HOMEs, each pointing at the throwaway
# primary. One checkout stands in for the repo on both machines: same repo,
# same slug, two local stores.
HOME_A=$(mktemp -d)
HOME_B=$(mktemp -d)
trap 'rm -rf "$HOME_A" "$HOME_B"' EXIT
for home in "$HOME_A" "$HOME_B"; do
    mkdir -p "$home/.fno"
    printf '[store]\nremote_url = "%s"\n' "$PRIMARY_URL" > "$home/.fno/config.toml"
done

journal_a="$HOME_A/.fno/spaces/proof/events.jsonl"
journal_b="$HOME_B/.fno/spaces/proof/events.jsonl"

# Machine A: commit a head-pinned attestation through the real emit door.
# The commit hook does the mirroring; the script never calls the carrier
# for the write, so the proof exercises the producer path end to end.
head_sha=$(git -C "$root" rev-parse HEAD)
ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
cat > "$HOME_A/envelope.json" <<EOF
{"type":"review_attestation","ts":"$ts","source":"test","data":{"reviewer":"code-review","head_sha":"$head_sha","verdict":"pass","session_id":"proof-a","branch":"main"}}
EOF
env HOME="$HOME_A" "$FNO_BIN" doctor event emit-envelope \
    --events "$journal_a" --file "$HOME_A/envelope.json" >"$HOME_A/emit.out" 2>"$HOME_A/emit.err" \
    || fail "machine A emit exited nonzero: $(cat "$HOME_A/emit.err")"
echo "PASS: machine A committed the attestation"

# The push rides a child of the commit, so poll rather than sleep: the
# proof stays honest about what actually landed on the primary.
row=""
for _ in $(seq 1 20); do
    row=$(env HOME="$HOME_A" "$FNO_AGENTS_BIN" attest pull 2>/dev/null)
    [ -n "$row" ] && break
    sleep 0.5
done
echo "$row" | grep -F "\"head_sha\":\"$head_sha\"" >/dev/null \
    || fail "the primary does not hold A's attestation; pull answered: $row"
echo "PASS: the primary holds A's attestation"

# Machine B's gate read: B's journals are empty, so the row can only come
# from the primary segment. This is the exact read (native rows verb,
# review_attestation types) the Python merge gate consults through
# journal_lines.
rows_b=$(env HOME="$HOME_B" "$FNO_BIN" doctor event rows \
    --events "$journal_b" --type review_attestation 2>"$HOME_B/rows.err") \
    || fail "machine B rows read failed: $(cat "$HOME_B/rows.err")"
# The rows verb prints a JSON array, so envelope quotes arrive escaped;
# strip the backslashes before matching the raw substring.
echo "$rows_b" | tr -d '\\' | grep -F "\"head_sha\":\"$head_sha\"" >/dev/null \
    || fail "machine B's gate read stayed machine-local; rows answered: $rows_b"
echo "PASS: machine B's gate read sees A's review"

# A's own read answers ONE copy: the journal original plus the primary
# mirror dedup to a single row, by the ts+data-minus-repo key.
rows_a=$(env HOME="$HOME_A" "$FNO_BIN" doctor event rows \
    --events "$journal_a" --type review_attestation 2>"$HOME_A/rows.err") \
    || fail "machine A rows read failed: $(cat "$HOME_A/rows.err")"
count=$( { echo "$rows_a" | tr -d '\\' | grep -oF "\"head_sha\":\"$head_sha\"" || true; } | grep -c .)
if [ "$count" -ne 1 ]; then
    fail "A's read shows $count copies (expected 1); rows: $rows_a"
fi
echo "PASS: A's read dedups its own row against the primary copy"

echo "two-machine attestation proof: 4/4 checks passed"
