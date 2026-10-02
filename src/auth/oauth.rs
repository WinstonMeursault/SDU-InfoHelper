//! OAuth client discovery and refresh requests, independent of cache persistence.
use super::{
    TokenCache,
    model::{claim, number},
};
use crate::QueryError;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Utc;
use reqwest::{Url, blocking::Client};
use scraper::{Html, Selector};
use serde_json::Value;

/// Match the public frontend's OAuth client to the JWT client_id. Never print Basic values.
fn discover_client(client: &Client, cache: &TokenCache) -> Result<String, QueryError> {
    let id = claim(&cache.access_token, "client_id")
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or(QueryError::AuthFlow(
            "无法识别 OAuth 客户端，请导入对应的 client_authorization。",
        ))?;
    let auth = format!("bearer {}", cache.access_token);
    let response = client
        .get("https://mcard.sdu.edu.cn/charge-app/")
        .header("synjones-auth", &auth)
        .send()
        .map_err(|_| QueryError::Network)?;
    if matches!(response.status().as_u16(), 401 | 403) || response.status().is_redirection() {
        return Err(QueryError::Authentication);
    }
    if !response.status().is_success() {
        return Err(QueryError::Http(response.status().as_u16()));
    }
    let html = response.text().map_err(|_| QueryError::Network)?;
    let document = Html::parse_document(&html);
    let scripts = Selector::parse("script[src]").unwrap();
    let base = Url::parse("https://mcard.sdu.edu.cn/charge-app/").unwrap();
    let pattern = regex::Regex::new(r"Basic ([A-Za-z0-9+/=]+)").unwrap();
    for script in document.select(&scripts) {
        let Some(src) = script.value().attr("src") else {
            continue;
        };
        let Ok(url) = base.join(src) else { continue };
        if url.origin() != base.origin()
            || !url.path().starts_with("/charge-app/static/js/app.")
            || !url.path().ends_with(".js")
        {
            continue;
        }
        let response = client
            .get(url)
            .header("synjones-auth", &auth)
            .send()
            .map_err(|_| QueryError::Network)?;
        if matches!(response.status().as_u16(), 401 | 403) || response.status().is_redirection() {
            return Err(QueryError::Authentication);
        }
        if !response.status().is_success() {
            return Err(QueryError::Http(response.status().as_u16()));
        }
        let text = response.text().map_err(|_| QueryError::Network)?;
        for found in pattern.captures_iter(&text) {
            if let Ok(decoded) = STANDARD.decode(&found[1])
                && let Ok(pair) = String::from_utf8(decoded)
                && pair.split_once(':').is_some_and(|(name, _)| name == id)
            {
                return Ok(format!("Basic {}", &found[1]));
            }
        }
    }
    Err(QueryError::AuthFlow(
        "网页中未找到匹配的 OAuth 客户端，请导入对应的 client_authorization 或使用 CAS 重新登录。",
    ))
}

pub(super) fn refresh(client: &Client, old: &TokenCache) -> Result<TokenCache, QueryError> {
    let authorization = match old.client_authorization.as_ref() {
        Some(a) => a.clone(),
        None => discover_client(client, old)?,
    };
    refresh_request(client, old, old.provider.endpoint(), &authorization)
}

pub(super) fn refresh_request(
    client: &Client,
    old: &TokenCache,
    endpoint: &str,
    authorization: &str,
) -> Result<TokenCache, QueryError> {
    let refresh_token = old
        .refresh_token
        .as_ref()
        .filter(|x| !x.is_empty())
        .ok_or(QueryError::Authentication)?;
    let response = client
        .post(endpoint)
        .header("Authorization", authorization)
        .form(&[
            ("grant_type", "refresh_token"),
            ("scope", "all"),
            ("refresh_token", refresh_token),
            ("logintype", &old.logintype),
        ])
        .send()
        .map_err(|e| {
            if e.is_timeout() {
                QueryError::Timeout
            } else {
                QueryError::Network
            }
        })?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Err(QueryError::Authentication);
    }
    // OAuth invalid_grant commonly uses HTTP 400. Other HTTP errors, including
    // timeouts and rate limits, must remain retryable rather than trigger CAS.
    if !status.is_success() && status.as_u16() != 400 {
        return Err(QueryError::Http(status.as_u16()));
    }
    let payload: Value = response
        .json()
        .map_err(|_| QueryError::Response("令牌刷新响应格式错误，请稍后重试。"))?;
    if matches!(
        payload.get("error").and_then(Value::as_str),
        Some("invalid_grant" | "invalid_token" | "invalid_client" | "unauthorized_client")
    ) || matches!(number(payload.get("code")), Some(401 | 403))
    {
        return Err(QueryError::Authentication);
    }
    if !status.is_success() {
        return Err(QueryError::Http(status.as_u16()));
    }
    if payload.get("error").is_some() || number(payload.get("code")).is_some_and(|code| code != 200)
    {
        return Err(QueryError::Response("令牌刷新暂未成功，请稍后重试。"));
    }
    let mut new = TokenCache::from_response(&payload, old.provider, Utc::now().timestamp())
        .map_err(|_| QueryError::Response("令牌刷新响应缺少有效凭据，请稍后重试。"))?
        .merged_refresh(old);
    new.client_authorization = Some(authorization.into());
    Ok(new)
}
