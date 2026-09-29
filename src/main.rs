use anyhow::{bail, Context, Result};
use md5::{Digest, Md5};
use rand::{rngs::OsRng, RngCore};
use reqwest::blocking::Client;
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;
use std::{collections::HashMap, fs, io, io::Write, path::Path};
use url::Url;

mod cas_des;
use cas_des::encrypt as cas_encrypt;

const SERVICE: &str = "https://gyktgd.wh.sdu.edu.cn/dianbiao/AuthServlet.se";
const CAS_LOGIN: &str = "https://pass.sdu.edu.cn/cas/login";
const BALANCE_PATH: &str = "/dianbiao/chongzhi.jsp";

#[derive(Deserialize)]
struct Config {
    cas: Credentials,
    aircon: Option<RoomConfig>,
    #[serde(rename = "dorm_electricity")]
    _dorm_electricity: Option<RoomConfig>,
    output_html: Option<String>,
}

#[derive(Deserialize)]
struct RoomConfig {
    building: Option<u16>,
    floor: Option<u16>,
    room: Option<u16>,
}

impl Config {
    fn selected_aircon(&self) -> Result<Option<(u16, u16, u16)>> {
        match self.aircon.as_ref().map(|room| (room.building, room.room)) {
            None | Some((None, None)) => Ok(None),
            Some((Some(building), Some(room))) => {
                let floor = self
                    .aircon
                    .as_ref()
                    .and_then(|target| target.floor)
                    .unwrap_or(room / 100);
                if building == 0 || floor == 0 || room == 0 {
                    bail!("aircon 中的公寓、楼层和房间号必须大于 0");
                }
                Ok(Some((building, floor, room)))
            }
            Some(_) => bail!("aircon.building 和 aircon.room 必须同时填写"),
        }
    }
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
    device_id: Option<String>,
}

fn main() -> Result<()> {
    let mode = std::env::args().nth(1);
    if mode.as_deref() == Some("--check-config") {
        let config = load_config()?;
        let room = config.selected_aircon()?;
        println!(
            "配置格式有效；空调房间：{}",
            if room.is_some() {
                "已填写"
            } else {
                "待填写"
            }
        );
        return Ok(());
    }
    let (probe, sms, trust_device, json_output) = match mode.as_deref() {
        None => (false, false, false, false),
        Some("--probe") => (true, false, false, false),
        Some("--sms") => (false, true, false, false),
        Some("--trust-device") => (false, true, true, false),
        Some("--json") => (false, false, false, true),
        Some(_) => bail!("用法：cargo run [--probe|--check-config|--sms|--trust-device|--json]"),
    };
    let config = if probe {
        None
    } else {
        let mut config = load_config()?;
        ensure_device_id(&mut config)?;
        Some(config)
    };
    let selected_aircon = config
        .as_ref()
        .map(Config::selected_aircon)
        .transpose()?
        .flatten();
    let client = Client::builder()
        .cookie_store(true)
        .user_agent("Mozilla/5.0 (compatible; SDU-InfoHelper/0.1)")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("无法创建 HTTP 客户端")?;
    let mut login_url = Url::parse(CAS_LOGIN)?;
    login_url.query_pairs_mut().append_pair("service", SERVICE);

    let login_response = client
        .get(login_url)
        .send()
        .context("无法打开山东大学认证页面")?;
    let login_page_url = login_response.url().clone();
    let login_html = login_response.error_for_status()?.text()?;
    let form = parse_login_form(&login_html, &login_page_url)?;
    if probe {
        println!("CAS 登录页可达；表单字段：rsa、ul、pl、lt、execution、_eventId");
        println!("电费服务入口：{SERVICE}");
        return Ok(());
    }

    let config = config.expect("非探测模式已读取配置");
    let rsa = cas_encrypt(&format!(
        "{}{}{}",
        config.cas.username, config.cas.password, form.lt
    ));
    let fields = [
        ("rsa", rsa),
        ("ul", config.cas.username.encode_utf16().count().to_string()),
        ("pl", config.cas.password.encode_utf16().count().to_string()),
        ("lt", form.lt),
        ("execution", form.execution),
        ("_eventId", form.event_id),
    ];

    // 页面 login.js 先向 /cas/device 验证账号和当前设备，成功后才提交表单。
    let device_info = format!(
        "SDU-InfoHelper Rust CLI; account={}; id={}",
        config.cas.username,
        config.cas.device_id.as_deref().expect("已初始化设备标识")
    );
    let device_hash = hex::encode(Md5::digest(device_info.as_bytes()));
    let device_url = login_page_url.join("device")?;
    let device_fields = [
        ("d", device_hash.clone()),
        ("d_s", device_hash.clone()),
        ("d_md5", cas_encrypt(&device_hash)),
        ("d_browser_md5", cas_encrypt(&device_hash)),
        ("i", cas_encrypt(&device_info)),
        ("m", "1".to_owned()),
        ("u", cas_encrypt(&config.cas.username)),
        ("p", cas_encrypt(&config.cas.password)),
    ];
    let device_response = client
        .post(device_url.clone())
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&device_fields)
        .send()
        .context("CAS 设备检查请求失败")?
        .error_for_status()
        .context("CAS 设备检查返回错误状态")?;
    let device_result: DeviceResult = device_response
        .json()
        .context("CAS 设备检查未返回有效 JSON")?;
    match device_result.info.as_str() {
        "pass" | "binded" => {}
        "bind" if sms => verify_sms(
            &client,
            &device_url,
            &device_hash,
            &device_info,
            &config.cas.username,
            trust_device,
        )?,
        "bind" => bail!(
            "学校要求当前设备完成手机/扫码二次验证。请在本机运行 cargo run -- --sms；若想授信此设备，运行 cargo run -- --trust-device"
        ),
        "validErr" | "notFound" => bail!("学校设备检查未接受学号或密码，请核对 config.yaml"),
        "mobileErr" => bail!("学校要求手机验证，但账号未绑定手机"),
        other => bail!("CAS 设备检查返回未识别状态：{other}"),
    }

    let response = client
        .post(form.action)
        .form(&fields)
        .send()
        .context("统一身份认证请求失败")?;
    let result_url = response.url().clone();
    let status = response.status();
    let html = response.text()?;

    if result_url.host_str() == Some("pass.sdu.edu.cn") {
        let message = visible_text(&html);
        bail!(
            "认证未完成（HTTP {status}；可能是凭据错误或需要手机/设备二次验证）。页面提示：{}",
            short_text(&message)
        );
    }

    if result_url.host_str() != Some("gyktgd.wh.sdu.edu.cn") {
        bail!(
            "登录后跳转到了意外域名：{}",
            result_url.host_str().unwrap_or("未知")
        );
    }

    let page_html = if let Some((building, floor, room)) = selected_aircon {
        let detail_url = balance_url(&result_url, building, floor, room)?;
        let detail_response = client
            .get(detail_url)
            .send()
            .context("无法打开查余电页面")?;
        if detail_response.url().host_str() != Some("gyktgd.wh.sdu.edu.cn") {
            bail!("查余电页面跳转离开电费系统，登录会话可能已失效");
        }
        let page = detail_response.error_for_status()?.text()?;
        let reading = parse_aircon_reading(&page)?;
        if reading.building != building || reading.floor != floor || reading.room != room {
            bail!("查余电页面的房间与 config.yaml 不一致，已停止读取");
        }
        if json_output {
            println!(
                "{}",
                serde_json::json!({
                    "service": "aircon",
                    "building": reading.building,
                    "floor": reading.floor,
                    "room": reading.room,
                    "remaining_kwh": reading.remaining_kwh,
                })
            );
        } else {
            println!(
                "{} 公寓 {} 房间剩余电量：{} 度",
                building, room, reading.remaining_kwh
            );
        }
        page
    } else {
        if json_output {
            bail!("--json 需要先填写 aircon.building 和 aircon.room");
        }
        println!(
            "认证成功。请在 config.yaml 增加 aircon.building 和 aircon.room，以查询指定宿舍。"
        );
        html
    };
    if let Some(path) = config.output_html {
        let path = Path::new(&path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("无法创建输出目录")?;
        }
        fs::write(path, page_html).context("无法保存登录后页面")?;
        if !json_output {
            println!("页面已保存：{}", path.display());
        }
    }
    Ok(())
}

fn load_config() -> Result<Config> {
    let config_text = fs::read_to_string("config.yaml")
        .context("找不到 config.yaml；请复制 config.example.yaml 并填写学号、密码")?;
    let config: Config = serde_yaml::from_str(&config_text).context("config.yaml 格式有误")?;
    if config.cas.username.trim().is_empty()
        || config.cas.password.is_empty()
        || config.cas.username == "你的学号"
        || config.cas.password == "你的统一身份认证密码"
    {
        bail!("请先在 config.yaml 填写真实的学号和统一身份认证密码");
    }
    Ok(config)
}

fn ensure_device_id(config: &mut Config) -> Result<()> {
    if let Some(id) = config.cas.device_id.as_deref() {
        if hex::decode(id).is_err() || id.len() < 32 {
            bail!("cas.device_id 必须是至少 32 位的十六进制字符串");
        }
        return Ok(());
    }
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let id = hex::encode(bytes);
    let path = Path::new("config.yaml");
    let existing = fs::read_to_string(path)?;
    let updated = insert_device_id(&existing, &id)?;
    let temp = path.with_extension("yaml.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).context("无法创建临时配置文件")?;
    file.write_all(updated.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temp, path).context("无法保存设备标识到 config.yaml")?;
    config.cas.device_id = Some(id);
    Ok(())
}

fn insert_device_id(existing: &str, id: &str) -> Result<String> {
    let lines: Vec<&str> = existing.lines().collect();
    let cas_start = lines
        .iter()
        .position(|line| line.trim_end() == "cas:")
        .context("config.yaml 缺少顶层 cas: 字段，无法保存设备标识")?;
    let cas_end = lines
        .iter()
        .enumerate()
        .skip(cas_start + 1)
        .find(|(_, line)| !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#'))
        .map(|(index, _)| index)
        .unwrap_or(lines.len());
    let existing_index =
        (cas_start + 1..cas_end).find(|index| lines[*index].starts_with("  device_id:"));
    let mut updated = String::new();
    for (index, line) in lines.iter().enumerate() {
        if existing_index == Some(index) {
            updated.push_str(&format!("  device_id: \"{id}\"\n"));
            continue;
        }
        updated.push_str(line);
        updated.push('\n');
        if existing_index.is_none() && index == cas_start {
            updated.push_str(&format!("  device_id: \"{id}\"\n"));
        }
    }
    Ok(updated)
}

struct LoginForm {
    action: Url,
    lt: String,
    execution: String,
    event_id: String,
}

#[derive(Deserialize)]
struct DeviceResult {
    info: String,
}

struct AirconReading {
    building: u16,
    floor: u16,
    room: u16,
    remaining_kwh: f64,
}

fn verify_sms(
    client: &Client,
    device_url: &Url,
    device_hash: &str,
    device_info: &str,
    username: &str,
    trust_device: bool,
) -> Result<()> {
    let send_result: DeviceResult = client
        .post(device_url.clone())
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[("m", "2")])
        .send()
        .context("发送手机验证码请求失败")?
        .error_for_status()?
        .json()?;
    match send_result.info.as_str() {
        "send" => {}
        "max" => bail!("学校限制了验证码发送频率，请稍后再试"),
        "unknow" => bail!("学校反馈当前手机未绑定"),
        other => bail!("学校未发送验证码，状态：{other}"),
    }
    print!("验证码已发送到绑定手机。请在本机输入验证码：");
    io::stdout().flush()?;
    let mut code = String::new();
    io::stdin().read_line(&mut code)?;
    let code = code.trim();
    if code.is_empty() {
        bail!("未输入验证码");
    }
    let verify_result: DeviceResult = client
        .post(device_url.clone())
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[
            ("d", device_hash),
            ("i", device_info),
            ("m", "3"),
            ("u", username),
            ("c", code),
            ("s", if trust_device { "1" } else { "0" }),
        ])
        .send()
        .context("手机验证码验证请求失败")?
        .error_for_status()?
        .json()?;
    match verify_result.info.as_str() {
        "ok" => {
            if trust_device {
                println!("学校已接受当前设备的授信请求。后续是否免短信将在下次登录时验证。");
            }
            Ok(())
        }
        "most" => {
            println!("学校提示受信设备已达上限，已自动解除最早一台设备的授信。");
            Ok(())
        }
        "codeErr" => bail!("验证码错误"),
        "timeout" => bail!("验证码已超时"),
        "moreErr" => bail!("验证码错误次数过多，请重新获取"),
        other => bail!("手机验证失败，状态：{other}"),
    }
}

fn parse_login_form(html: &str, page_url: &Url) -> Result<LoginForm> {
    let document = Html::parse_document(html);
    let form_selector = Selector::parse("form#loginForm").unwrap();
    let input_selector = Selector::parse("input").unwrap();
    let form = document
        .select(&form_selector)
        .next()
        .context("找不到 CAS 登录表单")?;
    let action = page_url.join(form.value().attr("action").context("登录表单缺少 action")?)?;
    if action.host_str() != Some("pass.sdu.edu.cn") || action.scheme() != "https" {
        bail!("登录表单的目标地址不可信");
    }
    let field = |name: &str| -> Result<String> {
        form.select(&input_selector)
            .find(|input| input.value().attr("name") == Some(name))
            .and_then(|input| input.value().attr("value"))
            .map(str::to_owned)
            .with_context(|| format!("登录表单缺少 {name}"))
    };
    Ok(LoginForm {
        action,
        lt: field("lt")?,
        execution: field("execution")?,
        event_id: field("_eventId")?,
    })
}

fn balance_url(base: &Url, building: u16, floor: u16, room: u16) -> Result<Url> {
    if base.host_str() != Some("gyktgd.wh.sdu.edu.cn") || base.scheme() != "https" {
        bail!("电费查询基地址不可信");
    }
    let mut url = base.join(BALANCE_PATH)?;
    url.query_pairs_mut()
        .append_pair("gongyu", &building.to_string())
        .append_pair("sushe", &room.to_string())
        .append_pair("floor", &floor.to_string());
    Ok(url)
}

fn parse_aircon_reading(html: &str) -> Result<AirconReading> {
    let document = Html::parse_document(html);
    let row_selector = Selector::parse("tr").unwrap();
    let cell_selector = Selector::parse("td").unwrap();
    let mut fields = HashMap::new();
    for row in document.select(&row_selector) {
        let mut cells = row.select(&cell_selector).map(|cell| {
            cell.text()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        });
        if let (Some(label), Some(value)) = (cells.next(), cells.next()) {
            fields.insert(label.trim_end_matches([':', '：']).trim().to_owned(), value);
        }
    }
    let field = |name: &str| -> Result<&str> {
        fields
            .get(name)
            .map(String::as_str)
            .with_context(|| format!("查余电页面缺少{name}"))
    };
    let remaining_kwh = field("剩余电量")?
        .parse::<f64>()
        .context("剩余电量不是数字")?;
    if !remaining_kwh.is_finite() || remaining_kwh < 0.0 {
        bail!("剩余电量数值无效");
    }
    Ok(AirconReading {
        building: field("公寓")?.parse().context("公寓号不是数字")?,
        floor: field("楼层")?.parse().context("楼层不是数字")?,
        room: field("房间")?.parse().context("房间号不是数字")?,
        remaining_kwh,
    })
}

fn visible_text(html: &str) -> String {
    let document = Html::parse_document(html);
    let selector = Selector::parse("body").unwrap();
    document
        .select(&selector)
        .flat_map(|body| body.descendants())
        .filter(|node| {
            !node.ancestors().any(|ancestor| {
                ElementRef::wrap(ancestor)
                    .is_some_and(|element| matches!(element.value().name(), "script" | "style"))
            })
        })
        .filter_map(|node| node.value().as_text())
        .map(|text| text.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn short_text(text: &str) -> String {
    text.chars().take(1000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_dynamic_cas_fields() {
        let html = r#"<form id="loginForm" action="/cas/login?service=x"><input name="lt" value="LT-test"><input name="execution" value="e1s1"><input name="_eventId" value="submit"></form>"#;
        let form = parse_login_form(html, &Url::parse(CAS_LOGIN).unwrap()).unwrap();
        assert_eq!(form.lt, "LT-test");
        assert_eq!(
            form.action.as_str(),
            "https://pass.sdu.edu.cn/cas/login?service=x"
        );
    }

    #[test]
    fn builds_direct_balance_request() {
        let base = Url::parse(SERVICE).unwrap();
        assert_eq!(
            balance_url(&base, 8, 2, 207).unwrap().as_str(),
            "https://gyktgd.wh.sdu.edu.cn/dianbiao/chongzhi.jsp?gongyu=8&sushe=207&floor=2"
        );
    }

    #[test]
    fn visible_text_skips_scripts() {
        assert_eq!(
            visible_text("<body><h1>余额</h1><script>secret()</script><p>10 元</p></body>"),
            "余额 10 元"
        );
    }

    #[test]
    fn optional_room_sections_parse_without_triggering_query() {
        let yaml = "cas:\n  username: test\n  password: test\naircon:\n  building: null\n  room: null\ndorm_electricity:\n  building: null\n  room: null\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.selected_aircon().unwrap(), None);

        let yaml = yaml.replacen(
            "building: null\n  room: null",
            "building: 3\n  room: 405",
            1,
        );
        let config: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config.selected_aircon().unwrap(), Some((3, 4, 405)));

        let yaml = yaml.replace("room: 405", "floor: 5\n  room: 405");
        let config: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config.selected_aircon().unwrap(), Some((3, 5, 405)));
    }

    #[test]
    fn adds_or_replaces_device_id_without_touching_credentials() {
        let id = "0123456789abcdef0123456789abcdef";
        let config = "cas:\n  username: test\n  password: secret\n";
        let updated = insert_device_id(config, id).unwrap();
        assert!(updated.contains("  password: secret\n"));
        assert_eq!(updated.matches("device_id:").count(), 1);

        let config = "cas:\n  device_id: null\n  password: secret\n";
        let updated = insert_device_id(config, id).unwrap();
        assert_eq!(updated.matches("device_id:").count(), 1);
        assert!(updated.contains(id));
    }

    #[test]
    fn parses_balance_and_room_from_detail_page() {
        let page = r#"<form name="chongdianform"><table>
            <tr><td><h1>公寓: </h1></td><td><h1>8</h1></td></tr>
            <tr><td><h1>楼层: </h1></td><td><h1>2</h1></td></tr>
            <tr><td><h1>房间: </h1></td><td><h1>207</h1></td></tr>
            <tr><td><h1>剩余电量: </h1></td><td><h1>7.01</h1></td></tr>
            </table></form>"#;
        let reading = parse_aircon_reading(page).unwrap();
        assert_eq!((reading.building, reading.floor, reading.room), (8, 2, 207));
        assert_eq!(reading.remaining_kwh, 7.01);
    }
}
