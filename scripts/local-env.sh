#!/usr/bin/env bash
# Source from project scripts; changes apply only to their child processes.
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export CONDA_PKGS_DIRS="$PROJECT_ROOT/.cache/conda/pkgs"
export CONDA_ENVS_PATH="$PROJECT_ROOT/.local/conda/envs"
export CONDA_REGISTER_ENVS=false
export PIP_CACHE_DIR="$PROJECT_ROOT/.cache/pip"
export PYTHONNOUSERSITE=1
export XDG_CACHE_HOME="$PROJECT_ROOT/.cache"
export XDG_CONFIG_HOME="$PROJECT_ROOT/.local/config"
export XDG_DATA_HOME="$PROJECT_ROOT/.local/share"
export XDG_STATE_HOME="$PROJECT_ROOT/.local/state"
export JAVA_HOME="$PROJECT_ROOT/.conda/lib/jvm"
export ANDROID_USER_HOME="$PROJECT_ROOT/.local/android"
export ANDROID_SDK_HOME="$PROJECT_ROOT/.local"
export CARGO_HOME="$PROJECT_ROOT/.cache/cargo"
export CARGO_TARGET_DIR="$PROJECT_ROOT/target"
export PATH="$PROJECT_ROOT/.conda/bin:$PROJECT_ROOT/.tools/jadx/bin:$PROJECT_ROOT/.tools/platform-tools:$PATH"
mkdir -p "$CONDA_PKGS_DIRS" "$PIP_CACHE_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$ANDROID_USER_HOME"
