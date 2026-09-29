#!/usr/bin/env bash
# Source from project scripts; changes apply only to their child processes.
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export XDG_CACHE_HOME="$PROJECT_ROOT/.cache"
export XDG_CONFIG_HOME="$PROJECT_ROOT/.local/config"
export XDG_DATA_HOME="$PROJECT_ROOT/.local/share"
export XDG_STATE_HOME="$PROJECT_ROOT/.local/state"
export CARGO_HOME="$PROJECT_ROOT/.cache/cargo"
export CARGO_TARGET_DIR="$PROJECT_ROOT/target"
mkdir -p "$CARGO_HOME" "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
