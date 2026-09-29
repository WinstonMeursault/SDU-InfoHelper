#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/local-env.sh"
conda_bin="${CONDA_EXE:-$(command -v conda)}"
if [[ -f "$PROJECT_ROOT/.conda/conda-meta/history" ]]; then
    "$conda_bin" env update --prefix "$PROJECT_ROOT/.conda" --file "$PROJECT_ROOT/environment.yml"
else
    "$conda_bin" env create --prefix "$PROJECT_ROOT/.conda" --file "$PROJECT_ROOT/environment.yml" --yes
fi
