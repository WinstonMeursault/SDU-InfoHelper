# 通知渠道

监控通知支持 PushDeer 和通用 Webhook。配置位于 `config.yaml` 的
`notifications.channels`，可以同时启用多个渠道；配置格式见
[`config.example.yaml`](../config.example.yaml)。

每个渠道有唯一 `id`、`type`、`enabled` 和 `timeout_seconds`。
`enabled` 默认 true，超时默认 10 秒（允许 1 至 300 秒）。
示例配置默认禁用渠道，填写本机凭据后开启。
渠道 id 最多 128 字节，仅允许字母、数字、点、横线和下划线。

## PushDeer

在 PushDeer 客户端注册设备并生成 Key，将 Key 填入 `pushkey`。
这里需要的是推送 PushKey，而非 PushDeer 登录 Token。
`endpoint` 默认 `https://api2.pushdeer.com`；自架版可设置服务器基础地址，
程序会在其后添加 `/message/push`。

```yaml
notifications:
  channels:
    - id: pushdeer-main
      type: pushdeer
      pushkey: "在本机填写"
      endpoint: "https://api2.pushdeer.com"
```

请求为 POST 表单，包含 `pushkey`、`text`、`desp` 和 `type=text`。
标题和宿舍信息 / 预警内容由程序生成。
成功需要 HTTP 2xx、业务 code 为 0，且返回至少一个成功推送结果。
空结果、全部失败和无效响应都不能进入成功冷却。
“服务已接受”不等于设备已经展示通知。

参考：[官方说明](https://www.pushdeer.com/official.html)、
[服务端返回结构](https://github.com/easychen/pushdeer/blob/main/api/app/Http/Controllers/PushDeerMessageController.php)。

## Webhook

Webhook 固定 POST JSON，HTTP 2xx 表示接收成功，不解析接收方自定义业务码。
可以通过 headers 配置认证，不能覆盖 Content-Type、Host 或 Content-Length。

```yaml
notifications:
  channels:
    - id: webhook-main
      type: webhook
      url: "https://example.com/alerts"
      headers:
        Authorization: "Bearer 在本机填写"
```

请求包含以下字段：

| 字段 | 类型 / 含义 |
| --- | --- |
| schema_version | 整数，当前为 1 |
| event | electricity.low_balance / electricity.auth_required / electricity.test |
| event_id | 逻辑事件标识，同一事件重试保持一致 |
| checked_at | RFC 3339 UTC 时间 |
| title / message | 标题与文本内容 |
| location | campus / building / floor / room；目录完整值，或 null |
| remaining_kwh / threshold_kwh | 精确十进制字符串；无读数事件为 null |

接收方可用 event_id 去重；网络超时可能造成重复投递。
特定平台需要签名或其他 JSON 格式时，应新增渠道适配。

## 配置和错误处理

地址支持 HTTP / HTTPS，禁止 URL 内嵌用户名密码和片段。
自动重定向禁用，避免凭据被转发到其他地址；需要直接配置最终接口地址。
HTTP 408 / 429 / 5xx、网络失败和超时归为可重试错误；认证或参数错误需要修复配置。
PushDeer 响应最大 64 KiB；服务端错误原文不写入日志。

PushKey、认证头和 URL 内的敏感参数仅存于本机配置，不通过命令参数传递，
不进入日志、状态或错误输出。`check-config` 只做本地校验，不发送通知。
后台调度、各渠道独立冷却与测试命令见 [daemon 设计](daemon-design.md)。
