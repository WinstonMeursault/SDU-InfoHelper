# Daemon 实现设计

状态：实现前设计。日期：2026-09-30。

本文记录已确认需求、首版默认约定、实现边界和验收条件。当前信息足以开始实现；真实推送凭据仅用于用户本机配置与最终联调，不是开发前置条件。

## 1. 已确认需求与首版边界

| 项目 | 约定 | 来源 |
| --- | --- | --- |
| 监控指标 | 剩余电量，单位 kWh（度） | 用户确认 |
| 预警条件 | 剩余电量严格小于配置阈值 | 原始需求及讨论方案 |
| 检查周期 | 用户配置，以秒为单位 | 用户需求 |
| 推送渠道 | 首版同时实现 PushDeer、通用 Webhook | 用户确认 |
| 凭据位置 | PushKey 与 Webhook 认证信息放在 `config.yaml` | 用户确认及讨论方案 |
| 扩展能力 | 统一通知接口，配置支持多个渠道 | 用户需求 |
| 跨平台 | Linux、macOS、Windows | 用户需求 |
| 监控目标 | 一个配置文件中的一个宿舍普通用电目标 | 首版默认约定 |
| 默认周期 | 检查 6 小时、持续不足时每 24 小时重复提醒 | 沿用现有 `watch` 默认值 |
| 默认阈值 | 10 度 | 沿用现有默认值，比较边界见第 4 节 |
| 自启动 | 显式开启；安装与立即启动分开 | 首版默认约定 |
| 配置生效 | 监控参数及通知渠道通过 restart 生效 | 首版默认约定 |

首版不包含人民币电费换算、空调用电监控、多宿舍任务列表、桌面通知新渠道、任意消息模板或动态插件加载。现有空调查询、单次查询及 Linux 桌面提醒仍保留。

## 2. 当前代码基础

- `src/main.rs` 的 `Watch` 已实现定时查询、历史保存、阈值提醒与恢复后清除冷却。
- `src/lib.rs` 已提供 `Event`、SQLite `readings` / `alert_state`、十进制电量解析和提醒状态函数。
- `src/auth.rs` 已实现认证续期、缓存原子写入和跨进程认证锁。
- `src/settings.rs` 已读取统一 YAML，但尚无 daemon 和通知配置。
- `scripts/monitor.sh` 仅包装 Linux systemd 用户服务；`notify-send` 仅用于已有 Linux 桌面提醒。
- CI 已覆盖 Linux x86_64、Windows x86_64、macOS x86_64 / arm64。

需要注意：现有 `Event::success` 的判断为 `<=`，`watch` 遇到认证类错误会退出，当前历史默认路径相对于工作目录。新功能必须显式处理这些差异。

## 3. 模块与依赖

建议按职责拆分，具体文件布局可随实现调整：

```text
src/settings.rs              配置读取、校验、路径解析
src/monitor.rs               单次监控、调度、阈值与状态转换
src/notification/mod.rs      Alert / Notifier / 发送结果及分发
src/notification/pushdeer.rs PushDeer HTTP 适配
src/notification/webhook.rs  通用 Webhook HTTP 适配
src/daemon/mod.rs            实例身份、锁、状态、停止和日志
src/daemon/platform/         Linux / macOS / Windows 服务管理
src/main.rs                  CLI 参数解析与调用
```

监控核心输出统一 `Alert`，不依赖任何渠道协议。渠道实现提供类似 `send(&Alert) -> Result<DeliveryReceipt, NotifyError>` 的接口；错误区分可重试和不可重试，并提供脱敏后的原因。使用编译时注册的实现，增加新渠道不需要修改阈值逻辑。

复用现有 `reqwest::blocking`、`rust_decimal`、`rusqlite`、`serde` 和 `fs2`。首版无需为了低频单目标查询引入完整异步运行时；调度必须可以中断等待。确需新增的平台或信号处理依赖，保持 Rust 1.88 最低版本及四个平台编译兼容。

旧 `watch` 可逐步复用查询与阈值策略，但保留原有 CLI 和提醒行为；不能为了共享实现直接改变公共 `Event::success` 的含义。

## 4. 配置契约

```yaml
daemon:
  threshold_kwh: "10"
  interval_seconds: 21600
  repeat_after_seconds: 86400
  query_timeout_seconds: 20
  history: .local/electricity/history.sqlite3

notifications:
  channels:
    - id: pushdeer-main
      type: pushdeer
      enabled: true
      pushkey: "在本机填写"
      endpoint: "https://api2.pushdeer.com"
      timeout_seconds: 10

    - id: webhook-main
      type: webhook
      enabled: true
      url: "https://example.com/alerts"
      headers:
        Authorization: "Bearer 在本机填写"
      timeout_seconds: 10
```

PushDeer 发送消息使用的凭据叫 PushKey；配置字段采用 `pushkey`，不与服务登录 Token 混用。

校验规则：

- 阈值使用精确十进制，推荐 YAML 字符串形式；允许零阈值，不接受负阈值。
- 检查周期和重复提醒周期为整数秒，范围沿用现有 CLI 的 60 至 31,536,000 秒。
- 网络超时为 1 至 300 秒。
- 渠道 `id` 非空且唯一；`type` 首版只接受 `pushdeer`、`webhook`。
- `enabled` 默认 true；禁用渠道可以不填写凭据，但字段结构仍须有效。
- 启用的 PushDeer 渠道必须有非空 PushKey；启用的 Webhook 渠道必须有有效 URL。
- 地址只支持 HTTP / HTTPS，禁止 URL 内嵌用户名密码；认证通过配置字段或 headers 提供。
- Webhook 固定使用 `application/json`；自定义 headers 不得覆盖 Content-Type、Host 或 Content-Length。
- 新增配置段内拒绝未知字段和未知渠道类型，避免拼写错误被静默忽略；不对已有顶层配置追加全局未知字段限制。
- 旧配置没有新增段时，已有命令继续有效。`daemon run` / `install` 要求至少一个启用渠道，避免后台监控只有日志而无推送。
- `check-config` 校验新增段但不访问网络、不发送通知。

daemon 参数采用：显式 CLI 参数 > 对应已有环境变量（配置和历史路径）> YAML 配置 > 默认值。CLI 数值覆盖参数至少提供 `--threshold`、`--interval`、`--repeat-after`、`--timeout`；推送凭据不提供命令行参数。

旧 `query` / `watch` / Rust API 保持 `<=`；新 daemon 使用 `<`，恢复条件为 `>=`。通过显式比较策略共享代码，并分别覆盖边界测试。历史中 `low_balance` 表示产生该记录的调用方所采用策略的结果。

daemon 监控参数与通知配置在启动时读取，修改后执行 restart。现有认证流程仍需要按次读取配置和缓存；账号或宿舍目标修改后也建议 restart，且下次查询不得混用前一目标的预警状态。

## 5. CLI 与生命周期

所有 daemon 子命令支持 `--config`，保持现有配置文件环境变量和默认选择方式。

| 命令 | 行为 |
| --- | --- |
| `daemon run` | 前台运行；也是系统管理器启动的实际工作进程 |
| `daemon install [--autostart]` | 校验配置并注册当前用户后台任务；不立即运行 |
| `daemon start` | 启动已注册任务；未安装时给出 install 指引 |
| `daemon stop` | 请求优雅停止并等待结果；必要时由系统管理器终止 |
| `daemon restart` | 停止后启动，重新读取配置 |
| `daemon status [--json]` | 显示安装、自启动、运行和监控状态 |
| `daemon uninstall` | 停止并移除服务注册，保留配置、历史和日志 |
| `daemon test-notification [--channel ID]` | 指定单个启用渠道或全部启用渠道，显式发送测试消息 |

管理命令采用幂等语义：重复安装更新本程序的注册，重复启动已运行实例不产生第二实例，重复停止或卸载成功返回。更新正在运行服务的启动参数时，应明确提示需 restart，不能报告参数已经应用。

测试通知使用独立事件，不改变低电量预警冷却；输出各渠道结果，任一测试失败返回非零退出码。

## 6. 跨平台后台管理

| 平台 | 管理器 | 注册与运行约定 |
| --- | --- | --- |
| Linux | systemd 用户服务 | 安装用户级 unit；只有 `--autostart` 才 enable |
| macOS | launchd LaunchAgent | 非自启动 plist 存在本程序状态目录；开启自启动才部署到用户 LaunchAgents 目录 |
| Windows | 当前用户任务计划程序 | 手动任务可启动常驻 worker；只有 `--autostart` 才添加当前用户登录触发器 |

注册信息包含二进制、配置及运行路径的绝对位置，不携带凭据。实现使用参数数组 / 正确 XML 或 plist 编码，不拼接带用户输入的 shell 命令。目录和程序名包含空格、中文时必须正常运行。

Linux 无用户 systemd 时，管理命令明确说明不可用，仍支持 `daemon run`。容器同样以前台 run 交给外部进程管理器。

macOS 使用当前用户的适当 launchd domain；关闭自启动时不能通过无条件 KeepAlive 导致下次登录自动启动。后台进程本身不执行 Unix 双重 fork。

Windows 设置禁止并行实例，取消默认 72 小时运行上限，并显式处理电池运行 / 切换电池自动停止策略。后台启动须隐藏控制台窗口，同时保持现有交互 CLI 的终端输出可用。注册默认使用当前用户会话，不保存 Windows 登录密码。

进程意外退出可以由管理器有限重启；启动配置错误须避免反复重启。`needs_login` 保持进程存活，防止认证失败造成重启风暴。主动 stop 必须停止服务且不会被立即自动拉起。

登录自启动不同于无人登录时的系统服务。首版不提供需要管理员权限的系统级服务安装；电脑休眠和关机期间不查询、不主动唤醒。平台状态获取失败时报告“未知 / 管理器不可用”，不能误报“已停止”。

## 7. 查询调度和认证状态

启动完成后立即查询。正常周期以本轮查询开始时间计算，使用单调时间调度，避免系统时间调整影响等待。查询和发送顺序执行，单实例最多只有一个查询；超过周期则跳过错过的时点，不集中补查。系统恢复运行后如已到期，仅立即查询一次。

如果平台单调时钟不计入休眠，调度还需结合持久化的墙钟检查时间判断恢复后是否逾期。时间异常时执行一次查询后重建周期，不补发历史预警。

| 状态 | 处理 |
| --- | --- |
| 正常 / 低电量 | 保存本次成功读数，判断是否需要按渠道推送 |
| 查询暂时失败 | 保存失败记录；保留最后成功读数用于 status 展示，但不用于预警判断；下一周期重试 |
| `needs_login` | 推送一次登录失效提醒，按冷却重复；保持进程存活，下一周期尝试恢复 |
| 配置或本地存储不可用 | 明确报告错误并停止，避免运行但无法记录状态 |
| 停止中 | 取消等待和待发送任务，等待当前有超时限制的操作结束，释放锁 |

后台认证必须显式禁止交互：不读取 stdin，不自动发送短信或请求设备授信。遇到验证码、二次验证、缺少凭据等情况进入需要用户处理的状态；保留已有正常续期和免交互 CAS 回退。

成功查询后退出 `needs_login` 并清除该事件的冷却。认证恢复不另外推送恢复消息。普通网络失败首版只写日志及状态，不增加一种每次查询失败都推送的事件。

## 8. 统一通知事件与协议

Webhook 固定 POST JSON。首版事件类型：

- `electricity.low_balance`：有效读数低于阈值。
- `electricity.auth_required`：查询认证需要用户处理。
- `electricity.test`：用户显式测试。

JSON 契约示例：

```json
{
  "schema_version": 1,
  "event": "electricity.low_balance",
  "event_id": "一次逻辑预警的唯一标识",
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

时间统一使用 RFC 3339 UTC；电量字段使用字符串以保留十进制精度。认证和测试事件没有有效电量，相关字段为 null；目标尚未解析时 location 也可为 null。位置字段沿用当前完整目录值，接收方不要假定它只是显示名称。未经脱敏的认证错误、账号、Token、请求头及原始响应不出现在推送中。

同一逻辑事件的所有渠道和重试沿用 event_id；下一次持续不足提醒生成新的 event_id。接收端可用 event_id 去重。网络超时可能导致重复投递，首版不承诺恰好一次交付。

PushDeer 将 title / message 映射到 `text` / `desp`，显式指定消息类型，通过 POST 表单发送 `pushkey`。复用项目已有 HTTP 客户端基础，但推送使用独立、无学校 Cookie 的客户端。

Webhook 的任意 HTTP 2xx 表示接收方接受；不解析其自定义业务返回码。PushDeer 需同时校验 HTTP 状态、业务返回码和推送结果；空结果或明确的全部失败不能记为成功。状态表述为“推送服务已接受”，不承诺设备已展示。使用受限大小的响应解析，不能将整个服务端响应写入日志。

发送请求默认禁止自动重定向，避免认证信息被转发到另一地址。协议不要求本机桌面会话。特定平台的签名规则、自定义负载等需求以后通过新渠道实现扩展。

## 9. 冷却、重试和持久化

预警状态按“daemon 实例 + 事件类型 + 目标 + 规范化阈值 + 渠道 id”区分；认证事件无需电量阈值。只有该渠道成功接受消息才更新 `last_sent`，写日志不算推送成功。

复用现有 `alert_state` 保存成功冷却，但新 topic 带版本命名空间，避免与旧 watch 状态相互影响。增加独立发送状态表，保存 event_id、尝试次数、下一次尝试时间和脱敏错误；不保存请求凭据。SQLite 升级必须兼容已有数据库及并发单次查询。

默认重试规则属于实现默认值，首版不再增加配置项：

1. 同一轮预警最多 3 次尝试（含首次），退避等待分别为 5 秒和 30 秒。
2. 网络连接错误、超时、HTTP 408 / 429 / 5xx 可重试；429 可参考有效 Retry-After，但不得延迟到事件已经失去有效性之后。
3. 明确的凭据 / 参数错误（例如 400 / 401 / 403）不紧密重试，记录渠道错误，等待配置修复与 restart。
4. 预算耗尽后，等待下次有效查询再决定是否开启一轮发送尝试；不因每秒调度而无限发送。
5. 每次重试前，如果检查周期已到，优先查询；恢复、认证失败或无法获得有效新读数时取消待发送低电量事件。发送仅基于本轮仍有效的成功读数。
6. 一个渠道失败不阻止其他渠道首次尝试；各渠道轮流执行到期任务，不在某一渠道的退避等待内阻塞全部渠道。

有效读数恢复至阈值及以上时，清除该目标与阈值的低电量冷却和待发送任务。下一次低于阈值立即预警。重启后保留成功冷却及重试预算；启动先查新读数，再决定是否恢复待发送事件，不直接投递持久化的旧余额。

## 10. 实例、路径和停止

配置文件在启动 / 注册时解析为规范化绝对路径，据此生成稳定的实例标识和系统服务名称。同一配置只能有一个 daemon；不同配置可以各自运行。

daemon 的 YAML 相对历史路径以配置文件目录为基准。显式 CLI / 环境变量历史路径保留现有工作目录解析习惯，并在注册时固定为绝对路径。默认历史仍为配置目录下 `.local/electricity/history.sqlite3`；旧命令默认路径不改变。

每个实例的管理文件固定放在配置目录下 `.local/electricity/daemon/<instance-id>/`，与所选历史文件无关，包含锁、脱敏状态、日志及管理器注册元数据。变更历史路径不能绕过实例锁。

运行期间持续持有非阻塞独占文件锁，进程退出后操作系统释放；不通过删除锁文件判断存活。认证锁与 daemon 锁职责不同，普通 query 仍可与 daemon 同时执行。

status 结合管理器信息、实例锁和原子写入状态文件判断运行情况，不能只看 PID 或遗留状态文件。状态包括最近尝试、最近成功、最后成功余额及时间、下次检查、认证状态、各渠道发送状态和错误。旧余额必须明确标注时间及当前查询失败状态。

停止提供跨平台本地控制请求；请求绑定当前运行实例的随机标识，避免遗留停止请求终止下一次运行。等待过程最多每秒检查一次停止请求，并在 Unix 上处理 SIGINT / SIGTERM。网络操作由超时限制；stop 超时后可调用管理器终止，并如实报告是否正常停止。

日志默认写入每实例日志，采用有界轮转，例如每文件 5 MiB、保留 3 个归档。Unix 私有文件权限 600；Windows 放在当前用户目录并沿用适当访问权限。新增凭据类型不实现可打印秘密的 Debug；HTTP 错误、URL、响应和 headers 均经脱敏。

## 11. 验证与验收

开发测试使用本地模拟 HTTP 服务与临时配置，不调用学校线上服务，不发送真实 PushDeer / Webhook 消息，不使用真实账号和 Token。

核心验收：

- `<` / `=` / `>` 边界、精确小数、零与负电量；旧 watch 的 `<=` 保持兼容。
- 首次不足、持续不足、冷却到期、充值恢复、再次不足、重启后冷却保留。
- 查询失败不能产生低电量预警，已有 pending 事件不会在余额未知或恢复后错误重发。
- 多渠道成功 / 失败互不影响，固定预算重试和 event_id 去重契约成立。
- PushDeer 表单编码及业务失败解析；Webhook JSON、headers、超时、限流、重定向与非 2xx 行为。
- 无效配置、缺少凭据、未知字段、重复渠道 id、禁用渠道和旧配置兼容。
- 同配置重复启动、锁释放、遗留状态、停止请求和原子状态更新。
- 非交互认证失效不会读 stdin、发送短信、无限重启或记录为余额零。
- 配置 / 二进制路径包含空格和中文，后台运行不依赖启动工作目录。
- 新历史表迁移不破坏原数据，并允许单次 query 并发保存历史。
- 日志、状态、错误与测试输出不包含 PushKey、Authorization 或学校认证秘密。

平台验收分两层：CI 四平台编译、单元 / CLI 集成测试及服务配置生成测试；真实登录会话中验证 install / start / stop / restart / status / uninstall、自启动以及 Windows 隐藏窗口。没有真实平台验证的项目必须明确标记，不能把编译通过等同于后台生命周期验证通过。

完成代码后运行现有 fmt / test / clippy 检查，补充 README、配置示例及发布说明。真实学校查询与真实推送仅在用户本机凭据配置后联调，测试通知必须通过显式命令执行。

## 12. 实现顺序

1. 配置结构与校验、统一通知事件和接口、PushDeer / Webhook 适配及模拟服务测试。
2. 抽取单次监控与比较策略、调度、认证状态、SQLite 冷却和发送重试。
3. 完成 `daemon run`、实例锁、状态、可中断停止和日志轮转。
4. 实现三个平台的服务管理适配、CLI 管理命令和通知测试命令。
5. 执行跨平台检查、生命周期验证，更新用户文档与配置示例。

阶段完成不代表整体任务完成；三平台后台管理和两个通知渠道都属于本次范围。

## 13. 参考资料

- [PushDeer 官方仓库与接口说明](https://github.com/easychen/pushdeer)
- [PushDeer 官方 POST 示例](https://www.pushdeer.com/official.html)
- [PushDeer 推送服务实现及结果格式](https://github.com/easychen/pushdeer/blob/main/api/app/Http/Controllers/PushDeerMessageController.php)
- [Apple：Creating Launch Daemons and Agents](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html)
- [Microsoft：LogonTrigger](https://learn.microsoft.com/en-us/windows/win32/taskschd/logontrigger)
- [Microsoft：TaskSettings.ExecutionTimeLimit](https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings-executiontimelimit)
