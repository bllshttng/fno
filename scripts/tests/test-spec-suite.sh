#!/usr/bin/env bash
set -euo pipefail

bash tests/spec/test_blueprint_phase_close.sh
bash tests/spec/test_claims_arg.sh
bash tests/spec/test_executor_transcription.sh
# FNO_AGENTS_BIN outranks PATH (the claim-door contract): the smoke legs run
# with the binary pinned, never on PATH.
"${FNO_AGENTS_BIN:-fno-agents}" test-run doctor --stream \
  tests/spec/test_product_md_check.py \
  tests/spec/test_impeccable_stages_validator.py
