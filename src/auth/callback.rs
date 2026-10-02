//! Known CAS callbacks and frontend SSO ticket exchange.
use super::{LoginOptions, OAuthProvider, TokenCache, device::credentials, secure_write};
use crate::{
    QueryError, cas,
    settings::{Credentials, Settings},
};
use chrono::Utc;
use reqwest::{Url, blocking::Client};
use scraper::{Html, Selector};
use serde_json::Value;
use std::{path::Path, time::Duration};

pub(super) fn cache_from_url(url: &Url) -> Option<TokenCache> {
    let mut fields = serde_json::Map::new();
    for (key, value) in url
        .query_pairs()
        .chain(url.fragment().into_iter().flat_map(|fragment| {
            Url::parse(&format!("https://mcard.sdu.edu.cn/?{fragment}"))
                .ok()
                .into_iter()
                .flat_map(|u| {
                    u.query_pairs()
                        .map(|(k, v)| (k.into_owned().into(), v.into_owned().into()))
                        .collect::<Vec<_>>()
                })
        }))
    {
        if [
            "access_token",
            "token",
            "refresh_token",
            "expires_in",
            "logintype",
        ]
        .contains(&key.as_ref())
        {
            fields.insert(key.into_owned(), Value::String(value.into_owned()));
        }
    }
    TokenCache::from_response(
        &Value::Object(fields),
        OAuthProvider::Berserker,
        Utc::now().timestamp(),
    )
    .ok()
}

pub(super) fn cas_token(
    settings: &Settings,
    path: &Path,
    timeout: Duration,
    options: LoginOptions,
) -> Result<TokenCache, QueryError> {
    let credentials = credentials(path)?;
    let mut cache = cas_token_for_credentials(settings, path, timeout, &credentials, options)?;
    cache.cas_username = Some(credentials.username);
    Ok(cache)
}

fn cas_token_for_credentials(
    settings: &Settings,
    path: &Path,
    timeout: Duration,
    credentials: &Credentials,
    options: LoginOptions,
) -> Result<TokenCache, QueryError> {
    let client = cas::client(timeout)?;
    let response = cas::login(&client, cas::DORM_ENTRY, credentials, options)?;
    let url = response.url().clone();
    if let Some(cache) = cache_from_url(&url) {
        return Ok(cache);
    }
    let body = response.text().map_err(|_| QueryError::Network)?;
    if let Ok(payload) = serde_json::from_str::<Value>(&body) {
        return TokenCache::from_response(
            &payload,
            OAuthProvider::Berserker,
            Utc::now().timestamp(),
        );
    }
    if url.path() == "/plat/"
        && url
            .query_pairs()
            .any(|(k, v)| k == "name" && v == "loginTransit")
        && let Some((_, ticket)) = url.query_pairs().find(|(k, _)| k == "ticket")
    {
        // The actual CAS callback uses the plat loginTransit component's SSO grant.
        // username/password here are the one-time ticket, never the CAS password.
        let authorization = sso_client(&client, &body, &url)?;
        let response = client
            .post(OAuthProvider::Berserker.endpoint())
            .header("Authorization", &authorization)
            .header("synAccessSource", "h5")
            .form(&[
                ("username", ticket.as_ref()),
                ("password", ticket.as_ref()),
                ("grant_type", "password"),
                ("scope", "all"),
                ("loginFrom", "app"),
                ("logintype", "sso"),
                ("device_token", "h5"),
            ])
            .send()
            .map_err(|_| QueryError::Network)?;
        if !response.status().is_success() {
            return Err(QueryError::AuthFlow("校园卡平台未接受 CAS 一次性票据。"));
        }
        let payload: Value = response
            .json()
            .map_err(|_| QueryError::AuthFlow("SSO 票据交换未返回有效 JSON。"))?;
        let mut cache =
            TokenCache::from_response(&payload, OAuthProvider::Berserker, Utc::now().timestamp())?;
        cache.logintype = "sso".into();
        cache.client_authorization = Some(authorization);
        return Ok(cache);
    }
    // charge-app explicitly exchanges a CAS ticket through this observed frontend route.
    if url.path().starts_with("/charge-app")
        && let Some((_, ticket)) = url.query_pairs().find(|(k, _)| k == "ticket")
    {
        let response = crate::client(timeout)?
            .post("https://mcard.sdu.edu.cn/blade-auth/token/fwdt")
            .form(&[("ticket", ticket.as_ref())])
            .send()
            .map_err(|_| QueryError::Network)?;
        let payload: Value = response
            .json()
            .map_err(|_| QueryError::AuthFlow("票据交换响应格式错误。"))?;
        return TokenCache::from_response(&payload, OAuthProvider::Blade, Utc::now().timestamp());
    }
    // Keep an unknown callback local for protocol investigation, never in logs.
    let diagnostic = settings
        .cache_path(path)
        .with_file_name("cas-callback.html");
    secure_write(&diagnostic, body.as_bytes())?;
    secure_write(&diagnostic.with_extension("url"), url.as_str().as_bytes())?;
    Err(QueryError::AuthFlow(
        "CAS 已返回宿舍平台，但未找到令牌；该账号的回调格式尚需验证，请暂用现有令牌。",
    ))
}

pub(super) fn sso_header(source: &str) -> Result<String, QueryError> {
    let marker = source
        .find("logintype:\"sso\"")
        .ok_or(QueryError::AuthFlow("平台 SSO 登录组件格式已变化。"))?;
    let section = &source[marker..];
    let end = section
        .char_indices()
        .map(|(i, _)| i)
        .nth(4000)
        .unwrap_or(section.len());
    let basic = regex::Regex::new(r#"Authorization:"(Basic [A-Za-z0-9+/=]+)""#).unwrap();
    let header = basic
        .captures(&section[..end])
        .ok_or(QueryError::AuthFlow("平台 SSO 客户端认证配置未找到。"))?;
    Ok(header[1].to_owned())
}
fn sso_client(client: &Client, html: &str, page_url: &Url) -> Result<String, QueryError> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("script[src]").unwrap();
    let scripts = document
        .select(&selector)
        .filter_map(|s| s.value().attr("src"))
        .filter_map(|s| page_url.join(s).ok());
    let hash_pattern = regex::Regex::new(r#"\blogin:"([a-f0-9]{8,})""#).unwrap();
    for script_url in scripts {
        if script_url.origin() != page_url.origin()
            || !script_url.path().starts_with("/plat/js/app.")
            || !script_url.path().ends_with(".js")
        {
            continue;
        }
        let response = client
            .get(script_url.clone())
            .send()
            .map_err(|_| QueryError::Network)?;
        if !response.status().is_success() {
            return Err(QueryError::Http(response.status().as_u16()));
        }
        let source = response.text().map_err(|_| QueryError::Network)?;
        let hash = hash_pattern
            .captures(&source)
            .ok_or(QueryError::AuthFlow("平台登录组件入口未找到。"))?;
        let chunk_url = script_url
            .join(&format!("login.{}.js", &hash[1]))
            .map_err(|_| QueryError::AuthFlow("平台登录组件地址错误。"))?;
        let response = client
            .get(chunk_url)
            .send()
            .map_err(|_| QueryError::Network)?;
        if !response.status().is_success() {
            return Err(QueryError::Http(response.status().as_u16()));
        }
        return sso_header(&response.text().map_err(|_| QueryError::Network)?);
    }
    Err(QueryError::AuthFlow("平台回调缺少已知登录组件。"))
}
