//! OAuth token data, metadata parsing and account binding.
use crate::QueryError;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Default, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    #[default]
    Berserker,
    Blade,
}
impl OAuthProvider {
    pub(super) fn endpoint(self) -> &'static str {
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

pub(super) fn claim(token: &str, key: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&decoded)
        .ok()?
        .get(key)
        .cloned()
}
pub(super) fn nonempty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}
pub(super) fn number(value: Option<&Value>) -> Option<i64> {
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
    pub(super) fn matches_account(&self, username: Option<&str>) -> bool {
        username.is_none_or(|username| self.cas_username.as_deref() == Some(username))
    }
    pub(super) fn usable(&self, now: i64) -> bool {
        self.expires_at
            .is_none_or(|exp| exp > now.saturating_add(300))
    }
    pub(super) fn merged_refresh(mut self, old: &Self) -> Self {
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
pub(super) fn validate_token(token: &str) -> Result<(), QueryError> {
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
