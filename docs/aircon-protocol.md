# 空调余电查询协议记录

以下行为来自山东大学（威海）电费页面的实际 HTML、脚本和一次成功查询。服务端未提供公开的协议文档；字段和页面结构可能变化。

## 登录

1. `GET https://pass.sdu.edu.cn/cas/login?service=...`：读取动态 `lt`、`execution` 和表单 `action`。
2. `POST https://pass.sdu.edu.cn/cas/device`，`m=1`：前端先检查账号和设备。网页脚本还提交加密的账号、密码和设备指纹。已观察到 `bind`，表示要求二次验证；脚本还处理 `pass`、`binded`、`validErr` 等状态。
3. 若需手机验证，`POST /cas/device`，`m=2` 发送验证码；随后 `m=3` 提交验证码。`s=1` 表示“信任此设备”，`s=0` 表示不授信。已实测 `s=1` 后使用同一个本地设备标识再次登录可免短信；学校在受信设备达到上限时会自动解除最早一台设备的授信。
4. `POST` 登录表单：提交 `rsa`、`ul`、`pl`、`lt`、`execution`、`_eventId`。`rsa` 使用学校页面的 `des.js` 算法，不能直接替换成标准 DES。

## 查余电

登录后，页面的“充值/查余电”按钮导航到：

```text
GET https://gyktgd.wh.sdu.edu.cn/dianbiao/chongzhi.jsp?gongyu=<公寓号>&sushe=<房间号>&floor=<楼层号>
```

这是读取余额的请求，返回服务端渲染的 HTML。页面表格包含公寓、楼层、房间和“剩余电量”；未观察到浏览器另发 JSON 余额请求。客户端直接构造此 GET 请求，并核对响应中的房间信息后才输出余电。楼层默认从三位数房间号推断，也可在 `config.yaml` 中显式填写。

同页还有两个独立地址：

- `GET /dianbiao/XueshengChongdianChaxunServlet.se?...`：页面上的“充电记录”按钮使用；本项目尚未解析。
- `POST /dianbiao/ShengchengdingdanServlet.se`：生成充值订单；余电查询不调用。

每次请求使用新的 CAS 会话和 Cookie；原始链接中的 `jsessionid` 不能作为固定配置。若电费页面重定向回 CAS、字段缺失或房间不符，客户端会报错，不输出旧余额。
