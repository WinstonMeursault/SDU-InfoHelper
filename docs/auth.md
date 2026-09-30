# 认证和刷新：证据、实现与验证边界

## 实际发现

2026-09-29，本地已有抓包只包含登录后的使用流量，没有发现 `refresh_token`。
访问 Token 与刷新 Token 是两个独立凭据，不能从已取得的 JWT 推导刷新 Token。

缴费应用的 `charge-app` 前端 JavaScript 明确包含以下逻辑：

- 登录响应读取 `access_token`、`refresh_token`、`expires_in`，记录登录时间。
- 到期前 300 秒发送表单 `grant_type=refresh_token&scope=all&refresh_token=...&logintype=...`。
- `pay` 客户端使用 `/blade-auth/oauth/token`；`attendance` 使用 `/berserker-auth/oauth/token`。
- 成功后更新访问 Token 与刷新 Token，支持服务端轮换刷新凭据。
- `mobile-app` 分支有自己的 Basic 客户端认证，但这段自动刷新条件不包含它。

因此能确认平台前端具备刷新能力，不能仅据此保证原生 App 的同一个 Token、
每个客户端或 CAS 登录都一定获发可用 refresh_token。
随后本分支经 CAS 登录实际取得了刷新凭据，并成功调用 Berserker 刷新接口；
刷新后的令牌再次查询宿舍电量成功。Blade 刷新路径来自前端代码，尚未实测该客户端。

## 宿舍平台统一认证入口

App 公开配置中的统一认证登录入口是：

```text
GET https://mcard.sdu.edu.cn/berserker-auth/cas/redirect/neusoft
  → https://pass.sdu.edu.cn/cas/oauth2.0/authorize
  → https://pass.sdu.edu.cn/cas/login?service=...
```

CAS OAuth 的 `redirect_uri` 指向 `/berserker-auth/cas/login/neusoft`。
匿名请求已验证这些跳转与动态登录表单；OAuth 会话状态、票据和授权码不可硬编码。
客户端从这一入口建立 Cookie 会话，完成母仓库同款 `device` 检查、短信验证和动态表单登录。
只允许 HTTPS 和学校认证、校园卡、空调服务的明确域名，保持证书校验。

本机实测 CAS 服务器协商 TLS 1.2 的 `ECDHE-RSA-AES128-SHA`，rustls 不支持该密码套件。
认证客户端沿用母仓库的 native-tls/OpenSSL；余额和 OAuth 请求继续使用 rustls。
不会关闭证书验证。Linux 构建使用已有的 OpenSSL 开发库与 pkg-config。

## 本地凭据生命周期

1. `config.yaml` 保存账号、密码和独立设备 ID，不打印凭据。
2. `auth login --trust-device` 首次授信；仅显式短信选项允许发送验证码，不保存短信码。
3. 正常 CAS 回调返回 `/plat/?name=loginTransit&ticket=...`。程序解析前端登录组件，
   按其逻辑调用 `/berserker-auth/oauth/token`：`grant_type=password`、`logintype=sso`、
   `scope=all`、`loginFrom=app`、`device_token=h5`；此处的 `username` 和 `password`
   均为一次性票据，不是统一认证密码。返回完整访问和刷新凭据。
4. Token 单独写入 `.local/electricity/auth.json`，原子替换、权限 600，记录本地 CAS 账号归属。
5. 有过期时间时，查询前提前 5 分钟尝试刷新；认证拒绝后续期并最多重试一次。
6. 需要 Basic 认证时，优先读取已缓存的值；否则从同一学校域名的网页资产中匹配 JWT 的 `client_id`。
7. 新响应中的 refresh_token 替换旧值；响应未提供新刷新凭据时保留旧值。
8. 明确的认证错误或无效刷新凭据触发正常 CAS 登录。网络故障、HTTP 408/429/5xx、
   暂时不可用或格式异常的刷新响应不会触发重新登录，不破坏缓存，后台在下一轮重试。
9. CAS 再次要求二次验证时停止并提示本机操作，后台不会发送短信或输出旧电量。

跨进程文件锁覆盖认证与更新，避免监控和手动查询同时消费并轮换同一个 refresh_token。
状态命令只输出是否缓存、是否有刷新凭据及到期时间，不输出 Token。
有 CAS 账号时，只使用该账号绑定的缓存；切换账号后，状态命令不再将旧缓存计为
当前账号的缓存，查询也不会使用或刷新旧账号凭据。刷新成功后保留账号归属。
旧版缓存缺少归属信息时，查询会重新通过配置的 CAS 账号登录以建立归属；
若需要二次验证，运行 `auth login --trust-device`。也可显式导入当前账号的
OAuth 响应。导入会将凭据绑定到当前配置的账号，请确认输入属于该账号。
CAS 账号未配置或留空时仍支持仅 Token 模式，不要求本地账号密码。
缓存里的 JWT 时间仅为未验签元数据，实际授权由学校服务端验证。

## 验证范围

本地测试覆盖刷新表单编码、Basic 头、新 Token 的轮换、非法授予、服务暂不可用、
限流/超时及异常响应、账号切换和仅 Token 导入、过期缓存保留、并发更新、
原子写入权限，以及 CAS 表单目标限制与学校加密算法。
2026-09-29 真实验证通过：匿名页面、首次账号设备检查、本机短信授信、
授信后的再次登录无需短信、一次性票据交换取得 access_token 与 refresh_token、
主动刷新以及刷新后宿舍电量查询。刷新前后均返回 35.67 度。
新凭据还成功列出楼栋/房间目录，并查询另一间房间；不绑定原始 App 抓包中的单一宿舍。
学校今后是否保持相同策略、刷新凭据的完整生命周期仍由服务端决定。
