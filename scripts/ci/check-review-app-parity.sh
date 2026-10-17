#!/usr/bin/env bash
# scripts/ci/check-review-app-parity.sh
#
# Cross-language review-App login + usage-marker parity check.
#
# The set of GitHub review Apps footnote recognizes is declared TWO times in
# two languages (the third copy lived in the deleted review_capability module;
# the Rust capability door reads BOT_PROFILES directly):
#
#   - Rust: BOT_PROFILES              in crates/fno-agents/src/loopcheck/bot_nudge.rs
#             (the gate's review classifier, refusal detection, nudging, and
#             the native capability door's recognized-login set)
#   - Python: DEFAULT_OPTIONAL_APPS            in cli/src/fno/pr/_reviews.py
#             (the optional-review signal on `fno do pr status`)
#
# Compared: the LOGIN set across both (a login on only one side is an App one
# path honors and another cannot explain), and that every Rust profile
# declares an explicit `usage_markers` field. A new App added to BOT_PROFILES
# with no marker decision is the AC10 failure in waiting: an unrecognized
# refusal then classifies as `absent` only if the markers were characterized,
# and "no marker matched" must never read as "did not refuse".
#
# The markers themselves are single-sourced in Rust (body_is_usage_limit unions
# them); the Python readers have no marker twin by design, so markers are not
# cross-compared - only their presence per profile is enforced.
#
# Pure text extraction (stdlib `ast` for Python, regex for Rust). No build, no
# venv, no Rust binary, so it is cheap enough to run on both the cli and
# crates CI legs. Mirrors scripts/ci/check-reviewer-descriptor-parity.sh.
#
# Exit codes:
#   0  both tables declare the same login set; every profile has markers
#   1  drift, or a table could not be extracted / is empty
#   2  usage error
#
# Flags:
#   --rust-file PATH           override the Rust source    (default: canonical)
#   --optional-file PATH       override DEFAULT_OPTIONAL_APPS src (default: canonical)
#   --selftest                 run built-in fixtures proving the check detects
#                              match / added-login / drifted-login / no-markers

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

RUST_FILE="${REPO_ROOT}/crates/fno-agents/src/loopcheck/bot_nudge.rs"
OPTIONAL_FILE="${REPO_ROOT}/cli/src/fno/config/__init__.py"
SELFTEST=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --rust-file)       RUST_FILE="$2"; shift 2 ;;
        --optional-file)   OPTIONAL_FILE="$2"; shift 2 ;;
        --selftest)        SELFTEST=1; shift ;;
        -h|--help)
            sed -n '2,40p' "${BASH_SOURCE[0]}"
            exit 0 ;;
        *) echo "check-review-app-parity: unknown flag: $1" >&2; exit 2 ;;
    esac
done

check_parity() {
    python3 - "$1" "$2" <<'PY'
import ast
import re
import sys

rust_path, optional_path = sys.argv[1:3]


def fail(msg):
    print(f"ERROR: {msg}", file=sys.stderr)
    sys.exit(1)


def read(path, label):
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read()
    except OSError as exc:
        fail(f"could not read {label} source {path}: {exc}")


def extract_string_container(src, path, target, label):
    """A name bound to a tuple/frozenset/set/list of string literals.

    Parses with `ast` so an implicitly-concatenated or parenthesized literal is
    read correctly; reading it wrong reports false parity, the one outcome worse
    than no check. Returns the set of string values.
    """
    try:
        tree = ast.parse(src)
    except SyntaxError as exc:
        fail(f"{label} ({target}) in {path} did not parse: {exc}")
    for node in ast.walk(tree):
        value = None
        if isinstance(node, ast.Assign) and any(
            isinstance(t, ast.Name) and t.id == target for t in node.targets
        ):
            value = node.value
        elif (
            isinstance(node, ast.AnnAssign)
            and isinstance(node.target, ast.Name)
            and node.target.id == target
        ):
            value = node.value  # annotated assign: _X: frozenset[str] = frozenset(...)
        if value is not None:
            elts = _container_elts(value)
            if elts is None:
                fail(f"{label} ({target}) in {path} is not a string container")
            return _string_set(elts, target, path, label)
    fail(f"{label} ({target}) not found in {path}")


def _container_elts(node):
    """elts of a tuple/list/set/frozenset of literals, else None."""
    if isinstance(node, (ast.Tuple, ast.List, ast.Set)):
        return node.elts
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) \
            and node.func.id in {"frozenset", "set", "tuple", "list"} \
            and node.args:
        arg = node.args[0]
        if isinstance(arg, (ast.Tuple, ast.List, ast.Set)):
            return arg.elts
    return None


def _string_set(elts, target, path, label):
    out = set()
    for e in elts:
        if isinstance(e, ast.Constant) and isinstance(e.value, str):
            out.add(e.value)
        else:
            fail(f"{label} ({target}) in {path} has a non-string element")
    return out


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
optional_src = read(optional_path, "DEFAULT_OPTIONAL_APPS")

rust = extract_rust(rust_src, rust_path)
optional = extract_string_container(optional_src, optional_path, "DEFAULT_OPTIONAL_APPS", "DEFAULT_OPTIONAL_APPS")

if not rust:
    fail(f"no review-App profiles extracted from {rust_path}")

problems = []
logins = sorted(set(rust) | set(optional))
for login in logins:
    if login not in rust:
        problems.append(f"  {login}: in Python, missing from Rust BOT_PROFILES")
    if login not in optional:
        problems.append(f"  {login}: in Rust, missing from Python DEFAULT_OPTIONAL_APPS")

if problems:
    print("ERROR: review-App login drift across the two tables.", file=sys.stderr)
    print("\n".join(problems), file=sys.stderr)
    print(
        f"  Rust  : BOT_PROFILES in {rust_path}\n"
        f"  Python: DEFAULT_OPTIONAL_APPS in {optional_path}\n"
        "  A login on only one side is an App one path honors and another cannot "
        "explain (a bot the gate sees but `fno do pr status` does not, or vice versa).",
        file=sys.stderr,
    )
    sys.exit(1)

marker_summary = ", ".join(
    f"{k}={len(v)} marker(s)" for k, v in sorted(rust.items())
)
print(
    f"review app parity OK: {len(rust)} app(s) ({', '.join(logins)}); {marker_summary}"
)
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

    _optional() { # file, extra-login-or-empty
        cat > "$1" <<EOF
DEFAULT_OPTIONAL_APPS = ("gemini-code-assist", "chatgpt-codex-connector"$2)
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

    _case() { # name expected_rc rust optional
        "${BASH_SOURCE[0]}" --rust-file "$3" --optional-file "$4" >/dev/null 2>&1
        rc=$?
        if [[ "$rc" == "$2" ]]; then
            echo "  ok: $1 (exit $rc)"
        else
            echo "  FAIL: $1 expected exit $2 got $rc" >&2
            fails=$((fails + 1))
        fi
    }

    echo "check-review-app-parity selftest:"
    _rust "$tmp/r.rs"; _optional "$tmp/o.py" ""
    _case "both agree" 0 "$tmp/r.rs" "$tmp/o.py"

    # extra login on python optional side
    _optional "$tmp/o2.py" ', "stray-bot"'
    _case "extra login in DEFAULT_OPTIONAL_APPS" 1 "$tmp/r.rs" "$tmp/o2.py"

    # extra login on rust side only (a profile missing markers also fails)
    _rust_nomarker "$tmp/r3.rs"
    _case "profile without usage_markers" 1 "$tmp/r3.rs" "$tmp/o.py"

    # login missing from DEFAULT_OPTIONAL_APPS
    cat > "$tmp/o3.py" <<'EOF'
DEFAULT_OPTIONAL_APPS = ("chatgpt-codex-connector",)
EOF
    _case "login missing from DEFAULT_OPTIONAL_APPS" 1 "$tmp/r.rs" "$tmp/o3.py"

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
    cat > "$tmp/o6.py" <<'EOF'
DEFAULT_OPTIONAL_APPS: tuple[str, ...] = ("chatgpt-codex-connector",)
EOF
    _case "collapsed single-entry table parses" 0 "$tmp/r6.rs" "$tmp/o6.py"

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

check_parity "$RUST_FILE" "$OPTIONAL_FILE"
