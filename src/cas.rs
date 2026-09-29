//! CAS login/device handshake from WinstonMeursault/SDU-InfoHelper (GPL-3.0).
use crate::{QueryError, cas_des::encrypt, settings::Credentials};
use md5::{Digest, Md5};
use reqwest::{
    Url,
    blocking::{Client, Response},
    redirect::Policy,
};
use scraper::{Html, Selector};
use serde::Deserialize;
use std::{
    io::{self, Write},
    time::Duration,
};

pub const DORM_ENTRY: &str = "https://mcard.sdu.edu.cn/berserker-auth/cas/redirect/neusoft";
pub const AIRCON_ENTRY: &str = "https://gyktgd.wh.sdu.edu.cn/dianbiao/AuthServlet.se";
const CAS_LOGIN: &str = "https://pass.sdu.edu.cn/cas/login";

#[derive(Clone, Copy, Default)]
pub struct LoginOptions {
    pub sms: bool,
    pub trust_device: bool,
}

pub fn trusted_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(
            url.host_str(),
            Some("pass.sdu.edu.cn" | "mcard.sdu.edu.cn" | "gyktgd.wh.sdu.edu.cn")
        )
}

pub fn client(timeout: Duration) -> Result<Client, QueryError> {
    Client::builder()
        // School CAS negotiates TLS 1.2 with an AES-CBC suite unsupported by rustls.
        // Keep certificate verification enabled; reuse the upstream OpenSSL backend.
        .use_native_tls()
        .cookie_store(true)
        .no_proxy()
        .timeout(timeout)
        .user_agent("Mozilla/5.0 (compatible; SDU-InfoHelper/0.1)")
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= 15 || !trusted_url(attempt.url()) {
                attempt.error("认证跳转地址不可信或次数过多")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| QueryError::Network)
}

fn network(error: reqwest::Error) -> QueryError {
    if error.is_timeout() {
        QueryError::Timeout
    } else {
        QueryError::Network
    }
}

fn checked(response: Response) -> Result<Response, QueryError> {
    if !response.status().is_success() {
        return Err(QueryError::Http(response.status().as_u16()));
    }
    Ok(response)
}

struct LoginForm {
    action: Url,
    lt: String,
    execution: String,
    event_id: String,
}
fn parse_form(html: &str, url: &Url) -> Result<LoginForm, QueryError> {
    let document = Html::parse_document(html);
    let form_selector = Selector::parse("form#loginForm").unwrap();
    let input_selector = Selector::parse("input").unwrap();
    let form = document
        .select(&form_selector)
        .next()
        .ok_or(QueryError::AuthFlow("未找到学校 CAS 登录表单。"))?;
    let action = url
        .join(
            form.value()
                .attr("action")
                .ok_or(QueryError::AuthFlow("CAS 表单缺少 action。"))?,
        )
        .map_err(|_| QueryError::AuthFlow("CAS 登录地址格式错误。"))?;
    if !trusted_url(&action)
        || action.host_str() != Some("pass.sdu.edu.cn")
        || action.path() != "/cas/login"
    {
        return Err(QueryError::AuthFlow("CAS 登录表单目标地址不可信。"));
    }
    let field = |name: &str| {
        form.select(&input_selector)
            .find(|i| i.value().attr("name") == Some(name))
            .and_then(|i| i.value().attr("value"))
            .map(str::to_owned)
            .ok_or(QueryError::AuthFlow("CAS 登录表单缺少动态字段。"))
    };
    Ok(LoginForm {
        action,
        lt: field("lt")?,
        execution: field("execution")?,
        event_id: field("_eventId")?,
    })
}

fn entry(entry: &str) -> Result<Url, QueryError> {
    if entry == DORM_ENTRY {
        Ok(Url::parse(entry).unwrap())
    } else if entry == AIRCON_ENTRY {
        let mut url = Url::parse(CAS_LOGIN).unwrap();
        url.query_pairs_mut().append_pair("service", entry);
        Ok(url)
    } else {
        Err(QueryError::Config("不支持的 CAS 服务入口。"))
    }
}

pub fn probe(entry_url: &str, timeout: Duration) -> Result<(), QueryError> {
    let client = client(timeout)?;
    let r = checked(client.get(entry(entry_url)?).send().map_err(network)?)?;
    let url = r.url().clone();
    let html = r.text().map_err(network)?;
    parse_form(&html, &url)?;
    Ok(())
}

/// Returns the existing cookie client and the final response for each service adapter.
pub fn login(
    client: &Client,
    entry_url: &str,
    credentials: &Credentials,
    options: LoginOptions,
) -> Result<Response, QueryError> {
    credentials.validate()?;
    let response = checked(client.get(entry(entry_url)?).send().map_err(network)?)?;
    let url = response.url().clone();
    // A fresh client normally has no CAS session. Handle an already authenticated client.
    if url.host_str() != Some("pass.sdu.edu.cn") {
        return Ok(response);
    }
    let html = response.text().map_err(network)?;
    let form = parse_form(&html, &url)?;
    let id = credentials
        .device_id
        .as_deref()
        .ok_or(QueryError::AuthFlow("缺少本地设备标识。"))?;
    let device_info = format!(
        "SDU-InfoHelper Rust CLI; account={}; id={id}",
        credentials.username
    );
    let hash = hex::encode(Md5::digest(device_info.as_bytes()));
    let device_url = Url::parse("https://pass.sdu.edu.cn/cas/device").unwrap();
    let result: DeviceResult = checked(
        client
            .post(device_url.clone())
            .header("X-Requested-With", "XMLHttpRequest")
            .form(&[
                ("d", hash.clone()),
                ("d_s", hash.clone()),
                ("d_md5", encrypt(&hash)),
                ("d_browser_md5", encrypt(&hash)),
                ("i", encrypt(&device_info)),
                ("m", "1".into()),
                ("u", encrypt(&credentials.username)),
                ("p", encrypt(&credentials.password)),
            ])
            .send()
            .map_err(network)?,
    )?
    .json()
    .map_err(|_| QueryError::AuthFlow("CAS 设备检查响应格式错误。"))?;
    match result.info.as_str() {
        "pass" | "binded" => {}
        "bind" if options.sms || options.trust_device => verify_sms(
            client,
            &device_url,
            &hash,
            &device_info,
            &credentials.username,
            options.trust_device,
        )?,
        "bind" => {
            return Err(QueryError::AuthFlow(
                "学校要求二次验证，请在本机运行 auth login --trust-device；后台不会发送短信。",
            ));
        }
        "validErr" | "notFound" => {
            return Err(QueryError::AuthFlow(
                "CAS 未接受账号或密码，请核对本机 config.yaml。",
            ));
        }
        "mobileErr" => return Err(QueryError::AuthFlow("CAS 要求手机验证，但账号未绑定手机。")),
        _ => return Err(QueryError::AuthFlow("CAS 设备检查返回未识别状态。")),
    }
    let rsa = encrypt(&format!(
        "{}{}{}",
        credentials.username, credentials.password, form.lt
    ));
    let response = checked(
        client
            .post(form.action)
            .form(&[
                ("rsa", rsa),
                (
                    "ul",
                    credentials.username.encode_utf16().count().to_string(),
                ),
                (
                    "pl",
                    credentials.password.encode_utf16().count().to_string(),
                ),
                ("lt", form.lt),
                ("execution", form.execution),
                ("_eventId", form.event_id),
            ])
            .send()
            .map_err(network)?,
    )?;
    if response.url().host_str() == Some("pass.sdu.edu.cn") {
        return Err(QueryError::AuthFlow(
            "CAS 登录未完成，请核对凭据或在本机完成二次验证。",
        ));
    }
    let expected = if entry_url == DORM_ENTRY {
        "mcard.sdu.edu.cn"
    } else {
        "gyktgd.wh.sdu.edu.cn"
    };
    if response.url().host_str() != Some(expected) {
        return Err(QueryError::AuthFlow("CAS 登录后未进入预期服务。"));
    }
    Ok(response)
}

#[derive(Deserialize)]
struct DeviceResult {
    info: String,
}
fn verify_sms(
    client: &Client,
    url: &Url,
    hash: &str,
    info: &str,
    username: &str,
    trust: bool,
) -> Result<(), QueryError> {
    let sent: DeviceResult = checked(
        client
            .post(url.clone())
            .header("X-Requested-With", "XMLHttpRequest")
            .form(&[("m", "2")])
            .send()
            .map_err(network)?,
    )?
    .json()
    .map_err(|_| QueryError::AuthFlow("短信发送响应格式错误。"))?;
    if sent.info != "send" {
        return Err(QueryError::AuthFlow(
            "学校未发送验证码，请检查绑定手机或稍后重试。",
        ));
    }
    eprint!("验证码已发送，请在本机终端输入：");
    io::stderr()
        .flush()
        .map_err(|_| QueryError::AuthFlow("无法打开本机终端。"))?;
    let mut code = String::new();
    io::stdin()
        .read_line(&mut code)
        .map_err(|_| QueryError::AuthFlow("无法读取验证码。"))?;
    let code = code.trim();
    if code.is_empty() {
        return Err(QueryError::AuthFlow("未输入验证码。"));
    }
    let verified: DeviceResult = checked(
        client
            .post(url.clone())
            .header("X-Requested-With", "XMLHttpRequest")
            .form(&[
                ("d", hash),
                ("i", info),
                ("m", "3"),
                ("u", username),
                ("c", code),
                ("s", if trust { "1" } else { "0" }),
            ])
            .send()
            .map_err(network)?,
    )?
    .json()
    .map_err(|_| QueryError::AuthFlow("短信验证响应格式错误。"))?;
    match verified.info.as_str() {
        "ok" => Ok(()),
        "most" => {
            eprintln!("学校已授信当前设备，并解除了最早一台设备的授信。");
            Ok(())
        }
        _ => Err(QueryError::AuthFlow("验证码未被接受，可能错误或已过期。")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_rejects_untrusted_actions() {
        let html = r#"<form id="loginForm" action="https://evil.example/cas/login"><input name="lt" value="x"></form>"#;
        assert!(parse_form(html, &Url::parse(CAS_LOGIN).unwrap()).is_err());
        assert!(!trusted_url(
            &Url::parse("http://pass.sdu.edu.cn/cas/login").unwrap()
        ));
        assert!(!trusted_url(
            &Url::parse("https://mcard.sdu.edu.cn:1234/").unwrap()
        ));
    }
    #[test]
    fn form_reads_dynamic_fields_and_service() {
        let html = r#"<form id="loginForm" action="/cas/login?service=x"><input name="lt" value="LT-test"><input name="execution" value="e1s1"><input name="_eventId" value="submit"></form>"#;
        let form = parse_form(html, &Url::parse(CAS_LOGIN).unwrap()).unwrap();
        assert_eq!(form.lt, "LT-test");
        assert_eq!(form.execution, "e1s1");
        assert_eq!(form.action.query(), Some("service=x"));
    }
}
