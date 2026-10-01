# SDU-InfoHelper

SDU-InfoHelper 是一个 Rust CLI，用于查询山东大学（威海）宿舍普通用电和空调余电，并在本机保存历史、刷新认证凭据和运行低电量监控。当前版本为 **v1.1.0**。

## 功能

- 宿舍目录查询：校区、楼栋、楼层和房间。
- 宿舍余额查询：十进制电量、供电状态和 JSON 输出。
- 空调余额查询：独立的 CAS 会话和 HTML 页面解析。
- CAS 登录、受信设备、OAuth access/refresh token 缓存与轮换。
- `daemon` 常驻监控：PushDeer / Webhook 通知、冷却、重试、历史和状态。
- Linux systemd、macOS launchd、Windows 任务计划程序的当前用户服务管理。
- Linux、Windows、macOS CLI 压缩包，以及 amd64/arm64 Docker 镜像。

## 快速开始

需要 Rust 1.88+、C 编译器、OpenSSL 开发库和 `pkg-config`。项目脚本会把 Cargo 缓存和构建产物放在项目目录内。

```bash
bash scripts/build.sh
umask 077
cp config.example.yaml config.yaml
# 编辑 config.yaml，填写 cas.username、cas.password 和 dorm_electricity 目标
bash scripts/electricity.sh check-config
bash scripts/electricity.sh auth login --trust-device
bash scripts/electricity.sh query --json
```

也可以从 [GitHub Releases](https://github.com/WinstonMeursault/SDU-InfoHelper/releases) 下载对应平台的二进制。Windows 使用 `sdu-infohelper.exe`；直接运行二进制时，默认读取当前目录的 `config.yaml`。

配置文件中的密码、PushKey、Webhook 认证头和生成的令牌只保存在本机。不要把 `config.yaml`、`.local` 或令牌放入版本库、命令参数、日志或聊天记录。认证缓存默认写入 `.local/electricity/auth.json`，历史默认写入 `.local/electricity/history.sqlite3`；可以用 `--config`、`--history`、`SDU_INFOHELPER_CONFIG` 和 `SDU_INFOHELPER_HISTORY` 覆盖路径。

## 查询命令

目录返回的完整 `value`（包括 `&` 后的显示名）可以直接填入 `dorm_electricity`。也可以填写目录中唯一匹配的名称或数字，程序不会猜测不存在的房间 ID。

```bash
bash scripts/electricity.sh list campuses
bash scripts/electricity.sh list buildings --campus '校区value'
bash scripts/electricity.sh list floors --campus '校区value' --building '楼栋value'
bash scripts/electricity.sh list rooms --campus '校区value' --building '楼栋value' --floor '楼层value'
bash scripts/electricity.sh query --campus '校区value' --building '楼栋value' --floor '楼层value' --room '房间value'
bash scripts/electricity.sh aircon --json
bash scripts/electricity.sh history --limit 20
```

首次登录使用 `auth login --trust-device`；仅当本次登录需要验证码时显式加 `--sms`。后续查询会在到期前尝试刷新 token，刷新失败时按配置回退到 CAS。后台 daemon 不读取 stdin、不发送短信；出现二次验证时会进入 `needs_login`，请在本机完成登录后重启或等待下一轮。

## 常驻监控

在 `config.yaml` 的 `notifications.channels` 中启用至少一个 PushDeer 或 Webhook 渠道，然后注册当前用户服务：

```bash
./target/release/sdu-infohelper check-config
./target/release/sdu-infohelper daemon install --config "$PWD/config.yaml"
./target/release/sdu-infohelper daemon start --config "$PWD/config.yaml"
./target/release/sdu-infohelper daemon status --config "$PWD/config.yaml"
```

`daemon install --autostart` 开启登录自启动。修改监控参数或通知凭据后执行 `daemon restart`；`daemon stop` 停止服务，`daemon uninstall` 移除服务注册但保留配置、认证缓存、历史和日志。容器中使用 `daemon run`，由容器平台负责重启。

默认每 6 小时查询一次，电量严格低于 10 度时提醒，持续不足每 24 小时重复提醒。详细配置、事件格式、平台行为和迁移说明见 [对外 API](docs/api.md)。

## Docker

镜像以非 root 用户运行，数据目录为 `/data`。挂载可写目录后执行：

```bash
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD/data:/data" \
  ghcr.io/winstonmeursault/sdu-infohelper:v1.1.0 check-config
```

更多发布目标和 tag 流程见 [发布说明](docs/release.md)。

## 开发

```bash
bash scripts/check.sh
```

对外调用方式、daemon、通知和迁移说明见 [对外 API](docs/api.md)，学校系统的内部接口记录见 [学校系统接口记录](docs/school-interfaces.md)。项目使用 GPL-3.0-only，详见 [LICENSE](LICENSE)。
