# 学校系统接口记录

本文只记录 SDU-InfoHelper 访问的山东大学（威海）内部系统行为，包括地址、字段、认证流程和本地实测证据。它不是学校发布的稳定 API 规范，页面结构、动态参数、设备策略和权限范围都可能改变。本文不包含账号、令牌或真实房间号。

## 宿舍普通用电

余额请求：

    POST https://mcard.sdu.edu.cn/charge/feeitem/getThirdData
    Content-Type: application/x-www-form-urlencoded
    synjones-auth: bearer <TOKEN>

| 字段 | 内容 |
| --- | --- |
| feeitemid | 411，威海电控缴费项目 |
| type | IEC，查询剩余电量 |
| level | 4，已选定房间 |
| campus / building / floor / room | 目录接口返回的完整 value |

同一地址使用 type=select、level=0..3 逐级获取校区、楼栋、楼层和房间。目录值通常包含“编号&显示名称”，必须把完整 value 传给下一层或余额请求，不能仅凭显示名称拼接 ID。

### 响应

核心信息位于 map.showData.信息，例如：

    {"msg":"success","code":200,"map":{"showData":{"信息":"剩余电量为35.67度，供电状态：查询失败"}}}

客户端解析十进制电量并单独保存供电状态；供电状态异常不会把明确电量改成零。正式查询还核对校区、楼栋、楼层和房间，目标不匹配、字段缺失、非 JSON、HTTP 错误和业务错误都视为失败，不返回旧余额。

实测该接口每次返回一间房间的电量，尚未发现批量接口。省略 Cookie 和普通 Authorization 仍可查询；完全省略 synjones-auth 返回 HTTP 401。JWT 的 exp 只用于本地到期提示，实际有效性由服务端决定。

## 空调余电

空调服务使用独立 CAS 会话。登录前读取动态表单：

    GET https://pass.sdu.edu.cn/cas/login?service=...
    POST https://pass.sdu.edu.cn/cas/device
    POST https://pass.sdu.edu.cn/cas/login

/cas/device 的 m=1 检查账号和设备；需要二次验证时，m=2 发送验证码，m=3 提交验证码。参数 s=1 授信设备，s=0 只验证本次登录。登录表单使用动态 lt、execution、action，并提交页面脚本生成的 rsa、ul、pl、_eventId。rsa 使用学校页面的 des.js 算法，不能替换为标准 DES。

余额页面：

    GET https://gyktgd.wh.sdu.edu.cn/dianbiao/chongzhi.jsp?gongyu=<公寓号>&sushe=<房间号>&floor=<楼层号>

这是服务端渲染的 HTML 页面。客户端解析公寓、楼层、房间和“剩余电量”，核对目标房间后才输出结果。楼层未显式配置时由三位数房间号推断，例如 405 推断为 4 层。每次请求使用新的 CAS Cookie，链接中的 jsessionid 不能固定保存。

页面上的充电记录和充值订单使用其他地址，但余额查询不会调用：

- GET /dianbiao/XueshengChongdianChaxunServlet.se
- POST /dianbiao/ShengchengdingdanServlet.se

## 宿舍平台认证与 OAuth

宿舍平台入口：

    GET https://mcard.sdu.edu.cn/berserker-auth/cas/redirect/neusoft
      -> https://pass.sdu.edu.cn/cas/oauth2.0/authorize
      -> https://pass.sdu.edu.cn/cas/login?service=...

CAS 登录完成后，客户端使用一次性票据换取 OAuth 凭据。访问 Token 和刷新 Token 是两个独立凭据，不能从 JWT 推导刷新 Token。实测 Berserker 刷新路径：

    POST https://mcard.sdu.edu.cn/berserker-auth/oauth/token
    grant_type=refresh_token&scope=all&refresh_token=<TOKEN>&logintype=...

缴费网页还包含 /blade-auth/oauth/token 路径；该路径来自前端代码，当前未作为本项目默认路径实测。其他客户端的 Basic 认证不能直接推断为同一刷新规则。

## 实测和安全边界

- 只允许代码中的学校 HTTPS 域名，保持证书校验；不把学校请求 URL、动态票据或 Cookie 当作配置常量。
- 认证缓存原子写入并绑定 CAS 账号；刷新失败时保留旧缓存，明确授权失效才回退 CAS。
- 后台监控不读取 stdin、不发送短信、不等待验证码；需要二次验证时进入 needs_login。
- 2026-09-29 已实测宿舍目录、跨房间查询、CAS 首次授信、免短信再次登录、refresh_token 刷新及刷新后余额查询。
- 本地协议测试使用模拟 HTTP 服务和假凭据；真实登录和设备授信只在用户自己的环境进行。
