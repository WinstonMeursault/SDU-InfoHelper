# SDU-InfoHelper

山大威海宿舍剩余电量查询与本地监控。Python 用于协议验证和抓包导入，
Rust 用于独立查询、定时监控、历史记录和低电量提醒。

## 快速开始

准备 Linux、已有 Miniforge 和 Rust 编译器。从项目目录执行：

```bash
bash scripts/setup.sh
bash scripts/run.sh python scripts/setup-android-tools.py
bash scripts/build.sh
```

首次使用按 [抓包流程](docs/first-capture.md) 登录自己的账号并查询一次宿舍电量，
然后导入实际保存的抓包文件：

```bash
bash scripts/run.sh python electricity.py import-capture captures/你的抓包文件.mitm
bash scripts/electricity.sh query
```

也可参考 [配置样例](examples/request.example.json) 手动创建
`.local/electricity/request.json`，将 Token 和宿舍参数替换为自己的有效值。
样例不包含可用凭据，真实配置应保留在本地。

## 正式查询与监控

复用已有 Rust 编译器；依赖下载到项目 `.cache/cargo/`，编译产物在 `target/`。

```bash
bash scripts/build.sh
bash scripts/electricity.sh query
bash scripts/electricity.sh query --json
bash scripts/electricity.sh history --limit 10
```

查询直接连接学校 HTTPS 接口，不依赖手机、USB、mitmweb 或环境中的 HTTP 代理。
每次查询把时间、宿舍、电量、供电状态及错误保存到 `.local/electricity/history.sqlite3`。
查询失败保留空电量，不会写成零。接口有时同时返回有效电量与供电状态“查询失败”，
两项分别保留。

## 选择其他宿舍

接口支持校区、楼栋、楼层、房间目录，并可用同一登录凭据指定房间查询。
以下命令默认使用本地配置的上级目录：

```bash
bash scripts/electricity.sh list campuses
bash scripts/electricity.sh list buildings
bash scripts/electricity.sh list floors
bash scripts/electricity.sh list rooms
```

目录返回“名称”和“参数值”；将返回的参数值完整复制到查询参数，包括其中的 `&`。
同一楼层选择另一个房间：

```bash
bash scripts/electricity.sh query --room '房间参数值' --json
```

跨楼栋时先列楼层，再列房间；查询时提供完整的下级参数：

```bash
bash scripts/electricity.sh list floors --building '楼栋参数值'
bash scripts/electricity.sh list rooms --building '楼栋参数值' --floor '楼层参数值'
bash scripts/electricity.sh query --building '楼栋参数值' --floor '楼层参数值' --room '房间参数值'
```

跨校区时额外传入 `--campus '校区参数值'`。各级目录命令支持 `--json`。
覆盖参数只作用于本次命令，不修改本地配置或正在运行的监控目标。
返回数据会核对四级宿舍信息，历史记录包含宿舍信息。旧版历史没有宿舍字段的记录显示“宿舍未记录”。
不同宿舍的提醒状态分别保存；也可通过 `watch` 的同名参数监控指定房间。

当前验证的是威海电控项目 `feeitemid=411`；列目录仍需有效登录。
接口返回剩余电量（度），每次查询一间房。尚未发现一次返回所有宿舍电量的批量接口。

## 后台监控

启动项目内后台监控，默认每 6 小时查询、剩余电量不高于 10 度时提醒：

```bash
bash scripts/monitor.sh start
```

查看状态：`bash scripts/monitor.sh status`。停止：`bash scripts/monitor.sh stop`。

调整参数，或开启 Linux 桌面通知：

```bash
bash scripts/monitor.sh start --interval 21600 --threshold 10 --notify-desktop
```

已有监控进程时 `start` 不重复启动。修改参数需要先 `stop` 再 `start`。
日志在 `.local/electricity/monitor.log`，默认低电量提醒最多每 24 小时一次；
电量恢复到阈值以上后会重新允许下一次低电量提醒。`--repeat-after` 可调整重复提醒间隔。
桌面通知使用已有 `notify-send`，适用于当前 Linux 桌面会话。
后台由当前用户的 systemd 临时服务 `sdu-infohelper-electricity.service` 托管，
适用于当前有用户 systemd 的 Linux 环境。临时服务元数据由 systemd 保存在运行时目录，
项目脚本与数据仍在本项目内。电脑需开机联网；休眠时查询暂停，重启后重新运行 `start`。

也可在前台运行，按 Ctrl+C 结束：

```bash
bash scripts/electricity.sh watch --interval 21600 --threshold 10
```

## 登录凭据与抓包导入

当前宿舍参数及 Token 仅存放在 `.local/electricity/request.json`，文件权限为 `600`。
该配置从本人的成功余额查询中导入。协议说明见 [已验证的协议](docs/protocol.md)。
JSON 查询输出中的 `token_expires_at_claim` 是从 Token 声明读取的时间，
服务端可提前撤销登录，以实际请求结果为准。

凭据失效时监控记录错误并停止，避免将登录问题当成缺电。
目前需要在 App 登录后重新抓取一次查询，再导入并启动监控：

```bash
bash scripts/run.sh python electricity.py import-capture captures/你的抓包文件.mitm
bash scripts/monitor.sh start
```

导入时自动选择最新的成功 `type=IEC` 查询，只保留查询所需的请求头和宿舍参数。
可加 `--expect 35.67` 核对某次页面数值；日后电量变化时填写当次实际值。
抓包导入只需 Python 工具，正式 Rust 查询不需要 Python 环境。

Python 原型仍可单独验证：

```bash
bash scripts/run.sh python electricity.py query --json
bash scripts/run.sh python -m unittest discover -s tests
bash scripts/run.sh cargo test --locked
```

若将二进制部署到其他目录，使用 `--config` 和 `--history` 显式指定本地文件路径，
并在目标网络验证一次查询。

正式流程从 [第一次抓包](docs/first-capture.md) 开始。启动并自动保存流量：

```bash
bash scripts/capture.sh
```

## 项目内环境（Linux）

复用已有 Miniforge；无需 `sudo`、`conda init`，也不向 base 环境安装依赖。

```bash
bash scripts/setup.sh
bash scripts/run.sh python scripts/setup-android-tools.py
```

`setup.sh` 使用 `$CONDA_EXE` 或 PATH 中的 `conda`，按照 `environment.yml`
在项目内创建 Python 3.12 环境，安装 Java 17；httpx 和 mitmproxy
统一交给该环境内的 pip 解析依赖，避免 Conda 与 pip 的版本约束冲突。
第二条命令从官方发布站点下载 JADX 和 Android Platform-Tools。
第一次安装需要联网；安装脚本可重复执行。

| 路径 | 内容 |
| --- | --- |
| `.conda/` | Miniforge 创建的项目环境，含 Python、Java、Python 包 |
| `.cache/` | Conda、pip 等缓存 |
| `.tools/` | JADX、adb 及下载包 |
| `.local/` | 抓包证书、工具配置、Android 用户数据 |
| `base.apk` | 待分析 APK |
| `analysis/`、`captures/` | 本地分析结果与抓包文件 |

以上本地目录和 APK 已加入 `.gitignore`。抓包记录可能含登录凭据，不应提交。
脚本通过进程环境变量指定路径，不修改 shell 启动文件或用户 Conda 配置；
同时设置 `CONDA_REGISTER_ENVS=false`，避免向用户环境列表注册。

adb 是一个例外：当前官方版本仍固定访问 `~/.android`。
如需将其数据留在项目内，可以在确认 `~/.android` 尚不存在时，
手动创建指向项目 `.local/android/` 的软链接。已有目录可能存有设备密钥，
应先检查并妥善保留；安装脚本不会自动创建或覆盖该链接。
迁移项目时同步处理对应链接。

## 使用

在项目目录执行，无需激活环境：

```bash
bash scripts/run.sh python --version
bash scripts/run.sh python your_script.py
bash scripts/run.sh jadx --version
bash scripts/run.sh adb version
```

通过统一入口运行工具，才能使用项目内的配置和缓存目录。
如 IDE 需要解释器，选择 `.conda/bin/python`。

反编译 APK（输出仅保留在本地）：

```bash
bash scripts/run.sh jadx -d analysis/jadx base.apk
```

启动抓包，代理监听 8080，管理页面仅监听本机 8081：

```bash
bash scripts/run.sh mitmweb --listen-host 0.0.0.0 --listen-port 8080 \
  --web-host 127.0.0.1 --web-port 8081 --no-web-open-browser
```

电脑浏览器打开 `http://127.0.0.1:8081`，按终端提示完成管理页面认证。
手机与电脑连接同一可互通的局域网，在手机 Wi-Fi 中设置 HTTP 代理为
电脑局域网 IP、端口 8080，再访问 `http://mitm.it` 安装手机所需的代理 CA。
CA 及私钥保存在项目的 `.local/mitmproxy/`，不要分享私钥。
App 是否信任用户证书需要实际验证；浏览器抓包成功不代表 App 一定成功。
结束后关闭手机代理，并退出抓包进程。

需要 USB 调试时，在手机开启并授权后运行：

```bash
bash scripts/run.sh adb devices
```

若出现 USB 权限问题，再根据本机发行版处理；安装脚本不修改系统 udev 规则。
