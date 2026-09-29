#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/local-env.sh"
binary="$PROJECT_ROOT/target/release/sdu-infohelper"
if [[ ! -x "$binary" ]]; then
    echo 'Run bash scripts/build.sh first.' >&2
    exit 1
fi
cd "$PROJECT_ROOT"
exec "$binary" "$@"
