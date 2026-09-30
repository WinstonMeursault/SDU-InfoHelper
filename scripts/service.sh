#!/usr/bin/env bash
set -euo pipefail
umask 077
source "$(dirname -- "$0")/local-env.sh"
cd "$PROJECT_ROOT"
state_dir="$PROJECT_ROOT/.local/service"
binary="$PROJECT_ROOT/target/release/sdu-infohelper"
unit="sdu-infohelper-multi-user.service"
action="${1:-status}"
if [[ $# -gt 0 ]]; then shift; fi

if [[ "$action" == cli || "$action" == init ]]; then
    if [[ ! -x "$binary" ]]; then
        echo '请先执行 bash scripts/build.sh。' >&2
        exit 1
    fi
    if [[ "$action" == init ]]; then
        exec "$binary" service --data-dir "$state_dir" tick
    fi
    exec "$binary" service --data-dir "$state_dir" "$@"
fi

mkdir -p "$state_dir"
chmod 700 "$state_dir"
exec 9>"$PROJECT_ROOT/.local/service-manager.lock"
if ! flock -n 9; then
    echo '另一个服务管理操作正在执行，请稍后重试。' >&2
    exit 1
fi
running() { systemctl --user is-active --quiet "$unit"; }
case "$action" in
    start)
        if running; then
            printf '多用户服务已运行：%s。\n' "$unit"
            exit 0
        fi
        if [[ ! -x "$binary" ]]; then
            echo '请先执行 bash scripts/build.sh。' >&2
            exit 1
        fi
        if ! systemctl --user show-environment > /dev/null 2>&1; then
            echo '用户 systemd 不可用，请用 bash scripts/service.sh cli run 在前台运行。' >&2
            exit 1
        fi
        systemd-run --user --quiet --collect --service-type=exec --unit="$unit" \
            --description='SDU electricity multi-user worker' \
            --working-directory="$PROJECT_ROOT" \
            --property="StandardOutput=append:$state_dir/worker.log" \
            --property=StandardError=inherit --property=UMask=0077 \
            "$binary" service --data-dir "$state_dir" run "$@"
        sleep 1
        if ! running; then
            echo '多用户服务未能启动，请检查 .local/service/worker.log。' >&2
            exit 1
        fi
        printf '多用户服务已启动：%s；QQ 发送适配器尚未接入。\n' "$unit"
        ;;
    stop)
        if running; then systemctl --user stop "$unit"; fi
        echo '多用户服务已停止。'
        ;;
    status)
        if running; then
            worker_pid="$(systemctl --user show "$unit" --property=MainPID --value)"
            printf '多用户服务运行中，PID %s。\n' "$worker_pid"
        else
            echo '多用户服务未运行。'
        fi
        ;;
    *)
        echo '用法：bash scripts/service.sh init | cli <service 子命令> | start [run 参数] | stop | status' >&2
        exit 1
        ;;
esac
