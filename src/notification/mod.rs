//! Channel-neutral alerts. HTTP errors never include credentials, URLs or response bodies.
use crate::{Event, Location, settings::monitoring::ChannelSettings};
use chrono::Utc;
use rand::{RngCore, rngs::OsRng};
use reqwest::{
    blocking::{Client, Response},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

mod pushdeer;
mod webhook;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Alert {
    pub schema_version: u32,
    pub event: String,
    pub event_id: String,
    pub checked_at: String,
    pub title: String,
    pub message: String,
    pub location: Option<Location>,
    pub remaining_kwh: Option<String>,
    pub threshold_kwh: Option<String>,
}

impl Alert {
    fn new(event: &str, title: &str, message: String) -> Self {
        let mut bytes = [0u8; 16];
        OsRng.fill_bytes(&mut bytes);
        Self {
            schema_version: 1,
            event: event.into(),
            event_id: hex::encode(bytes),
            checked_at: Utc::now().to_rfc3339(),
            title: title.into(),
            message,
            location: None,
            remaining_kwh: None,
            threshold_kwh: None,
        }
    }
    pub fn low_balance(event: &Event) -> Self {
        let mut alert = Self::new(
            "electricity.low_balance",
            "宿舍电量不足",
            format!(
                "剩余 {} 度，低于阈值 {} 度。",
                event.remaining_kwh.as_deref().unwrap_or("未知"),
                event.threshold_kwh.as_deref().unwrap_or("未知"),
            ),
        );
        alert.checked_at.clone_from(&event.checked_at);
        alert.location = event.location.clone();
        alert.remaining_kwh.clone_from(&event.remaining_kwh);
        alert.threshold_kwh.clone_from(&event.threshold_kwh);
        alert
    }
    pub fn auth_required(location: Option<Location>) -> Self {
        let mut alert = Self::new(
            "electricity.auth_required",
            "电费监控需要登录",
            "请在本机运行 auth login --trust-device 完成登录；监控将在下一检查周期尝试恢复。"
                .into(),
        );
        alert.location = location;
        alert
    }
    pub fn test() -> Self {
        Self::new(
            "electricity.test",
            "SDU-InfoHelper 推送测试",
            "通知渠道测试消息。".into(),
        )
    }
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct NotifyError {
    pub message: String,
    pub retryable: bool,
    pub retry_after_seconds: Option<u64>,
}

impl NotifyError {
    pub(crate) fn new(message: &str, retryable: bool) -> Self {
        Self {
            message: message.into(),
            retryable,
            retry_after_seconds: None,
        }
    }
    pub(crate) fn network(error: reqwest::Error) -> Self {
        Self::new(
            if error.is_timeout() {
                "通知请求超时。"
            } else {
                "通知请求失败。"
            },
            true,
        )
    }
}

#[derive(Debug)]
pub struct DeliveryReceipt;

pub trait Notifier {
    fn send(&self, alert: &Alert) -> Result<DeliveryReceipt, NotifyError>;
}

pub struct HttpNotifier {
    config: ChannelSettings,
    client: Client,
}

impl HttpNotifier {
    pub fn new(config: &ChannelSettings) -> Result<Self, NotifyError> {
        config
            .validate()
            .map_err(|_| NotifyError::new("通知渠道配置无效。", false))?;
        if !config.enabled() {
            return Err(NotifyError::new("通知渠道已禁用。", false));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds()))
            .connect_timeout(Duration::from_secs(config.timeout_seconds()))
            .redirect(Policy::none())
            .build()
            .map_err(|_| NotifyError::new("无法创建通知客户端。", false))?;
        Ok(Self {
            config: config.clone(),
            client,
        })
    }
    pub fn id(&self) -> &str {
        self.config.id()
    }
}

impl Notifier for HttpNotifier {
    fn send(&self, alert: &Alert) -> Result<DeliveryReceipt, NotifyError> {
        match &self.config {
            ChannelSettings::Pushdeer(config) => pushdeer::send(&self.client, config, alert),
            ChannelSettings::Webhook(config) => webhook::send(&self.client, config, alert),
        }
    }
}

pub(crate) fn check_response(response: &Response) -> Result<(), NotifyError> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let code = status.as_u16();
    let mut error = NotifyError::new(
        &format!("通知服务返回 HTTP {code}。"),
        code == 408 || code == 429 || status.is_server_error(),
    );
    if code == 429 {
        error.retry_after_seconds = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|value| {
                value.parse::<u64>().ok().or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(value)
                        .ok()
                        .map(|date| {
                            date.timestamp()
                                .saturating_sub(Utc::now().timestamp())
                                .max(0) as u64
                        })
                })
            });
    }
    Err(error)
}
