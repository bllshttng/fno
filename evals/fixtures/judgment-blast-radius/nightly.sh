#!/usr/bin/env bash
set -euo pipefail
python3 export.py --out "nightly-$(date +%F).txt"
