//! Authentication application service: account-bound cache, OAuth refresh and CAS fallback.
mod cache;
mod callback;
mod device;
mod model;
mod oauth;

pub use crate::cas::LoginOptions;
use crate::{
    Config, QueryError, cas,
    control::{OperationControl, OperationError},
    settings::Settings,
};
use cache::{load_cache, lock, save_cache};
use callback::cas_token;
use chrono::Utc;
use device::credentials;
pub use model::{OAuthProvider, TokenCache};
use oauth::refresh;
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::Value;
use std::{fs, path::Path, time::Duration};

/// Compatibility adapter for existing callers; generic file I/O lives in local infrastructure.
pub fn secure_write(path: &Path, bytes: &[u8]) -> Result<(), QueryError> {
    crate::local::secure_write(path, bytes)
        .map_err(|_| QueryError::Config("无法原子保存本地凭据。"))
}
/// Refresh at most once under a process-shared lock; background CAS never sends SMS.
pub fn token(
    settings: &Settings,
    path: &Path,
    timeout: Duration,
    rejected: Option<&str>,
) -> Result<TokenCache, QueryError> {
    token_controlled(
        settings,
        path,
        &OperationControl::uninterrupted(timeout),
        rejected,
    )
    .map_err(OperationError::into_query)
}

pub(crate) fn token_controlled(
    settings: &Settings,
    path: &Path,
    control: &OperationControl<'_>,
    rejected: Option<&str>,
) -> Result<TokenCache, OperationError> {
    let cache_path = settings.cache_path(path);
    let _lock = lock(&cache_path, control)?;
    control.check()?;
    let token = token_from_cache(
        settings,
        path,
        rejected,
        |cache| refresh(&crate::client(control.timeout)?, cache),
        || cas_token(settings, path, control.timeout, LoginOptions::default()),
    )?;
    control.check()?;
    Ok(token)
}

#[cfg(test)]
fn token_with(
    settings: &Settings,
    path: &Path,
    rejected: Option<&str>,
    refresh: impl FnOnce(&TokenCache) -> Result<TokenCache, QueryError>,
    login: impl FnOnce() -> Result<TokenCache, QueryError>,
) -> Result<TokenCache, QueryError> {
    let cache_path = settings.cache_path(path);
    let _lock = lock(
        &cache_path,
        &OperationControl::uninterrupted(Duration::from_secs(30)),
    )
    .map_err(OperationError::into_query)?;
    token_from_cache(settings, path, rejected, refresh, login)
}

fn token_from_cache(
    settings: &Settings,
    path: &Path,
    rejected: Option<&str>,
    refresh: impl FnOnce(&TokenCache) -> Result<TokenCache, QueryError>,
    login: impl FnOnce() -> Result<TokenCache, QueryError>,
) -> Result<TokenCache, QueryError> {
    let cache_path = settings.cache_path(path);
    let cache = load_cache(&cache_path)?;
    if let Some(cache) = cache
        .as_ref()
        .filter(|cache| cache.matches_account(settings.cas_username()))
    {
        if cache.usable(Utc::now().timestamp()) && rejected != Some(cache.access_token.as_str()) {
            return Ok(cache.clone());
        }
        if cache.refresh_token.is_some() {
            match refresh(cache) {
                Ok(new) => {
                    save_cache(&cache_path, &new)?;
                    return Ok(new);
                }
                // Invalid grants and an unavailable client configuration can be
                // recovered through CAS. Service/response failures keep the cache.
                Err(QueryError::Authentication | QueryError::AuthFlow(_)) => {}
                Err(error) => return Err(error),
            }
        }
    }
    let new = login()?;
    save_cache(&cache_path, &new)?;
    Ok(new)
}

pub fn login(
    path: &Path,
    timeout: Duration,
    options: LoginOptions,
) -> Result<AuthStatus, QueryError> {
    let settings = Settings::read(path)?;
    let cache_path = settings.cache_path(path);
    let _lock = lock(&cache_path, &OperationControl::uninterrupted(timeout))
        .map_err(OperationError::into_query)?;
    let cache = cas_token(&settings, path, timeout, options)?;
    save_cache(&cache_path, &cache)?;
    Ok(AuthStatus::from_cache(Some(&cache)))
}
pub fn probe(aircon: bool, timeout: Duration) -> Result<(), QueryError> {
    cas::probe(
        if aircon {
            cas::AIRCON_ENTRY
        } else {
            cas::DORM_ENTRY
        },
        timeout,
    )
}

#[derive(Serialize)]
pub struct AuthStatus {
    pub cached: bool,
    pub has_refresh_token: bool,
    pub expires_at: Option<String>,
}
impl AuthStatus {
    fn from_cache(cache: Option<&TokenCache>) -> Self {
        Self {
            cached: cache.is_some(),
            has_refresh_token: cache.is_some_and(|c| c.refresh_token.is_some()),
            expires_at: cache
                .and_then(|c| c.expires_at)
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map(|t| t.to_rfc3339()),
        }
    }
}
pub fn status(path: &Path) -> Result<AuthStatus, QueryError> {
    let settings = Settings::read(path)?;
    Ok(AuthStatus::from_cache(
        load_cache(&settings.cache_path(path))?
            .as_ref()
            .filter(|cache| cache.matches_account(settings.cas_username())),
    ))
}

pub fn renew(path: &Path, timeout: Duration) -> Result<AuthStatus, QueryError> {
    let settings = Settings::read(path)?;
    let old = load_cache(&settings.cache_path(path))?.ok_or(QueryError::AuthFlow(
        "没有可续期的本地令牌，请先运行 auth login。",
    ))?;
    let new = token(&settings, path, timeout, Some(&old.access_token))?;
    Ok(AuthStatus::from_cache(Some(&new)))
}
pub fn import(
    path: &Path,
    input: &Path,
    provider: OAuthProvider,
    client_auth: Option<&Path>,
) -> Result<AuthStatus, QueryError> {
    let settings = Settings::read(path)?;
    let cache_path = settings.cache_path(path);
    let _lock = lock(
        &cache_path,
        &OperationControl::uninterrupted(Duration::from_secs(30)),
    )
    .map_err(OperationError::into_query)?;
    let payload: Value = serde_json::from_slice(
        &fs::read(input).map_err(|_| QueryError::Config("无法读取本地令牌 JSON。"))?,
    )
    .map_err(|_| QueryError::Config("本地令牌 JSON 格式错误。"))?;
    let mut cache = if payload.get("schema_version").is_some() {
        let request: Config = serde_json::from_value(payload)
            .map_err(|_| QueryError::Config("旧查询配置格式错误。"))?;
        request.validate()?;
        TokenCache::from_response(
            &serde_json::json!({"access_token":request.auth().unwrap().split_whitespace().last()}),
            provider,
            Utc::now().timestamp(),
        )?
    } else {
        TokenCache::from_response(&payload, provider, Utc::now().timestamp())?
    };
    cache.cas_username = settings.cas_username().map(str::to_owned);
    if let Some(file) = client_auth {
        let authorization = fs::read_to_string(file)
            .map_err(|_| QueryError::Config("无法读取本地 OAuth 客户端认证文件。"))?;
        let authorization = authorization.trim();
        if !authorization.starts_with("Basic ")
            || reqwest::header::HeaderValue::from_str(authorization).is_err()
        {
            return Err(QueryError::Config(
                "客户端认证文件必须为一行 Basic 认证头。",
            ));
        }
        cache.client_authorization = Some(authorization.into());
    }
    save_cache(&cache_path, &cache)?;
    Ok(AuthStatus::from_cache(Some(&cache)))
}

pub fn aircon_session(
    path: &Path,
    timeout: Duration,
    options: LoginOptions,
) -> Result<(Client, crate::settings::AirconTarget), QueryError> {
    let settings = Settings::read(path)?;
    settings.aircon.selected()?;
    let _lock = lock(
        &settings.cache_path(path),
        &OperationControl::uninterrupted(timeout),
    )
    .map_err(OperationError::into_query)?;
    let credentials = credentials(path)?;
    let client = cas::client(timeout)?;
    cas::login(&client, cas::AIRCON_ENTRY, &credentials, options)?;
    Ok((client, settings.aircon))
}

#[cfg(test)]
mod tests;
