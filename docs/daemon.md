# 常驻电量监控

Daemon 监控 `dorm_electricity` 配置中的一间宿舍，启动时立即查询，随后按配置周期检查。
阈值单位为度，条件为严格低于阈值；余额等于阈值时不提醒。
旧 `watch` 命令仍采用“阈值及以下”规则，原有用法保持兼容。

## 配置与前台运行

先完成宿舍目录配置与登录，并在 `notifications.channels` 中至少启用一个渠道。
PushDeer 和 Webhook 配置见 [通知说明](notifications.md)。凭据只在本机配置。

```bash
./sdu-infohelper check-config --config /absolute/path/config.yaml
./sdu-infohelper auth login --config /absolute/path/config.yaml --trust-device
./sdu-infohelper daemon test-notification --config /absolute/path/config.yaml
./sdu-infohelper daemon run --config /absolute/path/config.yaml
```

Windows 使用 `sdu-infohelper.exe`。`--config` 可以放在 daemon 子命令前或后，
也可使用 `SDU_INFOHELPER_CONFIG`；默认仍读取当前工作目录的 `config.yaml`。

默认阈值 10 度，每 6 小时检查，持续不足时每 24 小时重复提醒。
可以通过 `--threshold`、`--interval`、`--repeat-after`、`--timeout` 覆盖本次运行参数，
周期单位为秒，最短 60 秒。通知参数及监控参数修改后需重新启动。

```bash
./sdu-infohelper daemon run --config /absolute/path/config.yaml --threshold 15 --interval 3600
./sdu-infohelper daemon status --config /absolute/path/config.yaml
./sdu-infohelper daemon status --config /absolute/path/config.yaml --json
./sdu-infohelper daemon stop --config /absolute/path/config.yaml --wait-seconds 30
./sdu-infohelper daemon test-notification --config /absolute/path/config.yaml --channel pushdeer-main
```

同一配置只能运行一个 daemon；普通 `query` 可以同时查询并写入历史。
前台可按 Ctrl+C 停止，另一个终端也可以执行 stop。
stop 默认最多等待 30 秒，`--wait-seconds` 允许 1 至 300 秒。
正在进行的网络操作由超时限制；等待超时会明确报告尚未停止。

## 状态、历史与日志

daemon 的 YAML 历史路径相对于配置文件目录。`--history` 或
`SDU_INFOHELPER_HISTORY` 的显式相对路径相对于启动时工作目录。
默认历史为配置目录下 `.local/electricity/history.sqlite3`。

```bash
./sdu-infohelper history --history /absolute/path/.local/electricity/history.sqlite3
```

每个配置有独立实例目录：`.local/electricity/daemon/<instance-id>/`。
status 会显示实际目录；其中保存原子更新的 `status.json`、运行锁、停止请求和 `daemon.log`。
日志每文件最多约 5 MiB，保留 3 个归档。不要删除正在使用的锁文件。

状态包含最近完成的查询时间、下次检查、最近成功读数和各渠道发送结果。
查询失败时保留最近成功读数作为历史展示，并同时显示失败状态；旧余额不参与本次预警判断。
进程被强制终止后，status 根据实际实例锁显示 terminated，不把遗留 PID 当成存活证据。

## 认证与推送

后台复用已有 Token 刷新和免交互 CAS 回退，不发送短信、不等待终端验证码。
需要二次验证时进入 `needs_login`，通过已配置渠道提醒，继续等待下一查询周期。
在本机运行 `auth login --trust-device` 完成验证后，下一周期尝试恢复。

PushDeer / Webhook 各自冷却和重试。余额恢复会清除低电量冷却；再次不足立即提醒。
重启保留冷却，启动先读取新余额，不会直接发送数据库中的旧预警。
测试通知是显式发送，不创建查询历史、不修改预警冷却。

电脑休眠和关机期间不能查询；恢复后只查询一次，不集中补查。
后台服务管理的完整接口及三个系统的注册方案见 [实现设计](daemon-design.md)。
当前实现进度也在设计文档中记录。
