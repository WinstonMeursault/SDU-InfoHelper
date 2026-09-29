# SDU-InfoHelper

山东大学威海校区空调余电查询的 Rust 客户端。通过统一身份认证和手机二次验证后，按配置的公寓、房间读取剩余电量；宿舍电费查询字段已预留，尚未实现。

## 使用

1. `cp config.example.yaml config.yaml`，在本地填入学号和统一身份认证密码。查询空调电费时同时填写 `aircon.building`（公寓号）和 `aircon.room`（房间号）；特殊房间可另填 `aircon.floor`。`dorm_electricity` 是后续宿舍电费查询的预留字段。`config.yaml` 已被 Git 忽略。
2. 运行 `cargo run -- --probe` 可在不读取凭据的情况下验证学校登录页与表单。随后运行 `cargo run`。如提示需要手机验证，在本机运行 `cargo run -- --sms`，程序会发送验证码并在终端读取，再继续登录。若你决定授信当前设备，可改用 `cargo run -- --trust-device`；学校可能自动解除最早一台受信设备。
   `cargo run -- --check-config` 只检查本地配置格式，不访问学校网站。
3. 程序会输出指定房间的剩余电量，并核对页面上的公寓、楼层、房间。若启用了 `output_html`，查余电页面会保存在指定路径。请勿公开该文件，它可能含有个人信息。

设备授信后可运行 `cargo run -- --json`，只输出一行机器可读的 JSON，例如 `{"service":"aircon","building":1,"floor":2,"room":201,"remaining_kwh":7.01}`。若登录再次要求二次验证，命令会报错，不会输出旧余额。

程序不会把密码写进日志。若学校要求短信、二维码或其他二次认证，当前命令会停止并提示；请不要把短信码或密码发到聊天中。

学校前端在正式提交前还会请求 `device` 接口检查设备并可能要求手机验证。命令行程序首次登录会在本地 `config.yaml` 生成独立的 `cas.device_id`；授信后请保留它。只有显式传入 `--sms` 或 `--trust-device` 才会发送短信，验证码不会保存。

余电通过只读 `GET /dianbiao/chongzhi.jsp` 查询，程序不调用生成订单接口。详细请求及未验证之处见 [空调余电协议记录](docs/aircon-protocol.md)。

原始链接中的 `jsessionid` 是一次性会话标识，本项目使用不含它的稳定入口：`https://gyktgd.wh.sdu.edu.cn/dianbiao/AuthServlet.se`。

当前机器的默认 macOS 27.0 SDK 与链接器不兼容，编译时需临时指定已有的 26.5 SDK：`SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo run`。
