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

# One read-only probe of the primary's schema, through the same Hrana
# pipeline the carrier writes through. Ruling three's observable half: a
# gate read on a primary that never saw a push answers "no rows" and
# creates nothing, so the probe names what the pull must not have built.
schema_probe() {
    curl -fsS -X POST "$PRIMARY_URL/v2/pipeline" -H 'Content-Type: application/json' \
        -d @- <<'JSON'
{"requests":[{"type":"execute","stmt":{"sql":"SELECT name FROM sqlite_master WHERE type = 'table'"}}]}
JSON
}

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

# Ruling three, end to end: on a fresh primary no push has ever run, so no
# table exists. B's pull for a repo that never pushed must answer "no rows"
# (exit 0, empty), never an outage, and must not create the table either.
# The unit test pins the same no-creation on the Hrana double; this probe
# pins it on the real sqld.
no_rows=$(env HOME="$HOME_B" "$FNO_AGENTS_BIN" attest pull --repo github.com/o/never-pushed) \
    || fail "a pull from a tableless primary must read as no rows (exit 0), not an outage"
[ -z "$no_rows" ] || fail "the tableless pull answered rows: $no_rows"
tables=$(schema_probe)
echo "$tables" | grep -F review_attestation >/dev/null \
    && fail "the tableless pull created the table it read: $tables"
echo "PASS: a pull from a tableless primary reads no rows and creates nothing"

# Machine A: commit a head-pinned attestation through the real emit door.
# The commit hook does the mirroring; the script never calls the carrier
# for the write, so the proof exercises the producer path end to end.

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

# Ruling one: the push stamps the writing machine id into the row, so a
# coverage listing can name which machine's review a row carries.
echo "$row" | grep -F '"attest_machine":"' >/dev/null \
    || fail "A's pushed row carries no writing-machine stamp; pull answered: $row"
echo "PASS: the pushed row names its writing machine"

# Machine B's gate read: B's journals are empty, so the row can only come
# from the primary segment. This is the exact read (native rows verb,
# review_attestation types) the Python merge gate consults through
# journal_lines.
# Diagnostics first: a silent empty segment must name its half.
echo "diag: B direct pull: $(env HOME="$HOME_B" "$FNO_AGENTS_BIN" attest pull 2>&1 || true)"
echo "diag: B slug: $(git config --get remote.origin.url 2>&1 || true)"
echo "diag: B config: $(cat "$HOME_B/.fno/config.toml")"
rows_b=$(FNO_ATTEST_DEBUG=1 env HOME="$HOME_B" "$FNO_BIN" doctor event rows \
    --events "$journal_b" --type review_attestation 2>"$HOME_B/rows.err") \
    || fail "machine B rows read failed: $(cat "$HOME_B/rows.err")"
# The rows verb prints a JSON array, so envelope quotes arrive escaped;
# strip the backslashes before matching the raw substring.
echo "$rows_b" | tr -d '\\' | grep -F "\"head_sha\":\"$head_sha\"" >/dev/null \
    || fail "machine B's gate read stayed machine-local (rows.err: $(cat "$HOME_B/rows.err")); rows answered: $rows_b"
echo "$rows_b" | tr -d '\\' | grep -F '"attest_machine":"' >/dev/null \
    || fail "machine B's gate read lost the writing-machine stamp; rows answered: $rows_b"
echo "PASS: machine B's gate read sees A's review, stamped with A's machine"

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

echo "two-machine attestation proof: 6/6 checks passed"
