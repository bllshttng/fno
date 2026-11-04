#!/usr/bin/env bash
# scripts/ci/check-review-app-parity.sh
#
# Bot-profile marker-completeness check.
#
# The set of GitHub review Apps footnote recognizes is single-sourced in Rust
# (the Python DEFAULT_OPTIONAL_APPS half died with the config family; the
# model's review.optional_apps default and the Rust capability door read
# BOT_PROFILES directly):
#
#   - Rust: BOT_PROFILES in crates/fno-agents/src/loopcheck/bot_nudge.rs
#             (the gate's review classifier, refusal detection, nudging, and
#             the native capability door's recognized-login set)
#
# Enforced: every BotProfile declares an explicit `usage_markers` field. A new
# App added to BOT_PROFILES with no marker decision is the AC10 failure in
# waiting: an unrecognized refusal then classifies as `absent` only if the
# markers were characterized, and "no marker matched" must never read as "did
# not refuse". The markers themselves are single-sourced in Rust
# (body_is_usage_limit unions them).
#
# Pure regex extraction of the Rust source. No build, no venv, no Rust binary,
# so it is cheap enough to run on both the cli and crates CI legs.
#
# Exit codes:
#   0  every profile carries an explicit usage_markers field
#   1  a profile is missing markers, or the table could not be extracted
#   2  usage error
#
# Flags:
#   --rust-file PATH           override the Rust source    (default: canonical)
#   --selftest                 run built-in fixtures proving the check detects
#                              a missing-marker profile

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

RUST_FILE="${REPO_ROOT}/crates/fno-agents/src/loopcheck/bot_nudge.rs"
SELFTEST=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --rust-file)       RUST_FILE="$2"; shift 2 ;;
        --selftest)        SELFTEST=1; shift ;;
        -h|--help)
            sed -n '2,28p' "${BASH_SOURCE[0]}"
            exit 0 ;;
        *) echo "check-review-app-parity: unknown flag: $1" >&2; exit 2 ;;
    esac
done

check_markers() {
    python3 - "$1" <<'PY'
import re
import sys

rust_path = sys.argv[1]


def fail(msg):
    print(f"ERROR: {msg}", file=sys.stderr)
    sys.exit(1)


def read(path, label):
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read()
    except OSError as exc:
        fail(f"could not read {label} source {path}: {exc}")


def extract_rust(src, path):
    """{login: sorted[markers]} from BOT_PROFILES, plus a per-profile marker
    completeness check. A profile without an explicit `usage_markers:` field is
    a new App whose refusal shape was never decided (AC10)."""
    m = re.search(r"const\s+BOT_PROFILES\s*:\s*&\[BotProfile\]\s*=\s*&\[", src)
    if not m:
        fail(f"BOT_PROFILES not found in {path}")
    # Brace-scan to the matching close, then brace-scan each BotProfile
    # literal: rustfmt collapses a single-entry table onto fewer lines, so a
    # layout regex (`\n];`, entries ending `\n},`) misses real tables.
    start = m.end()
    depth = 1
    end = start
    while end < len(src) and depth:
        if src[end] == "[":
            depth += 1
        elif src[end] == "]":
            depth -= 1
        end += 1
    if depth:
        fail(f"BOT_PROFILES in {path} is unterminated")
    body = src[start:end - 1]
    blocks = []
    pos = 0
    while True:
        b = re.search(r"BotProfile\s*\{", body[pos:])
        if not b:
            break
        open_at = pos + b.end() - 1
        depth = 1
        close_at = open_at + 1
        while close_at < len(body) and depth:
            if body[close_at] == "{":
                depth += 1
            elif body[close_at] == "}":
                depth -= 1
            close_at += 1
        if depth:
            fail(f"BOT_PROFILES in {path} has an unterminated BotProfile entry")
        blocks.append(body[open_at + 1:close_at - 1])
        pos = close_at
    if not blocks:
        fail(f"BOT_PROFILES in {path} had no BotProfile entries")
    out = {}
    no_markers = []
    for block in blocks:
        lm = re.search(r'login:\s*"((?:[^"\\]|\\.)*)"', block)
        if not lm:
            fail(f"a BotProfile in {path} has no login field")
        login = lm.group(1).encode().decode("unicode_escape")
        mm = re.search(r"usage_markers:\s*&\[(.*?)\]", block, re.S)
        if not mm:
            no_markers.append(login)
            continue
        markers = re.findall(r'"((?:[^"\\]|\\.)*)"', mm.group(1))
        out[login] = sorted(m.encode().decode("unicode_escape") for m in markers)
    if no_markers:
        fail(
            f"BOT_PROFILES in {path}: profile(s) without an explicit usage_markers "
            f"field: {', '.join(sorted(no_markers))}. A new App must declare its "
            f"refusal shape (empty list is explicit; absent is undecided)."
        )
    return out


rust_src = read(rust_path, "Rust")
rust = extract_rust(rust_src, rust_path)
if not rust:
    fail(f"no review-App profiles extracted from {rust_path}")

marker_summary = ", ".join(
    f"{k}={len(v)} marker(s)" for k, v in sorted(rust.items())
)
print(f"bot markers OK: {len(rust)} app(s); {marker_summary}")
PY
}

# ── Selftest ───────────────────────────────────────────────────────────
# A check that can only ever pass is worse than no check.

run_selftest() {
    local tmp rc fails=0
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/review-app-parity-selftest.XXXXXX")
    trap 'rm -rf "$tmp"' RETURN

    _rust() { # file ; writes two profiles, both with markers
        cat > "$1" <<'EOF'
const BOT_PROFILES: &[BotProfile] = &[
    BotProfile {
        login: "chatgpt-codex-connector",
        review_handle: "@codex review",
        usage_markers: &["usage limits for code reviews"],
        nudgeable: true,
    },
    BotProfile {
        login: "gemini-code-assist",
        review_handle: "",
        usage_markers: &[],
        nudgeable: false,
    },
];
EOF
    }

    _rust_nomarker() { # a profile missing usage_markers -> must fail
        cat > "$1" <<'EOF'
const BOT_PROFILES: &[BotProfile] = &[
    BotProfile {
        login: "chatgpt-fno-connector",
        review_handle: "@codex review",
        nudgeable: true,
    },
    BotProfile {
        login: "gemini-code-assist",
        review_handle: "",
        usage_markers: &[],
        nudgeable: false,
    },
];
EOF
    }

    _case() { # name expected_rc rust
        "${BASH_SOURCE[0]}" --rust-file "$3" >/dev/null 2>&1
        rc=$?
        if [[ "$rc" == "$2" ]]; then
            echo "  ok: $1 (exit $rc)"
        else
            echo "  FAIL: $1 expected exit $2 got $rc" >&2
            fails=$((fails + 1))
        fi
    }

    echo "check-review-app-parity selftest:"
    _rust "$tmp/r.rs"
    _case "both profiles carry markers" 0 "$tmp/r.rs"

    _rust_nomarker "$tmp/r3.rs"
    _case "profile without usage_markers" 1 "$tmp/r3.rs"

    # rustfmt-collapsed single-entry table (the shape rustfmt gives a one-bot
    # table) must parse like the expanded one
    cat > "$tmp/r6.rs" <<'EOF'
pub(super) const BOT_PROFILES: &[BotProfile] = &[BotProfile {
    login: "chatgpt-codex-connector",
    review_handle: "@codex review",
    usage_markers: &["usage limits for code reviews", "codex usage limits"],
    nudgeable: true,
}];
EOF
    _case "collapsed single-entry table parses" 0 "$tmp/r6.rs"

    if [[ "$fails" == 0 ]]; then
        echo "selftest OK"
        return 0
    fi
    echo "selftest FAILED ($fails)" >&2
    return 1
}

if [[ "$SELFTEST" == 1 ]]; then
    run_selftest
    exit $?
fi

check_markers "$RUST_FILE"
