//! Monitoring configuration deliberately does not implement Debug: channels contain secrets.
use crate::QueryError;
use reqwest::{
    Url,
    header::{HeaderName, HeaderValue},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    str::FromStr,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonSettings {
    #[serde(deserialize_with = "decimal", serialize_with = "serialize_decimal")]
    pub threshold_kwh: Decimal,
    pub interval_seconds: u64,
    pub repeat_after_seconds: u64,
    pub query_timeout_seconds: u64,
    pub history: PathBuf,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            threshold_kwh: Decimal::from(10),
            interval_seconds: 21600,
            repeat_after_seconds: 86400,
            query_timeout_seconds: 20,
            history: PathBuf::from(".local/electricity/history.sqlite3"),
        }
    }
}

fn decimal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Decimal, D::Error> {
    let value = serde_yaml::Value::deserialize(deserializer)?;
    let text = match value {
        serde_yaml::Value::String(text) => text,
        serde_yaml::Value::Number(number) => number.to_string(),
        _ => return Err(serde::de::Error::custom("阈值必须是十进制数。")),
    };
    Decimal::from_str(&text).map_err(|_| serde::de::Error::custom("阈值必须是十进制数。"))
}

fn serialize_decimal<S: serde::Serializer>(
    value: &Decimal,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.normalize().to_string())
}

impl DaemonSettings {
    pub fn validate(&self) -> Result<(), QueryError> {
        if self.threshold_kwh < Decimal::ZERO {
            return Err(QueryError::Config("daemon.threshold_kwh 不得为负数。"));
        }
        if !(60..=31_536_000).contains(&self.interval_seconds)
            || !(60..=31_536_000).contains(&self.repeat_after_seconds)
        {
            return Err(QueryError::Config("监控周期必须为 60 至 31536000 秒。"));
        }
        validate_timeout(self.query_timeout_seconds)?;
        if self.history.as_os_str().is_empty() {
            return Err(QueryError::Config("daemon.history 不得为空。"));
        }
        Ok(())
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationSettings {
    pub channels: Vec<ChannelSettings>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ChannelSettings {
    Pushdeer(PushDeerSettings),
    Webhook(WebhookSettings),
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PushDeerSettings {
    pub id: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub pushkey: String,
    #[serde(default = "pushdeer_endpoint")]
    pub endpoint: String,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookSettings {
    pub id: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}

fn enabled() -> bool {
    true
}
fn timeout() -> u64 {
    10
}
fn pushdeer_endpoint() -> String {
    "https://api2.pushdeer.com".into()
}

pub fn validate_url(value: &str) -> Result<Url, QueryError> {
    let url = Url::parse(value).map_err(|_| QueryError::Config("通知地址不是有效 URL。"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(QueryError::Config(
            "通知地址必须是无内嵌凭据和片段的 HTTP/HTTPS URL。",
        ));
    }
    Ok(url)
}

fn validate_timeout(value: u64) -> Result<(), QueryError> {
    if !(1..=300).contains(&value) {
        return Err(QueryError::Config("网络超时必须为 1 至 300 秒。"));
    }
    Ok(())
}

impl ChannelSettings {
    pub fn id(&self) -> &str {
        match self {
            Self::Pushdeer(s) => &s.id,
            Self::Webhook(s) => &s.id,
        }
    }
    pub fn enabled(&self) -> bool {
        match self {
            Self::Pushdeer(s) => s.enabled,
            Self::Webhook(s) => s.enabled,
        }
    }
    pub fn timeout_seconds(&self) -> u64 {
        match self {
            Self::Pushdeer(s) => s.timeout_seconds,
            Self::Webhook(s) => s.timeout_seconds,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), QueryError> {
        // IDs enter status/log output; prohibit control characters and unbounded identifiers.
        if self.id().is_empty()
            || self.id().len() > 128
            || !self
                .id()
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(QueryError::Config(
                "通知渠道 id 仅允许字母、数字、点、横线和下划线，最多 128 字节。",
            ));
        }
        validate_timeout(self.timeout_seconds())?;
        match self {
            Self::Pushdeer(s) => {
                let url = validate_url(&s.endpoint)?;
                if url.query().is_some() {
                    return Err(QueryError::Config("PushDeer endpoint 不应包含查询参数。"));
                }
                if s.enabled && s.pushkey.trim().is_empty() {
                    return Err(QueryError::Config("启用 PushDeer 必须填写 pushkey。"));
                }
            }
            Self::Webhook(s) => {
                if s.enabled || !s.url.is_empty() {
                    validate_url(&s.url)?;
                }
                for (name, value) in &s.headers {
                    let name = HeaderName::from_str(name)
                        .map_err(|_| QueryError::Config("Webhook header 名称无效。"))?;
                    if matches!(name.as_str(), "content-type" | "host" | "content-length") {
                        return Err(QueryError::Config(
                            "Webhook 不允许覆盖 Content-Type、Host 或 Content-Length。",
                        ));
                    }
                    HeaderValue::from_str(value)
                        .map_err(|_| QueryError::Config("Webhook header 值无效。"))?;
                }
            }
        }
        Ok(())
    }
}

impl NotificationSettings {
    pub fn validate(&self) -> Result<(), QueryError> {
        let mut ids = BTreeSet::new();
        for channel in &self.channels {
            channel.validate()?;
            if !ids.insert(channel.id()) {
                return Err(QueryError::Config("通知渠道 id 不得重复。"));
            }
        }
        Ok(())
    }
    pub fn require_enabled(&self) -> Result<(), QueryError> {
        self.validate()?;
        if !self.channels.iter().any(ChannelSettings::enabled) {
            return Err(QueryError::Config("daemon 至少需要一个启用的通知渠道。"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn parse(text: &str) -> Result<Settings, String> {
        let settings: Settings = serde_yaml::from_str(text).map_err(|_| "格式无效".to_owned())?;
        settings.daemon.validate().map_err(|e| e.to_string())?;
        settings
            .notifications
            .validate()
            .map_err(|e| e.to_string())?;
        Ok(settings)
    }

    #[test]
    fn old_config_and_disabled_channels_remain_valid() {
        let settings = parse("{}\n").unwrap();
        assert_eq!(settings.daemon.threshold_kwh, Decimal::from(10));
        assert!(settings.notifications.require_enabled().is_err());
        let settings = parse("notifications:\n  channels:\n    - {type: pushdeer, id: p, enabled: false}\n    - {type: webhook, id: w, enabled: false}\n").unwrap();
        assert!(settings.notifications.require_enabled().is_err());
    }

    #[test]
    fn exact_thresholds_and_both_channel_types_are_accepted() {
        for threshold in ["\"10.001\"", "10.001", "0"] {
            let settings = parse(&format!("daemon:\n  threshold_kwh: {threshold}\nnotifications:\n  channels:\n    - {{type: pushdeer, id: p, pushkey: test}}\n    - {{type: webhook, id: w, url: 'https://example.invalid'}}\n")).unwrap();
            assert_eq!(
                settings.daemon.threshold_kwh,
                Decimal::from_str(threshold.trim_matches('"')).unwrap()
            );
            settings.notifications.require_enabled().unwrap();
        }
    }

    #[test]
    fn invalid_or_misspelled_configuration_is_rejected_without_echoing_secrets() {
        for text in [
            "daemon: {threshold_kwh: '-1'}",
            "daemon: {interval_seconds: 59}",
            "daemon: {repeat_after_seconds: 0}",
            "daemon: {query_timeout_seconds: 301}",
            "daemon: {interval_second: 3600}",
            "notifications: {channel: []}",
            "notifications: {channels: [{type: unknown, id: p}]}",
            "notifications: {channels: [{type: pushdeer, id: p, pushkey: secret, typo: 1}]}",
            "notifications: {channels: [{type: pushdeer, id: p}]}",
            "notifications: {channels: [{type: webhook, id: w, url: 'ftp://secret.invalid'}]}",
            "notifications: {channels: [{type: webhook, id: w, url: 'https://user:secret@example.invalid'}]}",
            "notifications: {channels: [{type: webhook, id: w, url: 'https://example.invalid', headers: {Content-Type: secret}}]}",
            "notifications: {channels: [{type: pushdeer, id: p, pushkey: secret}, {type: webhook, id: p, enabled: false}]}",
        ] {
            let error = parse(text)
                .err()
                .expect("must reject invalid configuration");
            assert!(!error.contains("secret"));
        }
    }
}
