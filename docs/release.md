# CLI 发布与部署

项目交付 CLI。GitHub Actions 在 Linux x86_64、Windows x86_64、macOS x86_64 和
macOS arm64 上运行测试；推送与 `Cargo.toml` 版本一致的 `v*` tag 后，发布流程会
构建四个 CLI 压缩包，先完成 GitHub Release，再将同一 Linux amd64/arm64 镜像
推送到 GHCR 和 Docker Hub。
发布流程不携带 `config.yaml`、令牌缓存或历史数据库。
Linux 原生压缩包在 Ubuntu 22.04 上构建，运行机器仍需兼容的 glibc、OpenSSL 3
和系统 CA 证书；其他发行版可使用 Docker 镜像或从源码构建。

## 独立二进制

解压对应平台的压缩包，在自己的工作目录中复制 `config.example.yaml` 为
`config.yaml` 并填写本机凭据，然后运行：

```bash
./sdu-infohelper check-config
./sdu-infohelper auth login --trust-device
./sdu-infohelper query --json
./sdu-infohelper aircon --json
```

Windows 将程序名换为 `sdu-infohelper.exe`。CLI 默认使用当前工作目录的
`config.yaml` 和 `.local/electricity/history.sqlite3`。可以通过 `--config`、
`--history` 或环境变量 `SDU_INFOHELPER_CONFIG`、`SDU_INFOHELPER_HISTORY`
设置绝对路径。宿舍认证缓存默认相对配置文件存放。

Linux、macOS 和 Windows 可使用 [daemon](daemon.md) 的 install / start 管理原生用户服务，
通过 PushDeer 或 Webhook 提醒。旧 `watch` 也可作为前台常驻进程交由自己的服务管理器启动；
仓库内的 `scripts/monitor.sh` 是可选的 systemd 用户服务包装器，不包含在
独立二进制部署的必需步骤。`--notify-desktop` 仅适合装有 `notify-send` 的
Linux 桌面；服务器上查询结果、错误和提醒可由服务管理器采集标准输出与错误。

## Docker

`Dockerfile` 使用 Debian 构建和运行，并包含 CA 证书与 OpenSSL 运行库。
镜像以非 root 用户运行，工作目录为 `/data`。将可写的数据目录挂载到 `/data`，
使容器用户有权读写配置、自动生成的设备 ID、认证缓存和历史文件：

Docker Hub 仓库名为 `winstonmeursault/sdu-infohelper`。Docker Hub 要求仓库名
使用小写字母，所以镜像名不能写成 `SDU-InfoHelper`。以下示例使用 GHCR 镜像；
也可将镜像地址换成 `winstonmeursault/sdu-infohelper:v1.0.0`。

```bash
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$PWD/data:/data" ghcr.io/winstonmeursault/sdu-infohelper:v1.0.0 check-config
docker run --rm -it --user "$(id -u):$(id -g)" \
  -v "$PWD/data:/data" ghcr.io/winstonmeursault/sdu-infohelper:v1.0.0 auth login --trust-device
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$PWD/data:/data" ghcr.io/winstonmeursault/sdu-infohelper:v1.0.0 query --json
```

首次运行前在宿主机的 `data/config.yaml` 填好配置。自动生成的设备 ID 会写回
该配置，因此登录时挂载目录需可写。后台运行可交给 Docker Compose、systemd
或其他调度器；可将镜像命令设为 `daemon run` 启动常驻监控。
容器内不执行 daemon install；实例状态和历史随 `/data` 挂载持久化。

## 打 tag 前

1. 将 `Cargo.toml` 版本与 release tag 对齐，并更新本文的示例镜像 tag。
2. 运行 `bash scripts/check.sh`，确认 GitHub Actions 的四个平台测试通过。
3. 在目标网络上分别验证宿舍普通用电和空调余电查询；学校接口可能变化。
4. 在 Docker Hub 的 `winstonmeursault` 命名空间下创建公开的 `sdu-infohelper`
   仓库。GitHub 仓库已配置 `DOCKERHUB_USERNAME` Actions 变量；还需配置具备
   推送权限的 `DOCKERHUB_TOKEN` Secret，优先使用单独创建的访问令牌。
   本机的 `docker login` 不会自动传给 GitHub Actions。
5. 确认仓库允许 GitHub Actions 写入 GHCR 软件包与 GitHub Release。

推送 tag 将自动发布；正式发布前先审查 tag 指向的提交与 Actions 配置。
