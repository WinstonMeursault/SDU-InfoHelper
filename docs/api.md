# 对外 API

本文记录 SDU-InfoHelper 对用户、脚本和通知接收方公开的接口。山东大学系统的内部地址、字段和认证细节见 [学校系统接口记录](school-interfaces.md)。

## CLI

所有命令都由 `sdu-infohelper` 提供；源码运行时可将其替换为 `cargo run --`。配置默认读取当前目录的 `config.yaml`，历史默认写入 `.local/electricity/history.sqlite3`。可使用 `--config`、`--history`、`SDU_INFOHELPER_CONFIG` 和 `SDU_INFOHELPER_HISTORY` 覆盖路径。

| 命令 | 用途 |
| --- | --- |
| `check-config` | 只校验本地 YAML，不访问网络 |
| `auth probe` | 匿名检查 CAS 页面是否可达 |
| `auth login` / `renew` / `status` / `import` | 登录、刷新、查看和导入本地凭据 |
| `list campuses / buildings / floors / rooms` | 查询宿舍目录 |
| `query` | 查询宿舍普通用电 |
| `aircon` | 查询空调余电 |
| `history` | 读取本地历史 |
| `watch` | 前台定时查询，兼容旧提醒规则 |
| `daemon run / install / start / restart / stop / status / uninstall` | 运行和管理常驻监控 |
| `daemon test-notification` | 显式测试通知渠道 |

查询和目录命令支持 `--json`。CLI 不会把密码、Token、PushKey 或 Webhook 认证头作为命令行参数接收。

## JSON 输出

### 宿舍查询事件

`query --json` 输出一个事件对象。主要字段如下：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `checked_at` | RFC 3339 字符串 | 查询时间 |
| `location` | 对象或 null | `campus`、`building`、`floor`、`room` |
| `remaining_kwh` | 十进制字符串或 null | 剩余电量；失败时为 null |
| `supply_status` | 字符串或 null | 服务端返回的供电状态 |
| `low_balance` | 布尔值或 null | 当前调用方的阈值判断 |
| `threshold_kwh` | 十进制字符串或 null | 本次使用的阈值 |
| `error` | 字符串或 null | 脱敏后的错误 |

失败不会把电量写成零，也不会用旧读数伪装本次成功。

### 空调查询

`aircon --json` 输出：

```json
{
  "building": 1,
  "floor": 4,
  "room": 405,
  "remaining_kwh": "35.67"
}
```

### 目录选项

`list ... --json` 输出选项数组，每项包含显示名称和传给下一级查询的完整 `value`：

```json
[
  { "name": "示例楼栋", "value": "id&示例楼栋" }
]
```

调用方应保存并复用 `value`，不要从 `name` 猜测 ID。

### Daemon 状态

`daemon status --json` 输出服务注册、运行状态、最近查询、下次检查、最近成功读数和各通知渠道状态。状态中的错误已经脱敏，不能作为日志协议以外的稳定字段扩展。

## 配置接口

配置示例见 [config.example.yaml](../config.example.yaml)。主要公共配置段：

- `cas`：本机登录账号、密码和设备 ID。
- `auth.cache`：认证缓存路径。
- `dorm_electricity`：宿舍查询目标。
- `aircon`：空调公寓、楼层和房间。
- `daemon`：阈值、周期、超时和历史路径。
- `notifications.channels`：PushDeer / Webhook 渠道。

相对 `auth.cache` 路径以配置文件目录为基准；daemon 的 YAML 历史路径也以配置文件目录为基准。配置中的秘密只保存在本机。

`check-config` 校验所有配置段。认证和普通查询只校验其所用功能的配置，不会因通知 URL 或监控阈值的语义错误而被阻断；`daemon run` 校验最终监控参数及通知渠道，`daemon test-notification` 校验通知渠道。所有命令仍要求 YAML 字段结构有效。

## 通知 Webhook

启用 Webhook 后，程序向配置的 URL 发送 POST JSON。请求固定包含：

```json
{
  "schema_version": 1,
  "event": "electricity.low_balance",
  "event_id": "唯一事件 ID",
  "checked_at": "2026-09-30T04:00:00Z",
  "title": "宿舍电量不足",
  "message": "剩余 8.50 度，低于阈值 10 度。",
  "location": {
    "campus": "主校区",
    "building": "示例楼栋",
    "floor": "4",
    "room": "405"
  },
  "remaining_kwh": "8.50",
  "threshold_kwh": "10"
}
```

事件类型为：

- `electricity.low_balance`：有效读数低于 daemon 阈值。
- `electricity.auth_required`：需要用户在本机重新登录。
- `electricity.test`：显式执行 `daemon test-notification`。

`remaining_kwh` 和 `threshold_kwh` 在没有有效读数的事件中为 null。HTTP 2xx 表示 Webhook 接收成功；接收方可用 `event_id` 去重。网络重试可能导致同一事件重复投递。

PushDeer 的配置和错误处理见本页的“通知渠道”部分。

## 常驻监控

Daemon 监控 `dorm_electricity` 中的一间宿舍，启动时立即查询，随后按配置周期检查。Daemon 使用严格的“小于阈值”规则；旧 `watch` 命令仍使用“低于或等于阈值”，以保持兼容。

默认阈值为 10 度，每 6 小时检查，持续不足时每 24 小时重复提醒。可通过 `--threshold`、`--interval`、`--repeat-after` 和 `--timeout` 覆盖本次运行。修改配置或通知凭据后执行 `daemon restart`。

```bash
./sdu-infohelper daemon install --config /absolute/path/config.yaml
./sdu-infohelper daemon start --config /absolute/path/config.yaml
./sdu-infohelper daemon status --config /absolute/path/config.yaml --json
./sdu-infohelper daemon test-notification --config /absolute/path/config.yaml --channel webhook-main
./sdu-infohelper daemon stop --config /absolute/path/config.yaml
./sdu-infohelper daemon uninstall --config /absolute/path/config.yaml
```

`daemon install` 只注册当前用户服务，不立即启动；加上 `--autostart` 才启用登录自启动。Linux 使用 systemd 用户服务，macOS 使用 launchd LaunchAgent，Windows 使用当前用户任务计划程序。容器中使用 `daemon run`，由容器平台负责重启。同一配置只能运行一个实例；`stop` 支持优雅停止和有限等待，`uninstall` 保留配置、认证缓存、历史和日志。

状态和日志保存在配置目录下 `.local/electricity/daemon/<instance-id>/`，包括运行锁、状态文件、停止请求和有界日志。查询失败时 status 可以展示最近成功读数，但该旧读数不会参与本次预警判断。休眠或关机期间不查询，恢复后最多立即查询一次。

后台认证会复用已有 Token 刷新和免交互 CAS 回退，不发送短信、不读取终端输入。需要二次验证时进入 `needs_login`，完成 `auth login --trust-device` 后下一周期恢复。

认证缓存锁的等待期限与查询 `--timeout` 一致；等待超时作为暂时查询失败处理，不删除缓存、不触发登录失效提醒。后台等待锁时可以响应停止请求，取消的查询不生成历史记录或通知。已经开始的 HTTP 请求仍受其网络超时限制，`--timeout` 不表示整段多步认证流程的总时长。无超时参数的 `auth import` 最多等待认证锁 30 秒。

### 已实现的生命周期约定

- 重复执行 `install` 会更新本程序的服务注册；重复 `start` 不会创建第二个实例；重复 `stop` 和 `uninstall` 可以安全执行。
- 修改运行参数不会影响已运行进程，必须执行 `restart` 才会重新读取配置。
- 注册信息使用二进制、配置和工作目录的绝对路径，不包含凭据；路径中包含空格、中文或特殊字符也受支持。
- Linux 使用用户级 systemd；macOS 使用当前用户的 launchd 图形会话；Windows 任务使用当前用户会话、禁止并行实例、隐藏后台控制台并取消默认运行时长限制。三者都不需要管理员权限或保存登录密码。
- 服务管理器不可用时会明确报告未知/不可用状态，并提示使用 `daemon run`；不会误报为已停止。
- 配置错误会让实例进入 failed 并停止，避免管理器无限重启；`needs_login` 保持进程存活，避免认证失败造成重启风暴。
- 日志单文件约 5 MiB、保留 3 个归档；Unix 管理文件使用私有权限。状态、日志和错误会脱敏，不输出 PushKey、Authorization、Token 或原始响应。

### 调度、预警和持久化

启动后立即查询，后续使用单调时间调度；错过的周期不集中补查。配置或本地存储不可用时停止；普通网络失败只记录状态并在下一周期重试。查询失败保留最后成功读数供 status 展示，但不会产生低电量预警。

预警状态按 daemon 实例、事件类型、目标、规范化阈值和渠道 ID 区分。每个渠道独立保存 event_id、尝试次数、下次重试时间和脱敏错误；认证提醒使用独立事件。成功查询恢复到阈值及以上时清除低电量冷却和待发送任务；重启后必须先取得新读数，不能直接发送数据库中的旧余额。

## 通知渠道

通知配置位于 `notifications.channels`，可以同时启用多个渠道。每个渠道有唯一 `id`、`type`、`enabled` 和 `timeout_seconds`；超时为 1 至 300 秒，id 只允许字母、数字、点、横线和下划线。

PushDeer 配置使用 PushKey（不是登录 Token）：

```yaml
notifications:
  channels:
    - id: pushdeer-main
      type: pushdeer
      enabled: true
      pushkey: "在本机填写"
      endpoint: "https://api2.pushdeer.com"
      timeout_seconds: 10
```

PushDeer 使用 POST 表单发送 `pushkey`、`text`、`desp` 和 `type=text`；必须同时满足 HTTP 2xx、业务 code 为 0 且至少有一个成功结果才算成功。

Webhook 使用 POST JSON，地址支持 HTTP/HTTPS，禁止内嵌凭据、片段和自动重定向；自定义 headers 不能覆盖 Content-Type、Host 或 Content-Length。HTTP 2xx 表示接收成功，不解析接收方自定义业务码。

两个渠道分别维护冷却和重试。每轮最多尝试 3 次，默认退避 5 秒和 30 秒；网络错误、超时、408、429 和 5xx 可重试。只有渠道成功接收后才进入冷却；恢复到阈值及以上会清除低电量冷却。测试通知不会创建查询历史或修改预警冷却。

## 迁移

旧版空调 CLI 命令迁移如下：

| 旧命令 | 当前命令 |
| --- | --- |
| `cargo run` | `cargo run -- aircon` |
| `cargo run -- --json` | `cargo run -- aircon --json` |
| `cargo run -- --probe` | `cargo run -- auth probe --aircon` |
| `cargo run -- --check-config` | `cargo run -- check-config` |
| `cargo run -- --sms` | `cargo run -- aircon --sms` |
| `cargo run -- --trust-device` | `cargo run -- aircon --trust-device` |

`dorm_electricity`、`auth.cache`、OAuth 刷新和 `remaining_kwh` 十进制字符串属于当前配置和输出格式。旧缓存若没有账号归属信息，配置 CAS 账号后会重新登录绑定；Token-only 配置仍可使用。旧的 `output_html` 配置不再使用，可以删除。

## Rust 库

库 API 适合在 Rust 程序中复用宿舍查询：

```rust,no_run
use std::{path::Path, time::Duration};
use sdu_infohelper::{with_dorm_auth, query, QueryError};

fn main() -> Result<(), QueryError> {
    let reading = with_dorm_auth(
        Path::new("config.yaml"),
        Duration::from_secs(20),
        |request, client| query(request, client),
    )?;
    println!("{}", reading.remaining_kwh);
    Ok(())
}
```

`selection_options` 可查询宿舍目录，`aircon::query_config` 查询空调，`auth` 模块负责登录、缓存、状态和导入。认证缓存使用跨进程锁和原子写入；具体学校请求不属于稳定的对外 API，请勿在调用方硬编码学校内部 URL。

`settings::Settings::load` 保留全量读取和校验行为；`Settings::read` 仅读取配置结构，供调用方执行相应功能的校验，`Settings::validate` 可显式执行全量校验。原有根模块导出的查询、类型和历史函数仍可按原路径调用。
