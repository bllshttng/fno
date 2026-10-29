#!/usr/bin/env bash
# scripts/ci/check-config-schema-drift.sh
#
# Config-schema anti-drift gate. The Pydantic SettingsModel is the single
# source of truth. It generates crates/fno/config_schema.json, and the native
# `fno config schema` renders docs/configuration-guide.md and both
# config.example.toml copies from that table. This fails the moment either
# link drifts.
#
# The deeper guards (registry completeness, table freshness, bash-default
# equality) live in cli/tests/test_config_schema_drift.py and the docs check
# in crates/fno/tests/config_schema_parity.rs; this shell gate is the named
# local surface for both links.
#
# Fail-closed: any error resolving the tools / the repo is a non-zero exit.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$REPO_ROOT"

if ! command -v uv >/dev/null 2>&1; then
    echo "check-config-schema-drift: 'uv' is not available" >&2
    exit 2
fi
# The table is embedded at compile time, so an installed `fno` would check the
# docs against its own old table and pass. Build this checkout's front door
# unless FNO_BIN names one.
if [ -n "${FNO_BIN:-}" ]; then
    FNO=("$FNO_BIN")
else
    if ! command -v cargo >/dev/null 2>&1; then
        echo "check-config-schema-drift: 'cargo' is not available (or set FNO_BIN to this checkout's build)" >&2
        exit 2
    fi
    FNO=(cargo run --quiet --manifest-path crates/fno/Cargo.toml --bin fno --)
fi

(cd cli && uv run python -m fno.config.schema_gen --check)
"${FNO[@]}" config schema --markdown --check
"${FNO[@]}" config schema --toml --check
