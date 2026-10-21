#!/usr/bin/env bash
# The preflight receipt scope is written by hand in two files, and it has
# already drifted once, silently.
#
# scripts/ci/preflight.sh declares the legs a full run must record;
# crates/fno-agents/src/verify_evidence.rs decides whether a receipt is
# trustworthy by comparing against its own copy. The check is "every required
# leg present, nothing unknown added", so a rename on one side fails it in
# BOTH directions: the receipt is DISCARDED rather than rejected, and with
# preflight.required = true a green preflight can never clear the gate.
# preflight.sh itself carries the array twice, so all copies are compared, not
# the first one a search happens to land on.
#
# The third copy (cli/src/fno/pr/_preflight.py) retired with its port; the
# Rust side is the one owner left beside the shell.
#
# The integration-target lists this also used to guard are gone: both callers
# now pass `--test '*'`, and a glob cannot drop a file the way the hand-written
# enumeration dropped three.
#
# The check reports the size of every set it compared. A count is the positive
# marker that the parse found anything: a regex that matches nothing reports
# "0 legs" rather than passing quietly.
set -uo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "parity=unavailable reason=not-a-git-checkout" >&2
    exit 2
}
cd "$repo_root" || exit 2

python3 - <<'PY'
import re
import sys

fail = 0


def die(msg):
    global fail
    print(f"  FAIL: {msg}")
    fail = 1


preflight = open("scripts/ci/preflight.sh").read()


print("== the preflight receipt scope reads the same in both files ==")
# EVERY copy, not the first one a search happens to land on. preflight.sh
# carries the array twice - the void path and the verdict-bearing path - and
# reading only the first left a rename in the second one invisible while every
# receipt it produced went untrusted. That is the exact bug this guard exists
# to catch, so the guard must not contain it.
copies = re.findall(r"REQUIRED_SCOPE_NAMES=\(([^)]*)\)", preflight)
if not copies:
    die("REQUIRED_SCOPE_NAMES not found in scripts/ci/preflight.sh")
    sys.exit(fail)
sets = [set(c.split()) for c in copies]
declared = sets[0]
copies_agree = True
for i, other in enumerate(sets[1:], 2):
    if other != declared:
        die(f"preflight.sh REQUIRED_SCOPE_NAMES copy {i} disagrees with copy 1: "
            f"missing {sorted(declared - other) or 'nothing'}, "
            f"extra {sorted(other - declared) or 'nothing'}")
        copies_agree = False
if copies_agree:
    print(f"  ok: {len(copies)} REQUIRED_SCOPE_NAMES copies in preflight.sh, identical")

rs = open("crates/fno-agents/src/verify_evidence.rs").read()

RS_FILE = "crates/fno-agents/src/verify_evidence.rs"


def literals(text, pattern):
    m = re.search(pattern, text, re.S)
    return set(re.findall(r'"([^"]+)"', m.group(1))) if m else set()


# The OPTIONAL set is checked too, and it is not a lesser case. The eligibility
# test rejects an UNKNOWN leg exactly as hard as it rejects a missing required
# one, so a leg added to one side's optional list and not the other's still
# untrusts every receipt - and the historical break this guard's header cites,
# `tracker-gates:fno`, was an optional leg. preflight.sh names the optional
# legs at their conditional `REQUIRED_SCOPE_NAMES+=(...)` sites, so the shell
# side of the optional comparison reads those additions.
additions = re.findall(r"REQUIRED_SCOPE_NAMES\+=\(([^)]*)\)", preflight)
shell_optional = set()
for addition in additions:
    shell_optional.update(addition.split())
shell_optional -= declared

groups = [
    ("required", declared,
     literals(rs, r"const PREFLIGHT_BASE_SCOPE: \[&str; \d+\] = \[(.*?)\];")),
    ("optional", shell_optional,
     literals(rs, r"const PREFLIGHT_OPTIONAL_SCOPE: \[&str; \d+\] = \[(.*?)\];")),
]

if not declared:
    die("read 0 required scope names from preflight.sh")
if not shell_optional:
    die("read 0 optional legs from preflight.sh's conditional additions")

for kind, from_sh, rs_names in groups:
    if not rs_names:
        die(f"{RS_FILE}: could not read the {kind} scope list")
        continue
    if rs_names != from_sh:
        die(f"{RS_FILE} {kind} scope disagrees with preflight.sh: "
            f"missing {sorted(from_sh - rs_names) or 'nothing'}, "
            f"extra {sorted(rs_names - from_sh) or 'nothing'}")
    else:
        print(f"  ok: {len(from_sh)} {kind} legs, identical across both files")

sys.exit(fail)
PY
rc=$?
echo ""
if ((rc == 0)); then
    echo "test_preflight_parity: ALL PASS"
else
    echo "test_preflight_parity: FAILED"
fi
exit $rc
