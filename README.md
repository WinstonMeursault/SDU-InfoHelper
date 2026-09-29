# SDU-InfoHelper

山大威海电量查询与本地监控，运行时只需要 Rust 编译出的一个程序。
提供统一身份认证、设备授信、宿舍普通用电目录与余额查询、OAuth 缓存与刷新，
以及独立的空调电量查询。

| 服务 | 查询方式 | 认证 |
| --- | --- | --- |
| 宿舍普通用电 | 校区 → 楼栋 → 楼层 → 房间；JSON 余额接口 | 缓存 Token → refresh_token → CAS 重新登录 |
| 空调用电 | 配置公寓、楼层、房间；HTML 余额页面 | CAS 登录和会话 Cookie |

2026-09-29 已实测宿舍目录、跨房间查询、CAS 首次授信、后续免短信登录，
并取得真实 refresh_token。主动刷新后再次读取宿舍电量成功。
空调查询基于既有实现和已记录协议，本次重构尚未用本机空调目标做真实查询。

## 安装和配置

使用已有 Rust 1.88+、C 编译器、OpenSSL 开发库及 pkg-config 构建。脚本把 Cargo 缓存放在 `.cache/cargo`、
编译结果放在 `target`，无需安装 Python、Miniforge、ADB 或代理到系统。

```bash
bash scripts/build.sh
umask 077
cp -n config.example.yaml config.yaml
# 用本机编辑器填写 cas.username / cas.password，以及需要的查询目标
bash scripts/electricity.sh check-config
bash scripts/electricity.sh auth probe
```

配置保留 `cas`、`aircon` 和 `dorm_electricity` 段，旧 CLI 命令和输出变化见
[迁移说明](docs/migration.md)。
`config.yaml`、`.local`、旧抓包与 APK 均被 Git 忽略。
账号密码只在本机配置；不要放到命令参数、日志或聊天里。

首次登录宿舍平台：

```bash
bash scripts/electricity.sh auth login --trust-device
bash scripts/electricity.sh auth status
# 可选：立即续期，用于确认刷新仍可用
bash scripts/electricity.sh auth renew
```

若学校要求验证码，程序在本机终端读取。`--trust-device` 请求授信这台电脑；
仅验证当次可用 `--sms`。学校可能在受信设备达到上限时解除最早一台设备。
后续保留自动生成的 `cas.device_id`，供统一认证再次登录时使用。
后台查询不会自动发送短信。

已有凭据可迁移，无需重新抓包：

```bash
bash scripts/electricity.sh auth import --input .local/electricity/request.json
```

`auth import` 也接受本地保存的 OAuth 响应 JSON，包含 `access_token`、
可选 `refresh_token` 和 `expires_in`；会保存完整刷新凭据，而非只保留访问 Token。
OAuth 客户端默认 `berserker`，缴费网页的另一种客户端可显式选 `--provider blade`。
必要时 `--client-auth-file` 指向保存 Basic 认证头的本地文件，不要把值写在命令行。
旧 `query --config .local/electricity/request.json` 仍可查询，但不启用自动续期。

## 宿舍目录和余额

```bash
bash scripts/electricity.sh list campuses
bash scripts/electricity.sh list buildings
bash scripts/electricity.sh list floors --campus '目录中的校区value' --building '目录中的楼栋value'
bash scripts/electricity.sh list rooms --campus '目录中的校区value' --building '目录中的楼栋value' --floor '目录中的楼层value'
bash scripts/electricity.sh query --json
```

把目录返回的完整 `value` 填入 `dorm_electricity`，包括 `&` 后的显示名。
也可填唯一匹配的目录名称；数字房间号只在服务器实际返回的目录能匹配时解析，
程序不会自行猜测房间 ID。只列目录时可以不配置完整宿舍目标。

单次查询另一宿舍可用 `query --campus ... --building ... --floor ... --room ...`，
不会改写默认监控目标。更换上级目录时需同时指定下级参数。
每次接口返回一间房间的电量，尚未发现所有宿舍电量的批量接口。
查询权限和范围仍以学校服务端为准。

程序核对响应中的校区、楼栋、楼层和房间，使用十进制电量。
供电状态与剩余电量分别记录；失败不会记录为零或返回旧余额。

## 监控和空调

```bash
bash scripts/monitor.sh start --interval 21600 --threshold 10
bash scripts/monitor.sh status
bash scripts/monitor.sh stop
bash scripts/electricity.sh history --limit 20
bash scripts/electricity.sh aircon --json
```

监控使用当前 Linux 用户的 systemd，默认每 6 小时查询一次、10 度及以下提醒。
历史、错误与提醒记录保存在 `.local/electricity/history.sqlite3`，
日志在 `.local/electricity/monitor.log`。可加 `--notify-desktop` 调用已有的 `notify-send`。
服务不默认配置开机启动。没有用户 systemd 时可运行 `electricity.sh watch` 前台监控。
空调查询需填写 `aircon.building` 和 `aircon.room`，楼层可显式指定。

## 可调用的 Rust API

CLI 的 `query --json` 和 `list ... --json` 可供脚本调用。
Rust 库的宿舍认证与查询入口：

```rust,no_run
use std::{path::Path, time::Duration};
use sdu_infohelper::{with_dorm_auth, query, QueryError};

fn main() -> Result<(), QueryError> {
    let reading = with_dorm_auth(
        Path::new("config.yaml"), Duration::from_secs(20),
        |request, client| query(request, client),
    )?;
    println!("{}", reading.remaining_kwh);
    Ok(())
}
```

`selection_options` 查询各级目录，`aircon::query_config` 查询空调，
`auth` 模块负责登录、缓存、状态和导入。认证操作使用跨进程锁，缓存原子写入且权限 600。
到期前 5 分钟尝试刷新；接口返回认证失效时续期后最多重试一次只读查询。
刷新响应提供新 refresh_token 时立即保存；无可用刷新凭据时尝试正常 CAS 登录。
学校撤销授权、刷新令牌到期或再次要求二次验证时仍需人工处理，不能保证永久免验证。

## 开发验证

```bash
source scripts/local-env.sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

本地分析工具、APK 和抓包属于调查资料，正常查询不会使用它们。
协议及验证边界见 [宿舍接口](docs/protocol.md)、[认证和刷新](docs/auth.md)
及 [空调接口](docs/aircon-protocol.md)。项目沿用 GPL-3.0。
