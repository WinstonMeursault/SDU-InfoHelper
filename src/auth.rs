//! OAuth cache and CAS fallback. No secrets in errors, status output or Debug.
pub use crate::cas::LoginOptions;
use crate::{
    Config, QueryError, cas,
    settings::{Credentials, Settings},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chrono::Utc;
use fs2::FileExt;
use rand::{RngCore, rngs::OsRng};
use reqwest::{Url, blocking::Client};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, io::Write, path::Path, time::Duration};

#[derive(Clone, Copy, Default, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    #[default]
    Berserker,
    Blade,
}
impl OAuthProvider {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Berserker => "https://mcard.sdu.edu.cn/berserker-auth/oauth/token",
            Self::Blade => "https://mcard.sdu.edu.cn/blade-auth/oauth/token",
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct TokenCache {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub provider: OAuthProvider,
    #[serde(default)]
    pub logintype: String,
    pub client_authorization: Option<String>,
    /// Local account binding; omitted for imports without a configured CAS account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cas_username: Option<String>,
}

fn claim(token: &str, key: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&decoded)
        .ok()?
        .get(key)
        .cloned()
}
fn nonempty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}
fn number(value: Option<&Value>) -> Option<i64> {
    value.and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
}
impl TokenCache {
    pub fn from_response(
        payload: &Value,
        provider: OAuthProvider,
        now: i64,
    ) -> Result<Self, QueryError> {
        if payload.get("error").is_some()
            || number(payload.get("code")).is_some_and(|code| code != 200)
        {
            return Err(QueryError::Authentication);
        }
        // Only structured token fields are accepted. Error messages / arbitrary HTML are not tokens.
        let data = token_object(payload).ok_or(QueryError::AuthFlow(
            "登录响应中未找到可用的 access_token；需要确认认证回调格式。",
        ))?;
        let access = nonempty(data.get("access_token"))
            .or_else(|| nonempty(data.get("token")))
            .ok_or(QueryError::Authentication)?;
        let access = access
            .strip_prefix("bearer ")
            .or_else(|| access.strip_prefix("Bearer "))
            .unwrap_or(&access)
            .to_owned();
        validate_token(&access)?;
        let expires = number(data.get("expires_in"))
            .filter(|n| *n > 0)
            .and_then(|n| now.checked_add(n));
        let jwt_exp = claim(&access, "exp").and_then(|v| v.as_i64());
        let expires_at = match (expires, jwt_exp) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Ok(Self {
            access_token: access,
            refresh_token: nonempty(data.get("refresh_token")),
            expires_at,
            provider,
            logintype: nonempty(data.get("logintype")).unwrap_or_default(),
            client_authorization: None,
            cas_username: None,
        })
    }
    fn matches_account(&self, settings: &Settings) -> bool {
        settings
            .cas_username()
            .is_none_or(|username| self.cas_username.as_deref() == Some(username))
    }
    fn usable(&self, now: i64) -> bool {
        self.expires_at
            .is_none_or(|exp| exp > now.saturating_add(300))
    }
    fn merged_refresh(mut self, old: &Self) -> Self {
        // Some providers rotate refresh tokens, others omit an unchanged one.
        self.refresh_token = self.refresh_token.or_else(|| old.refresh_token.clone());
        self.client_authorization = old.client_authorization.clone();
        self.cas_username = old.cas_username.clone();
        if self.logintype.is_empty() {
            self.logintype = old.logintype.clone();
        }
        self
    }
}
fn token_object(value: &Value) -> Option<&Value> {
    if value.get("access_token").and_then(Value::as_str).is_some()
        || value.get("token").and_then(Value::as_str).is_some()
    {
        return Some(value);
    }
    // Common CAS/OAuth envelopes, including the ticket exchange's data.
    for key in ["data", "map", "result"] {
        if let Some(found) = value.get(key).and_then(token_object) {
            return Some(found);
        }
    }
    None
}
fn validate_token(token: &str) -> Result<(), QueryError> {
    if token.is_empty()
        || token.len() > 32768
        || token.chars().any(char::is_whitespace)
        || !token.is_ascii()
    {
        Err(QueryError::AuthFlow("access_token 格式不匹配。"))
    } else {
        Ok(())
    }
}

pub fn secure_write(path: &Path, bytes: &[u8]) -> Result<(), QueryError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|_| QueryError::Config("无法创建本地认证目录。"))?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(parent).map_err(|_| QueryError::Config("无法创建本地认证目录。"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| QueryError::Config("无法创建本地凭据临时文件。"))?;
    temp.write_all(bytes)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|_| QueryError::Config("无法写入本地凭据。"))?;
    temp.persist(path)
        .map_err(|_| QueryError::Config("无法原子保存本地凭据。"))?;
    Ok(())
}
fn load_cache(path: &Path) -> Result<Option<TokenCache>, QueryError> {
    match fs::read(path) {
        Ok(bytes) => {
            let cache: TokenCache = serde_json::from_slice(&bytes)
                .map_err(|_| QueryError::Config("本地认证缓存格式错误。"))?;
            validate_token(&cache.access_token)?;
            Ok(Some(cache))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(QueryError::Config("无法读取本地认证缓存。")),
    }
}
fn save_cache(path: &Path, cache: &TokenCache) -> Result<(), QueryError> {
    secure_write(
        path,
        &serde_json::to_vec_pretty(cache)
            .map_err(|_| QueryError::Config("无法序列化认证缓存。"))?,
    )
}
fn lock(path: &Path) -> Result<fs::File, QueryError> {
    let path = path.with_extension("lock");
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|_| QueryError::Config("无法创建本地认证目录。"))?;
    // Open/create the same inode, rather than atomically replacing a contended lock file.
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|_| QueryError::Config("无法打开认证锁。"))?;
    file.lock_exclusive()
        .map_err(|_| QueryError::Config("无法锁定认证缓存。"))?;
    Ok(file)
}

fn credentials(config_path: &Path) -> Result<Credentials, QueryError> {
    // Re-read under the authentication lock: another process may have initialized
    // the device ID while this caller was waiting. Replacing it would lose trust.
    let mut credentials = Settings::load(config_path)?
        .cas
        .ok_or(QueryError::AuthFlow(
            "请在本机 config.yaml 配置 cas 账号密码。",
        ))?;
    credentials.validate()?;
    if let Some(id) = credentials.device_id.as_ref() {
        if id.len() < 32 || hex::decode(id).is_err() {
            return Err(QueryError::Config(
                "cas.device_id 必须为至少 32 位十六进制字符串。",
            ));
        }
    } else {
        let mut bytes = [0u8; 16];
        OsRng.fill_bytes(&mut bytes);
        let id = hex::encode(bytes);
        let text = fs::read_to_string(config_path)
            .map_err(|_| QueryError::Config("无法读取设备配置。"))?;
        let mut yaml: serde_yaml::Value = serde_yaml::from_str(&text)
            .map_err(|_| QueryError::Config("设备配置 YAML 格式错误。"))?;
        yaml["cas"]["device_id"] = serde_yaml::Value::String(id.clone());
        secure_write(
            config_path,
            serde_yaml::to_string(&yaml)
                .map_err(|_| QueryError::Config("无法保存设备配置。"))?
                .as_bytes(),
        )?;
        credentials.device_id = Some(id);
    }
    Ok(credentials)
}

fn cache_from_url(url: &Url) -> Option<TokenCache> {
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

fn cas_token(
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

fn sso_header(source: &str) -> Result<String, QueryError> {
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

fn refresh(client: &Client, old: &TokenCache) -> Result<TokenCache, QueryError> {
    let authorization = match old.client_authorization.as_ref() {
        Some(a) => a.clone(),
        None => discover_client(client, old)?,
    };
    refresh_request(client, old, old.provider.endpoint(), &authorization)
}

fn refresh_request(
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

/// Refresh at most once under a process-shared lock; background CAS never sends SMS.
pub fn token(
    settings: &Settings,
    path: &Path,
    timeout: Duration,
    rejected: Option<&str>,
) -> Result<TokenCache, QueryError> {
    token_with(
        settings,
        path,
        rejected,
        |cache| refresh(&crate::client(timeout)?, cache),
        || cas_token(settings, path, timeout, LoginOptions::default()),
    )
}

fn token_with(
    settings: &Settings,
    path: &Path,
    rejected: Option<&str>,
    refresh: impl FnOnce(&TokenCache) -> Result<TokenCache, QueryError>,
    login: impl FnOnce() -> Result<TokenCache, QueryError>,
) -> Result<TokenCache, QueryError> {
    let cache_path = settings.cache_path(path);
    let _lock = lock(&cache_path)?;
    let cache = load_cache(&cache_path)?;
    if let Some(cache) = cache
        .as_ref()
        .filter(|cache| cache.matches_account(settings))
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
    let settings = Settings::load(path)?;
    let cache_path = settings.cache_path(path);
    let _lock = lock(&cache_path)?;
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
    let settings = Settings::load(path)?;
    Ok(AuthStatus::from_cache(
        load_cache(&settings.cache_path(path))?
            .as_ref()
            .filter(|cache| cache.matches_account(&settings)),
    ))
}

pub fn renew(path: &Path, timeout: Duration) -> Result<AuthStatus, QueryError> {
    let settings = Settings::load(path)?;
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
    let settings = Settings::load(path)?;
    let cache_path = settings.cache_path(path);
    let _lock = lock(&cache_path)?;
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
    let settings = Settings::load(path)?;
    settings.aircon.selected()?;
    let _lock = lock(&settings.cache_path(path))?;
    let credentials = credentials(path)?;
    let client = cas::client(timeout)?;
    cas::login(&client, cas::AIRCON_ENTRY, &credentials, options)?;
    Ok((client, settings.aircon))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_accounts_and_unbound_legacy_caches_are_never_reused_or_refreshed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        secure_write(&path, b"cas:\n  username: new-account\n  password: ''\n").unwrap();
        let settings = Settings::load(&path).unwrap();
        let cache_path = settings.cache_path(&path);
        let mut cache = TokenCache::from_response(
            &serde_json::json!({"access_token":"old-access","refresh_token":"old-refresh","expires_in":3600}),
            OAuthProvider::Berserker,
            Utc::now().timestamp(),
        ).unwrap();
        for account in [Some("old-account"), None] {
            cache.cas_username = account.map(str::to_owned);
            save_cache(&cache_path, &cache).unwrap();
            let before = fs::read(&cache_path).unwrap();
            // Empty CAS password fails locally. If the old token were reused this
            // would succeed; refreshing it would attempt a network request.
            assert!(matches!(
                token(&settings, &path, Duration::from_secs(1), None),
                Err(QueryError::AuthFlow(_))
            ));
            assert!(!status(&path).unwrap().cached);
            assert_eq!(fs::read(&cache_path).unwrap(), before);
        }
        cache.cas_username = Some("new-account".into());
        save_cache(&cache_path, &cache).unwrap();
        assert_eq!(
            token(&settings, &path, Duration::from_secs(1), None)
                .unwrap()
                .access_token,
            "old-access"
        );
        assert!(status(&path).unwrap().cached);
    }

    #[test]
    fn imports_bind_to_the_configured_account_and_support_token_only_configs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let input = dir.path().join("response.json");
        secure_write(
            &input,
            br#"{"access_token":"imported-access","cas_username":"untrusted-input"}"#,
        )
        .unwrap();
        for (config, account) in [
            (
                "cas:\n  username: current-account\n  password: ''\n",
                Some("current-account"),
            ),
            ("cas: null\n", None),
            ("cas:\n  username: ''\n  password: ''\n", None),
        ] {
            secure_write(&path, config.as_bytes()).unwrap();
            import(&path, &input, OAuthProvider::Berserker, None).unwrap();
            let settings = Settings::load(&path).unwrap();
            let cache = load_cache(&settings.cache_path(&path)).unwrap().unwrap();
            assert_eq!(cache.cas_username.as_deref(), account);
            assert_eq!(
                token(&settings, &path, Duration::from_secs(1), None)
                    .unwrap()
                    .access_token,
                "imported-access"
            );
        }
    }

    #[test]
    fn rejected_refresh_grants_relogin_and_replace_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let settings = Settings::default();
        let old = TokenCache::from_response(
            &serde_json::json!({"access_token":"old-access","refresh_token":"old-refresh"}),
            OAuthProvider::Berserker,
            0,
        )
        .unwrap();
        save_cache(&settings.cache_path(&path), &old).unwrap();
        let mut refreshed = false;
        let mut logged_in = false;
        let new = token_with(
            &settings,
            &path,
            Some("old-access"),
            |_| {
                refreshed = true;
                Err(QueryError::Authentication)
            },
            || {
                logged_in = true;
                TokenCache::from_response(
                    &serde_json::json!({"access_token":"new-access","refresh_token":"new-refresh"}),
                    OAuthProvider::Berserker,
                    0,
                )
            },
        )
        .unwrap();
        assert!(refreshed && logged_in);
        assert_eq!(new.access_token, "new-access");
        assert_eq!(
            load_cache(&settings.cache_path(&path))
                .unwrap()
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("new-refresh")
        );
    }

    #[test]
    fn temporary_refresh_failures_remain_retryable() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        let cases = [
            ("408 Request Timeout", r#"{}"#),
            ("429 Too Many Requests", r#"{}"#),
            (
                "400 Bad Request",
                r#"{"error":"temporarily_unavailable","error_description":"must-not-be-logged"}"#,
            ),
            (
                "200 OK",
                r#"{"error":"temporarily_unavailable","error_description":"must-not-be-logged"}"#,
            ),
            ("200 OK", r#"{"code":500}"#),
            ("200 OK", "<html>service unavailable</html>"),
            ("200 OK", r#"{}"#),
        ];
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for (status, body) in cases {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|n| n.parse::<usize>().ok())
                            })
                            .unwrap();
                        if body.len() >= length {
                            break;
                        }
                    }
                }
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let old = TokenCache::from_response(
            &serde_json::json!({"access_token":"access","refresh_token":"refresh"}),
            OAuthProvider::Berserker,
            0,
        )
        .unwrap();
        let client = crate::client(Duration::from_secs(5)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let settings = Settings::default();
        let cache_path = settings.cache_path(&path);
        save_cache(&cache_path, &old).unwrap();
        let before = fs::read(&cache_path).unwrap();
        for (status, _) in cases {
            let error = token_with(
                &settings,
                &path,
                Some("access"),
                |cache| refresh_request(&client, cache, &endpoint, "Basic dGVzdDpwdWJsaWM="),
                || panic!("temporary refresh failure must not trigger CAS"),
            )
            .err()
            .unwrap();
            if status.starts_with("200") {
                assert!(matches!(error, QueryError::Response(_)), "{error:?}");
            } else {
                assert!(matches!(error, QueryError::Http(_)), "{error:?}");
            }
            assert!(!error.to_string().contains("must-not-be-logged"));
            assert_eq!(fs::read(&cache_path).unwrap(), before);
        }
        server.join().unwrap();
    }

    #[test]
    fn device_initialization_is_stable_and_preserves_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        secure_write(&path,b"cas:\n  username: test-account\n  password: 'test:password#value'\n  device_id: null\nextra: retained\n").unwrap();
        let first = credentials(&path).unwrap();
        let second = credentials(&path).unwrap();
        assert_eq!(first.device_id, second.device_id);
        assert_eq!(second.username, "test-account");
        assert_eq!(second.password, "test:password#value");
        let saved: serde_yaml::Value = serde_yaml::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["extra"].as_str(), Some("retained"));
    }

    #[test]
    fn sso_component_uses_the_authorization_after_the_sso_marker() {
        let script = r#"Authorization:"Basic ZGVjb3k6cHVibGlj", logintype:"sso", device_token:"h5"; url:"/berserker-auth/oauth/token", headers:{Authorization:"Basic dGVzdDpwdWJsaWM="}"#;
        assert_eq!(sso_header(script).unwrap(), "Basic dGVzdDpwdWJsaWM=");
        assert!(sso_header("unknown component").is_err());
    }

    #[test]
    fn refresh_http_grant_rotates_and_errors_preserve_the_old_cache() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/oauth/token", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for (status, body) in [
                (
                    "200 OK",
                    r#"{"access_token":"new-access","refresh_token":"rotated-refresh","expires_in":600}"#,
                ),
                (
                    "400 Bad Request",
                    r#"{"error":"invalid_grant","error_description":"must-not-be-logged"}"#,
                ),
                ("503 Service Unavailable", r#"{}"#),
                ("401 Unauthorized", r#"{}"#),
                ("403 Forbidden", r#"{}"#),
                ("200 OK", r#"{"error":"invalid_grant"}"#),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 1024];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|n| n.parse::<usize>().ok())
                            })
                            .unwrap();
                        if body.len() >= length {
                            let authorization = headers
                                .lines()
                                .filter_map(|line| line.split_once(':'))
                                .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                                .unwrap()
                                .1
                                .trim();
                            assert_eq!(authorization, "Basic dGVzdDpwdWJsaWM=");
                            assert!(body.contains("grant_type=refresh_token"));
                            assert!(body.contains("refresh_token=old%2Brefresh%26value"));
                            break;
                        }
                    }
                }
                write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let mut old = TokenCache::from_response(
            &serde_json::json!({"access_token":"old-access","refresh_token":"old+refresh&value"}),
            OAuthProvider::Berserker,
            0,
        )
        .unwrap();
        old.cas_username = Some("current-account".into());
        let client = crate::client(Duration::from_secs(5)).unwrap();
        let new = refresh_request(&client, &old, &endpoint, "Basic dGVzdDpwdWJsaWM=").unwrap();
        assert_eq!(new.access_token, "new-access");
        assert_eq!(new.refresh_token.as_deref(), Some("rotated-refresh"));
        assert_eq!(new.cas_username.as_deref(), Some("current-account"));
        let error = refresh_request(&client, &old, &endpoint, "Basic dGVzdDpwdWJsaWM=")
            .err()
            .unwrap();
        assert!(matches!(error, QueryError::Authentication));
        assert!(!error.to_string().contains("must-not-be-logged"));
        assert!(matches!(
            refresh_request(&client, &old, &endpoint, "Basic dGVzdDpwdWJsaWM="),
            Err(QueryError::Http(503))
        ));
        for _ in 0..3 {
            assert!(matches!(
                refresh_request(&client, &old, &endpoint, "Basic dGVzdDpwdWJsaWM="),
                Err(QueryError::Authentication)
            ));
        }
        assert_eq!(old.refresh_token.as_deref(), Some("old+refresh&value"));
        server.join().unwrap();
    }

    #[test]
    fn a_concurrent_new_token_is_used_without_refreshing_again() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        let settings = Settings::default();
        let mut current = TokenCache::from_response(
            &serde_json::json!({"access_token":"new-access","expires_in":3600}),
            OAuthProvider::Berserker,
            Utc::now().timestamp(),
        )
        .unwrap();
        current.refresh_token = Some("current-refresh".into());
        save_cache(&settings.cache_path(&config_path), &current).unwrap();
        let found = token(
            &settings,
            &config_path,
            Duration::from_secs(1),
            Some("old-access"),
        )
        .unwrap();
        assert_eq!(found.access_token, "new-access");
    }

    #[test]
    fn expired_token_without_credentials_requires_local_login_and_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        secure_write(&config_path, b"cas: null\n").unwrap();
        let settings = Settings::default();
        let old = TokenCache::from_response(
            &serde_json::json!({"access_token":"old-access","expires_in":1}),
            OAuthProvider::Berserker,
            0,
        )
        .unwrap();
        let path = settings.cache_path(&config_path);
        save_cache(&path, &old).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            token(&settings, &config_path, Duration::from_secs(1), None),
            Err(QueryError::AuthFlow(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    #[test]
    fn expiry_and_rotating_refresh_tokens() {
        let old=TokenCache::from_response(&serde_json::json!({"access_token":"access1","refresh_token":"refresh1","expires_in":"600"}),OAuthProvider::Berserker,100).unwrap();
        assert!(old.usable(100));
        assert!(!old.usable(400));
        let new=TokenCache::from_response(&serde_json::json!({"data":{"access_token":"access2","refresh_token":"refresh2","expires_in":900}}),old.provider,400).unwrap().merged_refresh(&old);
        assert_eq!(new.refresh_token.as_deref(), Some("refresh2"));
        let omitted = TokenCache::from_response(
            &serde_json::json!({"access_token":"access3"}),
            old.provider,
            100,
        )
        .unwrap()
        .merged_refresh(&old);
        assert_eq!(omitted.refresh_token.as_deref(), Some("refresh1"));
    }
    #[test]
    fn token_url_and_fragment_are_parsed_without_logging() {
        let url=Url::parse("https://mcard.sdu.edu.cn/charge-app/#access_token=example&refresh_token=refresh&expires_in=900").unwrap();
        let cache = cache_from_url(&url).unwrap();
        assert_eq!(cache.access_token, "example");
        assert!(cache.refresh_token.is_some());
    }
    #[test]
    fn oauth_error_does_not_become_a_token() {
        assert!(
            TokenCache::from_response(
                &serde_json::json!({"error":"invalid_grant","error_description":"secret-value"}),
                OAuthProvider::Berserker,
                0
            )
            .is_err()
        );
        assert!(validate_token("bad\r\nheader").is_err());
    }
    #[test]
    fn atomic_cache_has_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/auth.json");
        secure_write(&path, b"one").unwrap();
        secure_write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
