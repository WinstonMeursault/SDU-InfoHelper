# 从空调查询 CLI 迁移

本版本新增宿舍普通用电查询、Rust 库 API 和认证管理，CLI 改用明确的子命令。
原空调查询命令按下表替换；宿舍电量使用独立的 `query`、`list` 和 `watch` 命令。

| 原命令 | 当前命令 |
| --- | --- |
| `cargo run` | `cargo run -- aircon` |
| `cargo run -- --json` | `cargo run -- aircon --json` |
| `cargo run -- --probe` | `cargo run -- auth probe --aircon` |
| `cargo run -- --check-config` | `cargo run -- check-config` |
| `cargo run -- --sms` | `cargo run -- aircon --sms` |
| `cargo run -- --trust-device` | `cargo run -- aircon --trust-device` |

`cas` 账号、密码和 `device_id` 配置继续使用原结构。已授信的 `device_id` 应保留。
`aircon.building`、`floor`、`room` 保持数字配置；查询前需填写完整有效目标。
`dorm_electricity` 现在实际启用，可接受目录的完整值或唯一匹配的名称/数字。
新增 `auth.cache` 指定独立令牌缓存，相对路径以配置文件所在目录为基准。
认证缓存现在记录 CAS 账号归属，避免切换账号后继续使用旧凭据。
已有缓存若没有 `cas_username` 且配置了 CAS 账号，查询时会重新通过 CAS 登录；
若需要二次验证，运行 `auth login --trust-device`。也可用当前账号的 OAuth 响应
运行 `auth import`；仅 Token 模式（CAS 账号留空）不受影响。

电量 JSON 的 `remaining_kwh` 改为十进制字符串，避免浮点舍入。
新的 `aircon --json` 返回 `building`、`floor`、`room` 和 `remaining_kwh`，
不再包含原输出的 `service` 字段。调用方可根据所调用的子命令区分服务。
原 `output_html` 字段目前不启用，不再自动导出登录后页面；配置中可删除该字段。

构建要求为 Rust 1.88+、C 编译器和 OpenSSL 开发库；项目脚本将缓存与产物放在项目内。
Linux 用户 systemd 监控是可选功能，其他平台可以使用 CLI 查询或调用 Rust 库。
原先遇到 macOS 27.0 SDK 与链接器不兼容的环境，可继续临时指定已有的 SDK：

```bash
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo run -- aircon
```
