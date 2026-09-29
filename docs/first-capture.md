# 第一次抓包：山大v卡通电费查询

本阶段目标：确认手机请求能到达电脑，保存一次自己的宿舍余额查询。
完成后再根据实际接口编写 Python 验证脚本。

## 已确认的信息

- APK：`base.apk`，应用名山大v卡通，版本 2.3.24。
- 包名：`com.synjones.xuepay.sdu`，target SDK 28。
- APK 使用 SecShell 加固，JADX 只能得到部分可见代码和资源。
- 网络配置允许明文 HTTP，但未显式信任用户 CA。
- 查询接口及认证方式已验证，见 [协议说明](protocol.md)。

## 1. 启动电脑代理

在项目目录的电脑终端执行：

```bash
bash scripts/capture.sh
```

保持终端运行。复制终端显示的带 token 的本机管理链接，在电脑浏览器打开。
浏览器管理页面是 8081；手机连接的是代理端口 8080。
脚本将请求保存到 `captures/session-日期时间-进程号.mitm`。

查看电脑 Wi-Fi 地址：

```bash
ip -brief -4 addr
```

选 Wi-Fi 或以太网接口的地址，不选 `127.0.0.1` 或 VPN/TUN 接口。
Wi-Fi 的接口名称和地址依电脑而异，重连网络后重新确认。

## 2. 设置手机代理

让手机与电脑处于可互相访问的局域网。编辑手机当前 Wi-Fi 的设置，
将代理改为“手动”，服务器填写电脑 Wi-Fi IPv4，端口填写 `8080`。
主机名填写地址本身，不加 `http://`。首次验证时关闭手机移动数据及 VPN，
避免请求从其他网络发送。

不同系统菜单位置有差异，可以在当前 Wi-Fi 的详情或修改网络界面查找“代理”。

## 3. 先验证浏览器

在手机浏览器输入 `http://mitm.it`，确认显示 mitmproxy 证书下载页面，
并且电脑管理页面出现请求。打不开时先检查 IP、端口、代理进程和局域网互通；
校园 Wi-Fi 可能隔离设备，此时可改用可互通的网络或下方 USB 转发。

### 手机提示代理地址错误或无法连接

先确认 Wi-Fi 手动代理的服务器只填写电脑地址，端口为 `8080`。
如果电脑本地代理测试成功而手机仍无法连接，可以用 USB 转发：

1. 用支持数据传输的 USB 线连接手机，开启开发者选项和 USB 调试，并在手机授权电脑。
2. 保持抓包终端运行，在另一个项目终端执行：

   ```bash
   bash scripts/run.sh adb devices
   bash scripts/run.sh adb reverse tcp:8080 tcp:8080
   ```

   设备状态应为 `device`；`unauthorized` 表示手机尚未授权，空列表表示尚未识别设备。
3. 手机仍连接 Wi-Fi，将手动代理服务器改成 `127.0.0.1`，端口保持 `8080`。
4. 重试 `http://mitm.it`。此时手机代理连接经 USB 转发到电脑，不依赖 Wi-Fi 设备互通。

结束时恢复手机代理为“无”，并运行
`bash scripts/run.sh adb reverse --remove tcp:8080` 移除本次转发。

ADB reverse 的官方命令说明：
<https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/docs/user/adb.1.md>

下载 Android CA 证书，在手机设置中搜索“安装证书”，选择“CA 证书”安装。
它应作为用户 CA，而不是 Wi-Fi 客户端证书安装。仅安装当前项目生成的公开 CA 证书。

随后在手机浏览器访问 `https://mitmproxy.org`。成功标准是电脑抓包页面能看到
完整 GET 请求和响应内容。只有连接事件不算成功。

mitmproxy 官方验证流程：
<https://docs.mitmproxy.org/stable/overview/getting-started/>

## 4. 记录一次电费查询

浏览器验证成功后：

1. 记录当前时间，重新打开山大v卡通。
2. 用自己的账号进入威海校区电费页面。
3. 选择自己的楼栋、房间，刷新或重新进入余额页面一次。
4. 记下 App 显示的余额/电量、单位和查询时间，便于与返回数据核对。
5. 在电脑管理页面观察对应时段新增的请求、响应和报错。

第一轮只需查询；无需执行充值操作，也无需为了抓包而注销账号。

如果 App 的 HTTPS 请求失败，终端可能出现 `client does not trust the proxy's certificate`
或 TLS 错误。这不代表查询脚本不可行。Android 的默认 CA 信任规则及本 APK 的配置
提示它可能拒绝用户证书；但具体请求仍需实测，应用可能另用自定义网络库或 HTTP。
仅凭此配置不能确定是否存在证书绑定，也不能确定所有接口行为。

Android 官方 CA 信任规则：
<https://developer.android.com/privacy-and-security/security-config#CustomTrust>

## 5. 保存并导入

在电脑终端按 Ctrl+C 正常结束抓包，将手机 Wi-Fi 代理恢复为“无”。
流量文件可能包含会话凭据，保留在本地 `captures/` 即可，不要贴出 Token 或密码。

将实际抓包文件导入配置，再独立查询并与 App 核对：

```bash
bash scripts/run.sh python electricity.py import-capture captures/你的抓包文件.mitm
bash scripts/electricity.sh query
```

如需核对特定数值，可在导入命令末尾加 `--expect 当次电量`。
未捕获到成功查询时不要导入其他支付请求；先根据连接或 TLS 错误检查抓包路径。
