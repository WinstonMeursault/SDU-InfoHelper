#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/local-env.sh"
cd "$PROJECT_ROOT"
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
