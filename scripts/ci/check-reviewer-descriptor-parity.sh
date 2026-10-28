#!/usr/bin/env bash
# scripts/ci/check-reviewer-descriptor-parity.sh
#
# Cross-language reviewer-descriptor parity check (node).
#
# The set of local reviewers, and the invocation that satisfies each one, is
# declared twice:
#
#   - Python: _RESOLVABLE_REVIEWERS  in cli/src/fno/config/__init__.py
#             (the config validator + the init capability refusal)
#   - Rust: REVIEWER_INVOCATIONS   in
#             crates/fno-agents/src/loopcheck/self_review_floor.rs
#             (the stop gate's blocked reason)
#
# Compared per reviewer: the invocation string, whether it is a self-cert
# (Python `asserts == "self-cert"`, Rust's third tuple element), and the
# per-harness command overrides (Python `invocations` map, Rust's fourth element
# encoded `"harness=command;..."`).
#
# A name added on one side is a reviewer the other cannot explain. A drifted
# invocation is worse: the blocked reason then tells a wedged session to run a
# command that does not satisfy the gate, which is the failure class this whole
# node exists to delete. A drifted self-cert flag is worse still: the stop gate
# would name `declare` as an ordinary reviewer, inviting an operator to clear a
# review gate with nothing behind it. The old constant's own comment conceded the shape -
# "Kept in sync with the emit surfaces ... and the loop-check attestation read"
# - which is a request that a human remember. This makes it mechanical.
#
# Pure text extraction (stdlib `ast` for Python, a regex for Rust). No build,
# no venv, no Rust binary, so it is cheap enough to run on both the cli and
# crates CI legs.
#
# Exit codes:
#   0  the two tables declare the same names with the same invocations
#   1  drift, or a table could not be extracted / is empty
#   2  usage error
#
# Flags:
#   --rust-file PATH    override the Rust source   (default: canonical path)
#   --python-file PATH  override the Python source (default: canonical path)
#   --selftest          run built-in fixtures proving the check detects
#                       match / renamed / drifted-invocation / unextractable

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

RUST_FILE="${REPO_ROOT}/crates/fno-agents/src/loopcheck/self_review_floor.rs"
PYTHON_FILE="${REPO_ROOT}/cli/src/fno/config/__init__.py"
SELFTEST=0

# ── Extraction + comparison ────────────────────────────────────────────
# One python3 pass reads both sides and diffs them. The Python side is parsed
# with `ast` rather than regex because a descriptor's `invocation` may be an
# implicitly-concatenated multi-line string, which no line-oriented extractor
# reads correctly - and reading it WRONG here would report false parity, the
# one outcome worse than no check.

check_parity() {
    python3 - "$1" "$2" <<'PY'
import ast
import re
import sys

rust_path, python_path = sys.argv[1], sys.argv[2]


def fail(msg):
    print(f"ERROR: {msg}", file=sys.stderr)
    sys.exit(1)


def read(path, label):
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read()
    except OSError as exc:
        fail(f"{label} source not readable: {path} ({exc})")


def _normalize_per_harness(value):
    """Coerce a per-harness map to a plain {harness: command} dict, {} if absent."""
    if not isinstance(value, dict):
        return {}
    return {str(k): str(v) for k, v in value.items()}


def extract_python(src, path):
    """{name: (invocation, is_self_cert, per_harness)} from _RESOLVABLE_REVIEWERS."""
    try:
        tree = ast.parse(src)
    except SyntaxError as exc:
        fail(f"cannot parse {path}: {exc}")
    for node in ast.walk(tree):
        targets = []
        if isinstance(node, ast.Assign):
            targets = node.targets
        elif isinstance(node, ast.AnnAssign):
            targets = [node.target]
        if not any(
            isinstance(t, ast.Name) and t.id == "_RESOLVABLE_REVIEWERS" for t in targets
        ):
            continue
        value = node.value
        if not isinstance(value, ast.Dict):
            fail(f"_RESOLVABLE_REVIEWERS in {path} is not a dict literal")
        out = {}
        for key, val in zip(value.keys, value.values):
            if not isinstance(key, ast.Constant) or not isinstance(key.value, str):
                fail(f"_RESOLVABLE_REVIEWERS in {path} has a non-literal key")
            if not isinstance(val, ast.Call):
                fail(f"reviewer {key.value!r} in {path} is not a ReviewerDescriptor(...)")
            fields = {}
            for kw in val.keywords:
                if kw.arg in ("invocation", "asserts", "invocations", "requires"):
                    try:
                        fields[kw.arg] = ast.literal_eval(kw.value)
                    except ValueError:
                        fail(f"reviewer {key.value!r} in {path} has a non-literal {kw.arg}")
            invocation = fields.get("invocation")
            asserts = fields.get("asserts")
            requires = fields.get("requires")
            if not isinstance(invocation, str) or not invocation:
                fail(f"reviewer {key.value!r} in {path} declares no invocation")
            if asserts not in ("review-evidence", "self-cert"):
                fail(f"reviewer {key.value!r} in {path} declares no valid asserts")
            per = _normalize_per_harness(fields.get("invocations"))
            out[key.value] = (invocation, asserts == "self-cert", per, requires)
        return out
    fail(f"_RESOLVABLE_REVIEWERS not found in {path}")


def _parse_per_harness(s):
    """Parse Rust's `"harness=command;harness=command"` encoding to a {harness: command} dict."""
    out = {}
    for pair in s.split(";"):
        pair = pair.strip()
        if not pair:
            continue
        if "=" not in pair:
            fail(f"per-harness pair {pair!r} has no '='")
        h, _, v = pair.partition("=")
        out[h.strip()] = v.strip()
    return out


def extract_rust(src, path):
    """{name: (invocation, is_self_cert, per_harness)} from REVIEWER_INVOCATIONS."""
    m = re.search(
        r"const\s+REVIEWER_INVOCATIONS\s*:\s*&\[\(&str,\s*&str,\s*bool,\s*&str\)\]\s*=\s*&\[(.*?)\n\];",
        src,
        re.S,
    )
    if not m:
        fail(f"REVIEWER_INVOCATIONS not found in {path}")
    # Matched as whole tuples, not as a flat literal stream: positional pairing
    # of quoted strings would silently mis-align the moment a field is added,
    # and reporting false parity is the one outcome worse than no check.
    entries = re.findall(
        r'\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"\s*,\s*(true|false)\s*,\s*"((?:[^"\\]|\\.)*)"\s*,?\s*\)',
        m.group(1),
        re.S,
    )
    if not entries:
        fail(f"REVIEWER_INVOCATIONS in {path} did not extract as (name, invocation, bool, per)")
    # A PARTIAL extraction is the false-parity path: an entry whose formatting
    # the regex misses is skipped, and if it is Rust-only it produces no problem
    # at all - silently hiding the exact "declared in Rust, missing from Python"
    # case this check advertises. Counting tuple openings catches the realistic
    # edit (a non-literal bool or invocation). It does NOT catch an entry whose
    # NAME is a constant, since both patterns anchor on `("` and miss it equally
    # - a coarser count would fire on any `(` in a comment and fail a valid
    # table, which is the worse trade.
    opened = len(re.findall(r'\(\s*"', m.group(1)))
    if opened != len(entries):
        fail(
            f"REVIEWER_INVOCATIONS in {path}: {opened} entries present but only "
            f"{len(entries)} parsed; an unmatched entry would be compared as absent"
        )
    out = {}
    for name, inv, flag, per in entries:
        out[name.encode().decode("unicode_escape")] = (
            inv.encode().decode("unicode_escape"),
            flag == "true",
            _parse_per_harness(per.encode().decode("unicode_escape")),
        )
    return out


def extract_rust_requires(src, path):
    """{name: requires} from REVIEWER_BUILTIN_REQUIRES in the same Rust file.

    The `requires` capability column the review gate's descriptor union
    reads. A reviewer in REVIEWER_INVOCATIONS with no requires row here is a
    name the gate cannot compose a descriptor for, and is flagged like any
    other one-sided declaration.
    """
    m = re.search(
        r"const\s+REVIEWER_BUILTIN_REQUIRES\s*:\s*&\[\(&str,\s*&str\)\]\s*=\s*&\[(.*?)\n\];",
        src,
        re.S,
    )
    if not m:
        fail(f"REVIEWER_BUILTIN_REQUIRES not found in {path}")
    entries = re.findall(
        r'\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"\s*,?\s*\)',
        m.group(1),
        re.S,
    )
    if not entries:
        fail(f"REVIEWER_BUILTIN_REQUIRES in {path} did not extract as (name, requires)")
    opened = len(re.findall(r'\(\s*"', m.group(1)))
    if opened != len(entries):
        fail(
            f"REVIEWER_BUILTIN_REQUIRES in {path}: {opened} entries present but only "
            f"{len(entries)} parsed; an unmatched entry would be compared as absent"
        )
    return {
        name.encode().decode("unicode_escape"): req.encode().decode("unicode_escape")
        for name, req in entries
    }


py = extract_python(read(python_path, "Python"), python_path)
rs = extract_rust(read(rust_path, "Rust"), rust_path)
rs_req = extract_rust_requires(read(rust_path, "Rust"), rust_path)

if not py:
    fail(f"no reviewers extracted from {python_path}")

problems = []
for name in sorted(set(py) | set(rs)):
    if name not in rs:
        problems.append(f"  {name}: declared in Python, missing from Rust")
    elif name not in py:
        problems.append(f"  {name}: declared in Rust, missing from Python")
    elif py[name][0] != rs[name][0]:
        problems.append(
            f"  {name}: invocation differs\n"
            f"    Python: {py[name][0]!r}\n"
            f"    Rust  : {rs[name][0]!r}"
        )
    elif py[name][1] != rs[name][1]:
        problems.append(
            f"  {name}: self-cert flag differs\n"
            f"    Python asserts self-cert: {py[name][1]}\n"
            f"    Rust  self-cert        : {rs[name][1]}"
        )
    elif py[name][2] != rs[name][2]:
        problems.append(
            f"  {name}: per-harness commands differ\n"
            f"    Python: {py[name][2]!r}\n"
            f"    Rust  : {rs[name][2]!r}"
        )
    elif py[name][3] != rs_req.get(name):
        problems.append(
            f"  {name}: requires capability differs\n"
            f"    Python: {py[name][3]!r}\n"
            f"    Rust  : {rs_req.get(name)!r}"
        )

if problems:
    print("ERROR: reviewer descriptor drift between Python and Rust.", file=sys.stderr)
    print("\n".join(problems), file=sys.stderr)
    print(
        f"  Python: _RESOLVABLE_REVIEWERS in {python_path}\n"
        f"  Rust  : REVIEWER_INVOCATIONS + REVIEWER_BUILTIN_REQUIRES in {rust_path}\n"
        "  A drifted invocation makes the stop gate name a command that does "
        "not satisfy the gate.",
        file=sys.stderr,
    )
    sys.exit(1)

print(f"reviewer descriptor parity OK: {len(py)} reviewers ({', '.join(sorted(py))})")
PY
}

# ── Selftest ───────────────────────────────────────────────────────────
# A check that can only ever pass is worse than no check. Drives the real
# script over synthetic fixtures and asserts the expected exit codes.

run_selftest() {
    local tmp rc fails=0
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/reviewer-parity-selftest.XXXXXX")
    trap 'rm -rf "$tmp"' RETURN

    _rust() { # file, sigma-invocation, declare-self-cert-flag, sigma-per-harness
        cat > "$1" <<EOF
const REVIEWER_INVOCATIONS: &[(&str, &str, bool, &str)] = &[
    ("sigma", "$2", false, "${4:-}"),
    (
        "declare",
        "/fno:review declare",
        ${3:-true},
        "",
    ),
];
const REVIEWER_BUILTIN_REQUIRES: &[(&str, &str)] = &[
    ("sigma", "subagent-dispatch"),
    ("declare", "none"),
];
EOF
    }
    _python() {
        cat > "$1" <<EOF
_RESOLVABLE_REVIEWERS: dict[str, ReviewerDescriptor] = {
    "sigma": ReviewerDescriptor(
        kind="local-attestation",
        requires="subagent-dispatch",
        invocation=(
            "/fno:review "
            "$2"
        ),
        asserts="review-evidence",
    ),
    "declare": ReviewerDescriptor(
        kind="local-attestation",
        requires="none",
        invocation="/fno:review declare",
        asserts="self-cert",
    ),
}
EOF
    }

    _case() { # name expected_rc rust_file python_file
        "${BASH_SOURCE[0]}" --rust-file "$3" --python-file "$4" >/dev/null 2>&1
        rc=$?
        if [[ "$rc" == "$2" ]]; then echo "  PASS: $1"
        else echo "  FAIL: $1 (expected exit $2, got $rc)"; fails=$((fails + 1)); fi
    }

    # Case 1: the two tables agree, across an implicit multi-line Python
    # concatenation - the exact shape a line-oriented extractor gets wrong.
    _rust "$tmp/ok.rs" "/fno:review sigma"
    _python "$tmp/ok.py" "sigma"
    _case "matching tables accepted (multi-line invocation)" 0 "$tmp/ok.rs" "$tmp/ok.py"

    # Case 1b: the self-cert flag drifts while every invocation still matches.
    # A positional literal-pairing extractor would call this parity.
    _rust "$tmp/selfcert.rs" "/fno:review sigma" false
    _python "$tmp/selfcert.py" "sigma"
    _case "drifted self-cert flag rejected" 1 "$tmp/selfcert.rs" "$tmp/selfcert.py"

    # Case 2: same names, drifted invocation -> reject.
    _rust "$tmp/drift.rs" "/fno:review sigma --wait"
    _python "$tmp/drift.py" "sigma"
    _case "drifted invocation rejected" 1 "$tmp/drift.rs" "$tmp/drift.py"

    # Case 3: a reviewer added on one side only -> reject.
    cat > "$tmp/extra.rs" <<'EOF'
const REVIEWER_INVOCATIONS: &[(&str, &str, bool)] = &[
    ("sigma", "/fno:review sigma", false),
];
EOF
    _python "$tmp/extra.py" "sigma"
    _case "one-sided reviewer rejected" 1 "$tmp/extra.rs" "$tmp/extra.py"

    # Case 3b: one Rust entry the regex cannot match (a non-literal bool).
    # Without the count guard it is skipped and, being Rust-only, reported clean.
    cat > "$tmp/partial.rs" <<'EOF'
const REVIEWER_INVOCATIONS: &[(&str, &str, bool)] = &[
    ("sigma", "/fno:review sigma", false),
    ("declare", "/fno:review declare", true),
    ("ghost", "/fno:review ghost", IS_SELF_CERT),
];
EOF
    _python "$tmp/partial.py" "sigma"
    _case "partially-extracted Rust table rejected" 1 "$tmp/partial.rs" "$tmp/partial.py"

    # Case 4: the Rust table absent -> reject, not a silent empty==empty pass.
    printf '// no table here\n' > "$tmp/absent.rs"
    _python "$tmp/absent.py" "sigma"
    _case "missing Rust table rejected" 1 "$tmp/absent.rs" "$tmp/absent.py"

    # Case 5: the Python table absent -> reject.
    _rust "$tmp/nopy.rs" "/fno:review sigma"
    printf '# no table here\n' > "$tmp/nopy.py"
    _case "missing Python table rejected" 1 "$tmp/nopy.rs" "$tmp/nopy.py"

    # Case 6: per-harness commands declared on one side only -> reject. A
    # positional literal-pairing extractor would never see the map at all.
    _rust "$tmp/per.rs" "/fno:review sigma"
    cat > "$tmp/per.py" <<'EOF'
_RESOLVABLE_REVIEWERS: dict[str, ReviewerDescriptor] = {
    "sigma": ReviewerDescriptor(
        kind="local-attestation",
        requires="subagent-dispatch",
        invocation="/fno:review sigma",
        invocations={"codex": "/review"},
        asserts="review-evidence",
    ),
    "declare": ReviewerDescriptor(
        kind="local-attestation",
        requires="none",
        invocation="/fno:review declare",
        asserts="self-cert",
    ),
}
EOF
    _case "drifted per-harness commands rejected" 1 "$tmp/per.rs" "$tmp/per.py"

    # Case 7: the requires capability drifts on one side -> reject. A drifted
    # requires flips the verdict class the gate reports: sigma resolves
    # "unavailable (retired)" instead of "needs subagent-dispatch", so the
    # init refusal names the wrong remedy.
    _rust "$tmp/requires.rs" "/fno:review sigma"
    _python "$tmp/requires.py" "sigma"
    _case "matching tables accepted with requires" 0 "$tmp/requires.rs" "$tmp/requires.py"

    _rust "$tmp/requires_drift.rs" "/fno:review sigma"
    cat > "$tmp/requires_drift.py" <<'EOF'
_RESOLVABLE_REVIEWERS: dict[str, ReviewerDescriptor] = {
    "sigma": ReviewerDescriptor(
        kind="local-attestation",
        requires="none",
        invocation="/fno:review sigma",
        asserts="review-evidence",
    ),
    "declare": ReviewerDescriptor(
        kind="local-attestation",
        requires="none",
        invocation="/fno:review declare",
        asserts="self-cert",
    ),
}
EOF
    _case "drifted requires rejected" 1 "$tmp/requires_drift.rs" "$tmp/requires_drift.py"

    if [[ "$fails" -gt 0 ]]; then
        echo "reviewer-descriptor-parity selftest: $fails failure(s)" >&2
        return 1
    fi
    echo "reviewer-descriptor-parity selftest: all cases passed"
    return 0
}

# ── Arg parsing ────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --rust-file)   RUST_FILE="$2"; shift 2 ;;
        --python-file) PYTHON_FILE="$2"; shift 2 ;;
        --selftest)    SELFTEST=1; shift ;;
        -h|--help)     grep -E '^#( |$)' "$0" | sed -E 's/^# ?//'; exit 0 ;;
        *)             echo "ERROR: unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [[ "$SELFTEST" == "1" ]]; then
    run_selftest
    exit $?
fi

check_parity "$RUST_FILE" "$PYTHON_FILE"
exit $?
