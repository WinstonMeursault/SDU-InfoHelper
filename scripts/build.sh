#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/local-env.sh"
cd "$PROJECT_ROOT"
exec cargo build --release --locked
