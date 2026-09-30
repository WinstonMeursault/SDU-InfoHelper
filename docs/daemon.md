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

## 原生后台服务

将可执行文件放在固定位置后注册服务。安装记录可执行文件和配置的绝对路径，
运行目录为配置所在目录；移动程序后重新 install，再 restart。

```bash
./sdu-infohelper daemon install --config /absolute/path/config.yaml
./sdu-infohelper daemon start --config /absolute/path/config.yaml
./sdu-infohelper daemon restart --config /absolute/path/config.yaml
./sdu-infohelper daemon stop --config /absolute/path/config.yaml
./sdu-infohelper daemon uninstall --config /absolute/path/config.yaml
```

install 不启动监控。只有 `install --autostart` 才启用当前用户登录后自动启动；
再次执行不带该选项的 install 会取消自启动。stop 停止当前运行，保留自启动设置；
uninstall 停止并移除服务，保留配置、认证缓存、历史和日志。
配置或通知凭据修改后执行 restart。安装时若设置 `SDU_INFOHELPER_HISTORY`，
会将解析后的绝对历史路径写入服务参数；重新 install 可更新这一覆盖。

| 系统 | 后台管理 | 要求 |
| --- | --- | --- |
| Linux | systemd 用户服务，`~/.config/systemd/user/` | 当前用户的 systemd 会话可用 |
| macOS | launchd LaunchAgent | 当前用户已登录图形会话；自启动定义位于 `~/Library/LaunchAgents/` |
| Windows | 当前用户任务计划程序 | 当前用户已登录；不保存登录密码，后台隐藏控制台 |

Linux 自启动跟随用户管理器启动；退出登录后是否继续由系统 linger 设置决定。
macOS 与 Windows 跟随登录会话，注销后不保证继续运行。
Windows 任务允许电池运行，取消默认运行时长限制，并禁止并行实例。
服务管理器不可用时命令会明确报错，可以使用 `daemon run` 交给已有进程管理工具。
容器中使用前台 run，由容器平台负责重启。

start 对已运行实例幂等。stop 先请求优雅退出，再停止系统服务并取消待重启；
必要时由系统管理器终止进程。后台遇到无效配置等确定性错误时停止并保留 failed 状态，
避免无限重启；修复后 start / restart 会重置启动预算。

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
status 同时显示实例锁与服务管理器状态；管理器不可访问时，服务状态为未知并附错误，
不会据此断言工作进程已经停止。`--json` 返回这两组信息供外部工具使用。
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
完整接口与验证范围见 [实现设计](daemon-design.md)。
