#!/usr/bin/env bash
# Regen the config-schema goldens from the `fno` on PATH. The parity test
# renders with the crate-built binary, so build `fno` from this checkout and
# put it on PATH before running this, or run it from a checkout whose `fno`
# renders the same bytes as the crate (see crates/fno/tests/config_schema_parity.rs).
set -u

root=$(git rev-parse --show-toplevel) || exit 1
out_dir="crates/fno/tests/golden/config_schema"
cd "$root" || exit 1

fold() {
    # The parity test replaces the repo root with <ROOT> before comparing.
    sed -e "s|$root|<ROOT>|g"
}

capture() { # <golden-stem> <exit-code> <command...>
    stem=$1
    want=$2
    shift 2
    stdout=$(mktemp)
    stderr=$(mktemp)
    "$@" >"$stdout" 2>"$stderr"
    got=$?
    if [ "$got" != "$want" ]; then
        echo "regen: $stem exited $got, expected $want" >&2
        rm -f "$stdout" "$stderr"
        exit 1
    fi
    fold <"$stdout" >"$out_dir/$stem.stdout"
    # The parity test reads a missing .stderr as empty, so write one only
    # when the command printed something (two_modes is the one non-empty).
    if [ -s "$stderr" ]; then
        fold <"$stderr" >"$out_dir/$stem.stderr"
    fi
    rm -f "$stdout" "$stderr"
    echo "regen: wrote $out_dir/$stem.stdout"
}

capture json_schema 0 fno config schema --json-schema
capture wizard_plan 0 fno config schema --wizard-plan
capture markdown 0 fno config schema --markdown
capture toml 0 fno config schema --toml
capture markdown_check 0 fno config schema --markdown --check
capture toml_check 0 fno config schema --toml --check
capture two_modes 2 fno config schema --markdown --toml
capture setup_plan 0 fno config setup plan
capture setup_plan_advanced 0 fno config setup plan --advanced
