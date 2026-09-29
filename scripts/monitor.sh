#!/usr/bin/env bash
set -euo pipefail
umask 077
source "$(dirname -- "$0")/local-env.sh"
state_dir="$PROJECT_ROOT/.local/electricity"
mkdir -p "$state_dir"
binary="$PROJECT_ROOT/target/release/sdu-infohelper"
log_file="$state_dir/monitor.log"
unit="sdu-infohelper-electricity.service"
exec 9>"$state_dir/monitor.lock"
if ! flock -n 9; then
    echo '另一个监控管理操作正在执行，请稍后重试。' >&2
    exit 1
fi

monitor_running() {
    systemctl --user is-active --quiet "$unit"
}

action="${1:-status}"
if [[ $# -gt 0 ]]; then shift; fi
case "$action" in
    start)
        if monitor_running; then
            printf '监控已运行，服务 %s。\n' "$unit"
            exit 0
        fi
        if [[ ! -x "$binary" ]]; then
            echo '请先执行 bash scripts/build.sh。' >&2
            exit 1
        fi
        if ! systemctl --user show-environment > /dev/null 2>&1; then
            echo '当前用户的 systemd 不可用；可用 bash scripts/electricity.sh watch 在前台监控。' >&2
            exit 1
        fi
        systemd-run --user --quiet --collect --service-type=exec --unit="$unit" \
            --description='SDU Weihai dorm electricity monitor' \
            --working-directory="$PROJECT_ROOT" \
            --property="StandardOutput=append:$log_file" \
            --property=StandardError=inherit --property=UMask=0077 \
            "$binary" watch --config "$PROJECT_ROOT/config.yaml" \
            --history "$state_dir/history.sqlite3" "$@"
        sleep 1
        if ! monitor_running; then
            echo '监控未能启动，最近日志：' >&2
            tail -n 8 "$log_file" >&2
            exit 1
        fi
        printf '监控已启动，服务 %s；日志：%s\n' "$unit" "$log_file"
        ;;
    stop)
        if monitor_running; then
            systemctl --user stop "$unit"
            echo '监控已停止。'
        else
            echo '监控未运行。'
        fi
        ;;
    status)
        if monitor_running; then
            monitor_pid="$(systemctl --user show "$unit" --property=MainPID --value)"
            printf '监控运行中，PID %s；服务 %s。\n' "$monitor_pid" "$unit"
        else
            echo '监控未运行。'
        fi
        if [[ -f "$log_file" ]]; then tail -n 8 "$log_file"; fi
        ;;
    *)
        echo 'Usage: bash scripts/monitor.sh start [watch options] | stop | status' >&2
        exit 1
        ;;
esac
