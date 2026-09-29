#!/usr/bin/env bash
set -euo pipefail
umask 077
source "$(dirname -- "$0")/local-env.sh"
mkdir -p "$PROJECT_ROOT/captures"
capture_file="$PROJECT_ROOT/captures/session-$(date +%Y%m%d-%H%M%S)-$$.mitm"
printf '本次流量将保存到：%s\n' "$capture_file"
printf '手机代理端口：8080；电脑管理页面：http://127.0.0.1:8081\n'
printf '请按下方 mitmweb 日志中的带 token 链接打开管理页面。结束时按 Ctrl+C。\n'
exec bash "$PROJECT_ROOT/scripts/run.sh" mitmweb \
    --listen-host 0.0.0.0 --listen-port 8080 \
    --web-host 127.0.0.1 --web-port 8081 --no-web-open-browser \
    --set "save_stream_file=$capture_file" "$@"
