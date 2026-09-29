#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/local-env.sh"
if [[ ! -x "$PROJECT_ROOT/.conda/bin/python" ]]; then
    echo 'Run bash scripts/setup.sh first.' >&2
    exit 1
fi
if [[ $# -eq 0 ]]; then
    echo 'Usage: bash scripts/run.sh python|mitmweb|jadx|adb [arguments...]' >&2
    exit 1
fi
case "$1" in
    mitmweb|mitmproxy|mitmdump)
        tool="$1"
        shift
        exec "$PROJECT_ROOT/.conda/bin/$tool" --set "confdir=$PROJECT_ROOT/.local/mitmproxy" "$@"
        ;;
    jadx|jadx-gui)
        export JAVA_OPTS="${JAVA_OPTS:-} -Duser.home=$PROJECT_ROOT/.local"
        ;;
esac
exec "$@"
