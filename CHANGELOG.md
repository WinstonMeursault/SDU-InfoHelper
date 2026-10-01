# 更新记录

## v1.1.0 — 2026-09-30

新增跨平台常驻电量监控，可按配置周期查询一间宿舍，在剩余电量严格低于阈值时提醒。

- 支持 PushDeer 和通用 Webhook，可同时启用多个渠道，凭据保存在本机 config。
- 每个渠道独立冷却和有限重试，SQLite 保存发送状态；恢复电量后重新允许提醒。
- Linux systemd、macOS launchd、Windows 任务计划程序支持安装、启动、重启、停止、
  状态查询、卸载和显式开启登录自启动；也可使用 `daemon run` 前台运行。
- 提供通知测试、JSON 状态、实例锁和有界日志轮转。认证失效时提醒用户本机登录，
  后台不发送短信或等待验证码。
- 旧配置和 query / watch / auth 用法保持兼容；旧 watch 仍按“阈值及以下”判断。
- 将项目对外契约与学校系统记录分开整理为 [`docs/api.md`](docs/api.md) 和 [`docs/school-interfaces.md`](docs/school-interfaces.md)，删除重复的旧接口文档。

升级后在本机配置并启用至少一个通知渠道，按 [对外 API](docs/api.md) 的 daemon 部分注册服务。
容器使用 `daemon run`，由容器平台管理进程；配置、认证缓存和历史通过挂载目录持久化。

四个平台的格式、测试、clippy 和原生服务生命周期检查通过，Windows 检查控制台窗口不可见。
自启动设置已验证；实际重新登录触发与真实设备推送需在部署环境联调。
